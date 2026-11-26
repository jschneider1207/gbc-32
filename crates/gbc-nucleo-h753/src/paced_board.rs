//! Pieces of the audio-paced images (`paced`, `player`; moved from
//! src/bin/paced.rs): the DWT as the loop's [`CycleClock`], the CPU clock
//! kept running in Sleep, and the captured run lines with their framed
//! report.

use core::cell::Cell;
use core::fmt::{self, Write};

use gbc_firmware::CycleClock;

use crate::{Board, CPU_HZ, Uart, audio_dac, cycles, image, pac};

/// The DWT cycle counter as a [`CycleClock`], extended to 64 bits on every
/// reading ([`Dwt::total`]; readings must be less than a wrap, 9.1 s,
/// apart: `paced::run` reads it at least once per call and per idle).
pub struct Dwt {
    last: Cell<u32>,
    total: Cell<u64>,
}

impl Default for Dwt {
    fn default() -> Self {
        Self::new()
    }
}

impl Dwt {
    pub fn new() -> Self {
        Dwt {
            last: Cell::new(cycles()),
            total: Cell::new(0),
        }
    }

    /// Cycles since [`Dwt::new`], as of this call.
    pub fn total(&self) -> u64 {
        self.now();
        self.total.get()
    }
}

impl CycleClock for Dwt {
    fn now(&self) -> u32 {
        let c = cycles();
        let d = c.wrapping_sub(self.last.replace(c));
        self.total.set(self.total.get() + u64::from(d));
        c
    }

    fn hz(&self) -> u32 {
        CPU_HZ
    }
}

/// DBGMCU_CR (RM0433 Rev 9 61.5.8), bit 0 DBGSLEEP_D1: 1 keeps the CPU
/// clock running in Sleep (CSleep).
const DBGSLEEP_D1: u32 = 1 << 0;

fn dbgmcu() -> &'static pac::dbgmcu::RegisterBlock {
    // SAFETY: DBGMCU_CR is written only by `set_sleep_clock`, which the
    // images call from `main` only; the banner only reads IDC.
    unsafe { &*pac::DBGMCU::ptr() }
}

/// DBGMCU_CR as it is now.
pub fn dbgmcu_cr() -> u32 {
    dbgmcu().cr.read().bits()
}

/// DBGSLEEP_D1 set (`keep`: the cycle counter keeps counting in `WFI`, as
/// the paced loop's speed figures need) or clear.
pub fn set_sleep_clock(keep: bool) {
    let cr = dbgmcu().cr.read().bits();
    let cr = if keep {
        cr | DBGSLEEP_D1
    } else {
        cr & !DBGSLEEP_D1
    };
    // SAFETY: only bit 0 changes; the other bits are written back as read.
    dbgmcu().cr.write(|w| unsafe { w.bits(cr) });
    let _ = dbgmcu().cr.read();
}

/// A run's lines, kept for the reports, and also sent to the UART as
/// they are written (`live`). Writing past the end fails (and so ends the
/// run with a write error) and sets `overflow`.
pub struct Capture<'u, const N: usize> {
    pub buf: &'static mut [u8; N],
    pub len: usize,
    pub overflow: bool,
    pub live: Option<&'u mut Uart>,
}

impl<const N: usize> Write for Capture<'_, N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(u) = self.live.as_deref_mut() {
            // `\n` line ends become `\r\n` on the wire, as every line of
            // the board; the capture keeps `\n`.
            for (i, part) in s.split('\n').enumerate() {
                if i > 0 {
                    let _ = u.write_str("\r\n");
                }
                let _ = u.write_str(part);
            }
        }
        let Some(dst) = self.buf.get_mut(self.len..self.len + s.len()) else {
            self.overflow = true;
            return Err(fmt::Error);
        };
        dst.copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}

