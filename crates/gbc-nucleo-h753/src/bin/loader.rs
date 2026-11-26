//! The `loader` image (plan Stage 2 M2 "Loader", ADR 0038): receives a ROM
//! from `tools/gbc-pc` over the ST-LINK VCP and stores it in the shield's
//! QSPI NOR in the ADR 0037 image format. The protocol logic is
//! [`gbc_nucleo_h753::loader`]; this image wires it to the UART and the
//! QUADSPI driver.
//!
//! At reset: the banner at 115200, then a `boot` line with the stored
//! image as `rom_image::validate` sees it through the memory-mapped
//! window. Then the UART switches to [`LOADER_BAUD`] (the FIFOs on) and
//! the image serves the protocol.
//!
//! While the line is idle (3 s), it prints a status report every 10 s at
//! [`LOADER_BAUD`]: the banner again, then a `status` line (the stored
//! image's header now, the one validated at boot and how long that
//! took, the load in progress, the loader's counters, the UART
//! receive errors, `qspi::exit_fallbacks()`, and the longest time spent
//! answering each request type). Every text report ends with a 0x00, so
//! the PC's frame decoder drops it as one bad frame and resyncs before
//! the next reply. Bytes that arrive while a reply or report is being
//! sent are kept in a ring buffer (the transmitter is polled, not waited
//! on), so a report never causes an overrun.
//!
//! The rate is a build-time constant: `GBC_LOADER_BAUD=<bps>` when
//! building, else [`DEFAULT_BAUD`] (measured, `docs/reference/loader.md`).
//! gbc-link v1 has no baud switch, so the PC must be told the same rate
//! (`gbc-pc --baud`).
#![no_std]
#![no_main]

use core::fmt::{self, Write};

use gbc_firmware::rom_image::{ImageError, ImageHeader};
use gbc_link::ty;
use gbc_nucleo_h753::loader::{Loader, Nor, Progress};
use gbc_nucleo_h753::qspi::{self, NOR_BYTES, NorError, QUAD_IO_READ, Qspi};
use gbc_nucleo_h753::uart::{self, Uart};
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, led};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "loader";

/// The serving rate without `GBC_LOADER_BAUD`: 2 Mbit/s, 16x with
/// USARTDIV 59, exact at 118 MHz, so the receivers' whole tolerance
/// (3.33%) is left for the ST-LINK, whose own rate error is not known.
/// 4 Mbit/s cannot be exact (29.5 clocks per bit: -1.67% at best) and was
/// not chosen although its loads were clean (`docs/reference/loader.md`
/// "Baud rate").
const DEFAULT_BAUD: u32 = 2_000_000;

/// The rate the protocol is served at.
const LOADER_BAUD: u32 = parse_baud(option_env!("GBC_LOADER_BAUD"), DEFAULT_BAUD);

/// A decimal rate from the build environment (a bad value fails the
/// build).
const fn parse_baud(s: Option<&str>, default: u32) -> u32 {
    let Some(s) = s else { return default };
    let b = s.as_bytes();
    assert!(!b.is_empty(), "GBC_LOADER_BAUD is empty");
    let mut v: u32 = 0;
    let mut i = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_digit(), "GBC_LOADER_BAUD is not a number");
        v = v * 10 + (b[i] - b'0') as u32;
        i += 1;
    }
    v
}

/// The NOR behind the loader's trait: erase and program in indirect mode
/// at [`qspi::PRESCALER_SAFE`], reads through the window at
/// [`qspi::PRESCALER_MMAP`] with 0xEB (as `qspi-mmap` and `qspi-write`).
struct BoardNor {
    /// Always `Some` outside `with_window` (which takes it for
    /// `memory_mapped` and puts it back).
    q: Option<Qspi>,
}

impl BoardNor {
    fn q(&mut self) -> &mut Qspi {
        self.q
            .as_mut()
            .expect("the QUADSPI is handed back after every window")
    }
}

impl Nor for BoardNor {
    type Error = NorError;

    fn size(&self) -> u32 {
        NOR_BYTES
    }

    fn erase_4k(&mut self, addr: u32) -> Result<(), NorError> {
        self.q().erase_4k(addr)
    }