/// One framed report of the captured lines (the `paced` image's format,
/// which `run-paced.sh` parses):
///
/// ```text
/// <banner>
/// <prefix> paced report n=<k> rom=<name> frames=<N> image_bytes=<n> image_cksum=<crc>
/// <the captured lines>
/// <prefix> paced report-end n=<k> rom=<name> bytes=<len> cksum=<crc> overflow=<0|1>
/// ```
#[allow(clippy::too_many_arguments)]
pub fn report(
    board: &mut Board,
    prefix: &str,
    image_name: &str,
    rom_name: &str,
    n: u32,
    frames: u32,
    cap: &[u8],
    overflow: bool,
) {
    board.banner(image_name);
    let (image_bytes, image_cksum) = board.image();
    let out = &mut board.uart;
    // The UART writer never fails.
    let _ = writeln!(
        out,
        "{prefix} paced report n={n} rom={rom_name} frames={frames} \
         image_bytes={image_bytes} image_cksum={image_cksum}\r"
    );
    for line in cap.split_inclusive(|&b| b == b'\n') {
        let (body, nl) = match line.split_last() {
            Some((b'\n', body)) => (body, true),
            _ => (line, false),
        };
        // The capture holds only what `fmt::Write` wrote: UTF-8.
        let _ = out.write_str(core::str::from_utf8(body).unwrap_or("<not utf-8>"));
        if nl {
            let _ = out.write_str("\r\n");
        }
    }
    let _ = writeln!(
        out,
        "{prefix} paced report-end n={n} rom={rom_name} bytes={} cksum={} overflow={}\r",
        cap.len(),
        image::cksum(cap),
        u8::from(overflow)
    );
}

/// The `stats dac` line (the `paced` image's format), after `dac.stop()`
/// returned `stopped`.
pub fn dac_line(out: &mut dyn Write, stopped: bool) -> fmt::Result {
    let s = crate::audio_dac::stats();
    writeln!(
        out,
        "stats dac refills={} late={} spurious={} te={} dme={} fe={} udr1={} udr2={} \
         restarts={} isr_min={} isr_max={} stop={}",
        s.refills,
        s.late,
        s.spurious,
        s.te,
        s.dme,
        s.fe,
        s.udr1,
        s.udr2,
        s.restarts,
        s.isr_min,
        s.isr_max,
        if stopped { "ok" } else { "timeout" }
    )
}

/// The refill that completes the start ramp: `AudioDac::start` fills both
/// halves with its first [`audio_dac::FRAMES`] frames and each refill adds
/// [`audio_dac::HALF`], so the ramp's [`audio_dac::RAMP_FRAMES`] are done
/// at refill 30, and refill 31 is the first that pops the ring.
pub const RAMP_REFILLS: u32 =
    (audio_dac::RAMP_FRAMES - audio_dac::FRAMES as u32) / audio_dac::HALF as u32;
const _: () = assert!(RAMP_REFILLS == 30);
/// The run starts right after the refill this many before the ramp's
/// last: five halves (39 ms) before the first pop. The loop needs about
/// 22 ms to fill the ring (the `paced v1` line, 9 ms on the UART, then
/// four calls of at most 7.1 ms, `synth:worst-case`); with two refills
/// (23.4 ms) a 1,200-frame run's `min_fill` was 793, against 1,243 in
/// steady state.
pub const RAMP_LEFT: u32 = 4;

/// Starts the DAC and waits until its start ramp is `left` refills from its
/// end (at most 1 s): the paced loop starts then, so its speed figures do
/// not count the ramp ([`RAMP_LEFT`] for a measured run). The loop must
/// fill the ring within `left` refills (7.8 ms each), or the first pops
/// underrun. Returns the cycles waited.
pub fn start_dac(dac: &mut crate::audio_dac::AudioDac, left: u32) -> u32 {
    let ramp0 = audio_dac::stats().refills;
    let t0 = cycles();
    dac.start();
    while audio_dac::stats().refills.wrapping_sub(ramp0) < RAMP_REFILLS.saturating_sub(left)
        && cycles().wrapping_sub(t0) < CPU_HZ
    {}
    cycles().wrapping_sub(t0)
}