    fn erase_64k(&mut self, addr: u32) -> Result<(), NorError> {
        self.q().erase_64k(addr)
    }

    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
        self.q().program(addr, data)
    }

    fn with_window<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Result<R, NorError> {
        let mut q = self.q.take().expect("the QUADSPI is here");
        if let Err(e) = q.set_prescaler(qspi::PRESCALER_MMAP) {
            self.q = Some(q);
            return Err(e.into());
        }
        let (mut q, r) = match q.memory_mapped(&QUAD_IO_READ) {
            Ok(m) => {
                let r = f(m.bytes());
                (m.exit(), Ok(r))
            }
            Err((q, e)) => (q, Err(NorError::from(e))),
        };
        let back = q.set_prescaler(qspi::PRESCALER_SAFE);
        self.q = Some(q);
        back?;
        r
    }
}

/// Received bytes held while the transmitter is busy.
struct Ring {
    buf: [u8; 4096],
    head: usize,
    len: usize,
    /// Bytes dropped because the ring was full (never expected: the PC
    /// sends one frame at a time).
    dropped: u32,
    /// The most bytes ever held.
    high: usize,
}

impl Ring {
    const fn new() -> Self {
        Ring {
            buf: [0; 4096],
            head: 0,
            len: 0,
            dropped: 0,
            high: 0,
        }
    }
    fn push(&mut self, b: u8) {
        if self.len == self.buf.len() {
            self.dropped += 1;
            return;
        }
        let i = (self.head + self.len) % self.buf.len();
        self.buf[i] = b;
        self.len += 1;
        self.high = self.high.max(self.len);
    }
    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let b = self.buf[self.head];
        self.head = (self.head + 1) % self.buf.len();
        self.len -= 1;
        Some(b)
    }
}

/// Sends `data`, moving received bytes into `ring` while the transmitter
/// is full. Bounded: a byte the transmitter does not take within ~10 ms is
/// dropped (as `Uart::write_byte` does).
fn send(uart: &mut Uart, ring: &mut Ring, data: &[u8]) {
    for &b in data {
        let t0 = cycles();
        while !uart.try_write(b) {
            if let Some(r) = uart.try_read() {
                ring.push(r);
            }
            if cycles().wrapping_sub(t0) > CPU_HZ / 100 {
                break;
            }
        }
    }
    if let Some(r) = uart.try_read() {
        ring.push(r);
    }
}

/// A text report built in memory, then sent with [`send`].
struct Text {
    buf: [u8; 768],
    len: usize,
}

impl Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// `cycles` as milliseconds with one decimal.
struct Ms(u32);
impl fmt::Display for Ms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let us = u64::from(self.0) * 1_000_000 / u64::from(CPU_HZ);
        write!(f, "{}.{}ms", us / 1000, us % 1000 / 100)
    }
}

/// The stored image, for the reports.
#[derive(Clone, Copy)]
struct Stored(Result<Result<ImageHeader, ImageError>, NorError>);
impl fmt::Display for Stored {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Ok(Ok(h)) => write!(f, "valid len={} hash={:#018x}", h.len, h.hash),
            Ok(Err(ImageError::Erased)) => write!(f, "none(Erased)"),
            Ok(Err(e)) => write!(f, "bad({e:?})"),
            Err(e) => write!(f, "error({e:?})"),
        }
    }
}

/// The longest answer per request type, in cycles.
#[derive(Default)]
struct Slowest {
    hello: u32,
    begin: u32,
    chunk: u32,
    end: u32,
    other: u32,
}

impl Slowest {
    fn note(&mut self, request: Option<u8>, c: u32) {
        let slot = match request {
            Some(ty::HELLO) => &mut self.hello,
            Some(ty::ROM_BEGIN) => &mut self.begin,
            Some(ty::ROM_CHUNK) => &mut self.chunk,
            Some(ty::ROM_END) => &mut self.end,
            _ => &mut self.other,
        };
        *slot = (*slot).max(c);
    }
}

fn status(
    t: &mut Text,
    loader: &mut Loader<BoardNor>,
    uart: &Uart,
    ring: &Ring,
    slow: &Slowest,
    boot: (Stored, u32),
) -> fmt::Result {
    let stored = Stored(loader.stored_header());
    let c = loader.counters;
    let e = uart.rx_errors();
    write!(
        t,
        "{PREFIX} {IMAGE} status image={stored} boot_image={} boot_validate={} state=",
        boot.0,
        Ms(boot.1)
    )?;
    match loader.progress() {
        Progress::Idle => write!(t, "idle")?,
        Progress::Loading { received, len } => write!(t, "loading({received}/{len})")?,
    }
    write!(
        t,
        " frames={} bad_frames={} duplicates={} hellos={} begins={} already_loaded={} \
         chunks={} ends={} loads_ok={} verify_failures={} naks={} erases_64k={} erases_4k={} \
         flash_errors={} last_flash_error={:?}",
        c.frames,
        c.bad_frames,
        c.duplicates,
        c.hellos,
        c.begins,
        c.already_loaded,
        c.chunks,
        c.ends,
        c.loads_ok,
        c.verify_failures,
        c.naks,
        c.erases_64k,
        c.erases_4k,
        c.flash_errors,
        loader.last_flash_error,
    )?;
    writeln!(
        t,
        " overruns={} framing={} noise={} ring_high={} ring_dropped={} exit_fallbacks={} \
         slowest hello={} begin={} chunk={} end={} other={} baud={}\r",
        e.overruns,
        e.framing,
        e.noise,
        ring.high,
        ring.dropped,
        qspi::exit_fallbacks(),
        Ms(slow.hello),
        Ms(slow.begin),
        Ms(slow.chunk),
        Ms(slow.end),
        Ms(slow.other),
        uart::baud(),
    )
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    board.banner(IMAGE);
    let q = Qspi::init(qspi::PRESCALER_SAFE);
    let mut loader = Loader::new(BoardNor { q: Some(q) });
    let t0 = cycles();
    let stored = Stored(loader.validate_stored());
    let dt = cycles().wrapping_sub(t0);
    let _ = writeln!(
        board.uart,
        "{PREFIX} {IMAGE} boot image={stored} validate_time={} exit_fallbacks={}\r",
        Ms(dt),
        qspi::exit_fallbacks()
    );
    let _ = writeln!(
        board.uart,
        "{PREFIX} {IMAGE} serving gbc-link v{} at baud={LOADER_BAUD} max_chunk={} flash_len={NOR_BYTES}\r",
        gbc_link::PROTOCOL_VERSION,
        gbc_nucleo_h753::loader::MAX_CHUNK
    );
    // `set_baud` even at 115200: it turns the 16-byte FIFOs on.
    if let Err(e) = board.uart.set_baud(LOADER_BAUD) {
        panic!("loader baud {LOADER_BAUD} refused: {e:?}");
    }
    let mut ring = Ring::new();
    send(&mut board.uart, &mut ring, &[0]);

    let mut slow = Slowest::default();
    // Time in 10 ms ticks (each `read_timeout` that times out), so the
    // DWT counter's 9.1 s wrap does not matter.
    let (mut idle_ticks, mut since_report) = (0u32, 0u32);
    loop {
        let b = match ring.pop() {
            Some(b) => Some(b),
            None => board.uart.read_timeout(CPU_HZ / 100),
        };
        match b {
            Some(b) => {
                idle_ticks = 0;
                let t0 = cycles();
                if let Some(r) = loader.feed(b) {
                    slow.note(r.request, cycles().wrapping_sub(t0));
                    send(&mut board.uart, &mut ring, r.wire);
                    if r.request == Some(ty::ROM_CHUNK) {
                        led::toggle();
                    }
                }
            }
            None => {
                idle_ticks = idle_ticks.saturating_add(1);
                since_report = since_report.saturating_add(1);
            }
        }
        // Idle for 3 s and 10 s since the last report: report.
        if idle_ticks >= 300 && since_report >= 1000 {
            since_report = 0;
            board.banner(IMAGE);
            let mut t = Text {
                buf: [0; 768],
                len: 0,
            };
            let _ = status(&mut t, &mut loader, &board.uart, &ring, &slow, (stored, dt));
            let len = t.len;
            send(&mut board.uart, &mut ring, &t.buf[..len]);
            send(&mut board.uart, &mut ring, &[0]);
        }
    }
}
