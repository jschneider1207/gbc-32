//! The `qspi-write` image (plan Stage 2 M2): the NOR write side
//! (`qspi::Qspi::{erase_64k, erase_4k, program_page, program}`) and UART
//! receive, on the board. It erases and programs **only the last 64 KiB
//! block** of the NOR, [`SCRATCH`] = 0x7F_0000..0x80_0000: every write
//! goes through [`Scratch`], which panics on any address outside it. The
//! rest of the NOR is never written.
//!
//! Once after reset (each run costs the block 2 erase cycles of its
//! 100,000):
//!
//! 1. `args`: the driver's argument checks (misaligned erases, a page
//!    program crossing a page, a program past the end), all inside the
//!    block, before anything is erased;
//! 2. `erase64`: erase the block, then check it is all 0xFF (indirect
//!    0x03);
//! 3. `program`: one aligned full page with `program_page`, then the rest
//!    of the block in pieces of 1 to 1024 bytes (lengths from a fixed-seed
//!    LCG, so starts and ends are unaligned and most pieces span page
//!    boundaries) with `program`; the data is a fixed-seed LCG stream
//!    ([`Pattern`]);
//! 4. `verify`: the block read back with indirect 0x03 (39 MHz), indirect
//!    0xEB (99.2 MHz) and through the memory-mapped window, against the
//!    pattern; then unaligned window reads (byte, u16 and u32
//!    `from_le_bytes`, `read_unaligned`, 16-byte copies, cold and warm)
//!    around page and line boundaries against an 0x03 reference;
//! 5. `erase4`: erase sector 5 of the block; it must read 0xFF and the
//!    other 15 sectors still the pattern (0x03 and window);
//! 6. `final`: erase the whole block again and check it is 0xFF (0x03
//!    and window).
//!
//! Each step prints its DWT time next to the datasheet's typical and
//! maximum, and `PASS` or `FAIL`; then `result PASS|FAIL`.
//!
//! Then the UART echo: every byte received is sent back. `~` followed by a
//! digit switches the baud rate to [`RATES`]`[digit]` (announced at the
//! old rate first); `~!` stops reading for 50 ms (bytes sent meanwhile
//! overrun the receiver: the overrun count must rise). While the line is
//! idle (3 s), the whole report is
//! printed again every 10 s, with the receive counters.
#![no_std]
#![no_main]

use core::fmt::{self, Write};
use core::hint::black_box;

use gbc_nucleo_h753::qspi::{
    self, BLOCK_BYTES, NOR_BYTES, NorError, NorFlash, Op, PAGE_BYTES, QUAD_IO_READ,
    QUAD_IO_READ_XIP, Qspi, Read, SECTOR_BYTES,
};
use gbc_nucleo_h753::uart::{self, baud_setting};
use gbc_nucleo_h753::{self as board, CPU_HZ, Uart, cycles, led};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "qspi-write";
/// The scratch block: the NOR's last 64 KiB, the only one written.
const SCRATCH: u32 = NOR_BYTES - BLOCK_BYTES;
const _: () = assert!(SCRATCH == 0x7F_0000 && SCRATCH + BLOCK_BYTES == 0x80_0000);
/// The sector of the block that step 5 erases (0x7F_5000).
const SECTOR: u32 = 5;
const _: () = assert!(SECTOR > 0 && SECTOR < BLOCK_BYTES / SECTOR_BYTES - 1);
/// Read chunk for the checks.
const CHUNK: usize = 4096;
/// The rates `~0`..`~9` select.
const RATES: [u32; 10] = [
    115_200, 230_400, 460_800, 921_600, 1_000_000, 2_000_000, 3_000_000, 4_000_000, 6_000_000,
    8_000_000,
];
/// Extra rates in the baud table.
const TABLE_EXTRA: [u32; 1] = [12_000_000];

const READ_03: Read = Read::single(0x03);

/// A fixed-seed LCG (Numerical Recipes constants).
struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }
}

/// The block's contents after step 3, as a stream from its first byte:
/// byte k is byte k % 4 (little-endian) of the LCG state after k / 4 + 1
/// steps from seed [`Pattern::SEED`].
struct Pattern {
    lcg: Lcg,
    word: u32,
    pos: u32,
}

impl Pattern {
    const SEED: u32 = 0x5EED_0B10;
    fn new() -> Self {
        Pattern {
            lcg: Lcg(Self::SEED),
            word: 0,
            pos: 0,
        }
    }
    fn next(&mut self) -> u8 {
        if self.pos.is_multiple_of(4) {
            self.word = self.lcg.next();
        }
        let b = (self.word >> (8 * (self.pos % 4))) as u8;
        self.pos += 1;
        b
    }
    fn fill(&mut self, buf: &mut [u8]) {
        buf.iter_mut().for_each(|b| *b = self.next());
    }
}

/// The only way this image writes the NOR: each call checks that the
/// whole range is inside [`SCRATCH`] and panics otherwise, before the
/// driver sees it.
struct Scratch<'a>(&'a mut Qspi);

fn assert_scratch(addr: u32, len: u32) {
    assert!(
        addr >= SCRATCH && u64::from(addr) + u64::from(len) <= u64::from(NOR_BYTES),
        "write outside the scratch block: {addr:#x}+{len}"
    );
}

impl Scratch<'_> {
    fn erase_64k(&mut self, addr: u32) -> Result<(), NorError> {
        assert_scratch(addr - addr % BLOCK_BYTES, BLOCK_BYTES);
        self.0.erase_64k(addr)
    }
    fn erase_4k(&mut self, addr: u32) -> Result<(), NorError> {
        assert_scratch(addr - addr % SECTOR_BYTES, SECTOR_BYTES);
        self.0.erase_4k(addr)
    }
    fn program_page(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
        // The page `addr` is in: a page program never leaves it (data past
        // the page end wraps to its start, MX25L6433F 10-19), so even a
        // too-long one stays inside a scratch page.
        assert_scratch(addr - addr % PAGE_BYTES, PAGE_BYTES);
        self.0.program_page(addr, data)
    }
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
        assert_scratch(addr, data.len() as u32);
        self.0.program(addr, data)
    }
}

/// The report: everything printed goes to the UART and into a buffer, so
/// the echo loop can print it again.
struct Log<'a> {
    out: &'a mut Uart,
    buf: &'a mut [u8],
    len: usize,
}

impl Write for Log<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let _ = self.out.write_str(s);
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// `cycles` as milliseconds with three decimals.
struct Ms(u32);
impl fmt::Display for Ms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let us = u64::from(self.0) * 1_000_000 / u64::from(CPU_HZ);
        write!(f, "{}.{:03}ms", us / 1000, us % 1000)
    }
}

/// `us` microseconds as milliseconds.
struct UsMs(u32);
impl fmt::Display for UsMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}ms", self.0 / 1000, self.0 % 1000)
    }
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

/// What a byte of the block should be.
#[derive(Clone, Copy)]
enum Expect {
    Erased,
    Pattern,
    /// The pattern, except sector [`SECTOR`] erased.
    PatternButSector,
}

/// How the block is read.
#[derive(Clone, Copy, PartialEq)]
enum Via {
    Read03,
    ReadEb,
    Window,
}

/// Mismatch count and first bad address of one check.
#[derive(Default)]
struct Diff {
    bytes: u32,
    bad: u32,
    first_bad: Option<u32>,
    errors: u32,
}

impl Diff {
    fn ok(&self) -> bool {
        self.bad == 0 && self.errors == 0 && self.bytes == BLOCK_BYTES
    }
    fn print(&self, log: &mut Log, step: &str, via: &str) {
        let _ = write!(
            log,
            "{PREFIX} {IMAGE} {step} via={via} bytes={} mismatches={} errors={} first_bad=",
            self.bytes, self.bad, self.errors
        );
        let _ = match self.first_bad {
            Some(a) => write!(log, "{a:#08x}"),
            None => write!(log, "none"),
        };
        let _ = writeln!(log, " {}\r", verdict(self.ok()));
    }
}

/// Compares one chunk at block offset `off` with what `expect` says.
fn compare(d: &mut Diff, off: u32, got: &[u8], pat: &mut Pattern, expect: Expect) {
    for (i, &g) in got.iter().enumerate() {
        let o = off + i as u32;
        let p = pat.next();
        let want = match expect {
            Expect::Erased => 0xFF,
            Expect::Pattern => p,
            Expect::PatternButSector if o / SECTOR_BYTES == SECTOR => 0xFF,
            Expect::PatternButSector => p,
        };
        if g != want {
            d.bad += 1;
            d.first_bad.get_or_insert(SCRATCH + o);
        }
    }
    d.bytes += got.len() as u32;
}

/// Reads the whole block `via` and compares it with `expect`.
fn check(q: Qspi, via: Via, expect: Expect) -> (Qspi, Diff) {
    let mut d = Diff::default();
    let mut pat = Pattern::new();
    let mut buf = [0u8; CHUNK];
    let mut q = q;
    match via {
        Via::Read03 | Via::ReadEb => {
            let (cmd, pre) = if via == Via::Read03 {
                (READ_03, qspi::PRESCALER_SAFE)
            } else {
                (QUAD_IO_READ, qspi::PRESCALER_MMAP)
            };
            if q.set_prescaler(pre).is_err() {
                d.errors += 1;
            }
            for off in (0..BLOCK_BYTES).step_by(CHUNK) {
                if q.read(&cmd.at(SCRATCH + off), &mut buf).is_err() {
                    d.errors += 1;
                }
                compare(&mut d, off, &buf, &mut pat, expect);
            }
            let _ = q.set_prescaler(qspi::PRESCALER_SAFE);
            (q, d)
        }
        Via::Window => {
            let _ = q.set_prescaler(qspi::PRESCALER_MMAP);
            let m = match q.memory_mapped(&QUAD_IO_READ_XIP) {
                Ok(m) => m,
                Err((q, _)) => {
                    d.errors += 1;
                    return (q, d);
                }
            };
            let win = &m.bytes()[SCRATCH as usize..NOR_BYTES as usize];
            for off in (0..BLOCK_BYTES).step_by(CHUNK) {
                let o = off as usize;
                compare(&mut d, off, &win[o..o + CHUNK], &mut pat, expect);
            }
            let mut q = m.exit();
            let _ = q.set_prescaler(qspi::PRESCALER_SAFE);
            (q, d)
        }
    }
}

/// Invalidates the D-cache lines holding `addr..addr + len` of the window.
fn invalidate(addr: usize, len: usize) {
    let start = addr & !31;
    let end = (addr + len).next_multiple_of(32);
    // SAFETY: one thread, no interrupt; the lines are of the read-only
    // window (MPU AP read-only), so none is dirty and the invalidate drops
    // no write.
    unsafe {
        cortex_m::Peripherals::steal()
            .SCB
            .invalidate_dcache_by_address(start, end - start)
    };
}

/// Unaligned window reads around `bases` (block offsets) against a 0x03
/// reference of `base - 8 .. base + 56`, cold and warm. Returns (checks,
/// mismatches, read errors).
fn unaligned(q: Qspi, bases: &[u32]) -> (Qspi, u32, u32, u32) {
    let mut q = q;
    let mut refs = [[0u8; 64]; 6];
    let mut errors = 0;
    for (r, &b) in refs.iter_mut().zip(bases) {
        errors += u32::from(q.read(&READ_03.at(SCRATCH + b - 8), r).is_err());
    }
    let _ = q.set_prescaler(qspi::PRESCALER_MMAP);
    let m = match q.memory_mapped(&QUAD_IO_READ_XIP) {
        Ok(m) => m,
        Err((q, _)) => return (q, 0, 0, errors + 1),
    };
    let win = m.bytes();
    let (mut n, mut bad) = (0u32, 0u32);
    for cold in [true, false] {
        for (want, &b) in refs.iter().zip(bases) {
            for i in 0..48 {
                let o = (SCRATCH + b - 8) as usize + i;
                let w = &want[i..i + 16];
                let w16 = u16::from_le_bytes([w[0], w[1]]);
                let w32 = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                let p = win[o..o + 16].as_ptr();
                let mut chk = |ok: bool| {
                    n += 1;
                    bad += u32::from(!ok);
                    if cold {
                        invalidate(qspi::WINDOW + o, 16);
                    }
                };
                if cold {
                    invalidate(qspi::WINDOW + o, 16);
                }
                chk(black_box(win[o]) == w[0]);
                chk(u16::from_le_bytes(black_box(&win[o..o + 2]).try_into().unwrap()) == w16);
                chk(u32::from_le_bytes(black_box(&win[o..o + 4]).try_into().unwrap()) == w32);
                // SAFETY: `p..p + 16` is inside `win`; read_unaligned has
                // no alignment requirement.
                chk(unsafe { black_box(p.cast::<u16>()).read_unaligned() } == w16);
                // SAFETY: as above.
                chk(unsafe { black_box(p.cast::<u32>()).read_unaligned() } == w32);
                let mut block = [0u8; 16];
                block.copy_from_slice(black_box(&win[o..o + 16]));
                chk(black_box(block) == *w);
            }
        }
    }
    let mut q = m.exit();
    let _ = q.set_prescaler(qspi::PRESCALER_SAFE);
    (q, n, bad, errors)
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, u32) {
    let t0 = cycles();
    let r = f();
    (r, cycles().wrapping_sub(t0))
}

/// Step 1: argument checks; none may reach the NOR (all inside the
/// scratch block anyway, so a broken check could only write there, before
/// step 2 erases it).
fn args(log: &mut Log, q: &mut Qspi) -> bool {
    let zeros = [0u8; 257];
    let mut ok = true;
    let _ = write!(log, "{PREFIX} {IMAGE} args");
    let mut case = |log: &mut Log, name: &str, got: Result<(), NorError>, want: NorError| {
        let good = got == Err(want);
        ok &= good;
        let _ = write!(log, " {name}={}", if good { "ok" } else { "WRONG" });
        good
    };
    let mut s = Scratch(q);
    let r = s.erase_4k(SCRATCH + 0x1001);
    case(log, "erase4_misaligned", r, NorError::Alignment);
    let r = s.erase_4k(SCRATCH + 0x800);
    case(log, "erase4_half", r, NorError::Alignment);
    let r = s.erase_64k(SCRATCH + 0x1000);
    case(log, "erase64_misaligned", r, NorError::Alignment);
    let r = s.program_page(SCRATCH + 0xF0, &zeros[..32]);
    case(log, "page_cross", r, NorError::Alignment);
    let r = s.program_page(SCRATCH, &zeros);
    case(log, "page_257", r, NorError::Alignment);
    let r = s.program_page(NOR_BYTES - 16, &zeros[..32]);
    if case(log, "page_past_end", r, NorError::OutOfRange) {
        // `program` past the end, called on the driver directly (the
        // range is not all scratch). Only once `program_page` has been
        // seen refusing a page past the end: a broken `program` check
        // then writes at most 0x7F_FFF0..0x80_0000 (scratch, erased by
        // step 2), and its second page, at 0x80_0000, which the NOR would
        // take as 0 (it ignores A23), is refused by `program_page`.
        let r = s.0.program(NOR_BYTES - 16, &zeros[..32]);
        case(log, "program_past_end", r, NorError::OutOfRange);
    }
    let _ = writeln!(log, " {}\r", verdict(ok));
    ok
}

/// Step 3: programs the pattern over the whole block.
fn program(log: &mut Log, q: &mut Qspi) -> bool {
    let mut pat = Pattern::new();
    let mut lens = Lcg(0x1E57_0001);
    let mut buf = [0u8; 1024];
    let mut s = Scratch(q);
    let mut errors = 0u32;
    let mut first_err = None;
    // One aligned full page with program_page: the clean tPP figure.
    pat.fill(&mut buf[..PAGE_BYTES as usize]);
    let (r, page_cycles) = timed(|| s.program_page(SCRATCH, &buf[..PAGE_BYTES as usize]));
    if let Err(e) = r {
        errors += 1;
        first_err.get_or_insert(e);
    }
    // The rest with `program`: a 1-byte piece first, then LCG lengths.
    let mut off = PAGE_BYTES;
    let (mut pieces, mut pages, mut crossing) = (0u32, 0u32, 0u32);
    let mut total = 0u32;
    let mut slowest = 0u32;
    while off < BLOCK_BYTES {
        let want = if pieces == 0 {
            1
        } else {
            1 + lens.next() % 1024
        };
        let n = want.min(BLOCK_BYTES - off);
        let data = &mut buf[..n as usize];
        pat.fill(data);
        let a = SCRATCH + off;
        let np = (a + n - 1) / PAGE_BYTES - a / PAGE_BYTES + 1;
        let (r, c) = timed(|| s.program(a, data));
        if let Err(e) = r {
            errors += 1;
            first_err.get_or_insert(e);
        }
        total = total.wrapping_add(c);
        slowest = slowest.max(c / np);
        pieces += 1;
        pages += np;
        crossing += u32::from(np > 1);
        off += n;
    }
    let ok = errors == 0;
    let _ = write!(
        log,
        "{PREFIX} {IMAGE} program page256={} (datasheet tPP typ {} max {}) pieces={pieces} \
         page_programs={pages} crossing_pieces={crossing} total={} per_page_avg={} \
         slowest_piece_per_page={} errors={errors} first_err=",
        Ms(page_cycles),
        UsMs(Op::PageProgram.typ_us()),
        UsMs(Op::PageProgram.max_us()),
        Ms(total),
        Ms(total / pages.max(1)),
        Ms(slowest),
    );
    let _ = match first_err {
        Some(e) => write!(log, "{e:?}"),
        None => write!(log, "none"),
    };
    let _ = writeln!(log, " {}\r", verdict(ok));
    ok
}

fn erase_line(log: &mut Log, step: &str, op: Op, r: Result<(), NorError>, c: u32) -> bool {
    let _ = writeln!(
        log,
        "{PREFIX} {IMAGE} {step} time={} (datasheet typ {} max {}) result={r:?} {}\r",
        Ms(c),
        UsMs(op.typ_us()),
        UsMs(op.max_us()),
        verdict(r.is_ok())
    );
    r.is_ok()
}

fn baud_table(log: &mut Log) {
    let ker = uart::kernel_hz();
    let _ = writeln!(log, "{PREFIX} {IMAGE} uart kernel_hz={ker}\r");
    for &bps in RATES.iter().chain(&TABLE_EXTRA) {
        let _ = match baud_setting(ker, bps) {
            Some(s) => writeln!(
                log,
                "{PREFIX} {IMAGE} baud bps={bps} brr={:#06x} over8={} actual={} error_ppm={} {}\r",
                s.brr,
                u8::from(s.over8),
                s.actual,
                s.error_ppm,
                if s.error_ppm.unsigned_abs() <= uart::MAX_ERROR_PPM {
                    "ok"
                } else {
                    "over2%"
                }
            ),
            None => writeln!(log, "{PREFIX} {IMAGE} baud bps={bps} unreachable\r"),
        };
    }
}

/// Steps 1 to 6; returns whether all passed.
fn nor_test(log: &mut Log, q: Qspi) -> (Qspi, bool) {
    let mut q = q;
    let mut all = true;
    all &= args(log, &mut q);

    // Step 2.
    let (r, c) = timed(|| Scratch(&mut q).erase_64k(SCRATCH));
    all &= erase_line(log, "erase64", Op::BlockErase, r, c);
    let (q1, d) = check(q, Via::Read03, Expect::Erased);
    q = q1;
    d.print(log, "erase64", "read03");
    all &= d.ok();

    // Step 3.
    all &= program(log, &mut q);

    // Step 4.
    for (via, name) in [
        (Via::Read03, "read03"),
        (Via::ReadEb, "readeb"),
        (Via::Window, "window"),
    ] {
        let (q1, d) = check(q, via, Expect::Pattern);
        q = q1;
        d.print(log, "verify", name);
        all &= d.ok();
    }
    // Page boundaries, a sector boundary, the block's last line, odd ones.
    let bases = [0x100, 0x1000, 0x20E0, 0x7777, 0xA000 + 32, BLOCK_BYTES - 64];
    let (q1, n, bad, err) = unaligned(q, &bases);
    q = q1;
    let ok = bad == 0 && err == 0 && n > 0;
    let _ = writeln!(
        log,
        "{PREFIX} {IMAGE} verify unaligned checks={n} mismatches={bad} errors={err} {}\r",
        verdict(ok)
    );
    all &= ok;

    // Step 5.
    let (r, c) = timed(|| Scratch(&mut q).erase_4k(SCRATCH + SECTOR * SECTOR_BYTES));
    all &= erase_line(log, "erase4", Op::SectorErase, r, c);
    for (via, name) in [(Via::Read03, "read03"), (Via::Window, "window")] {
        let (q1, d) = check(q, via, Expect::PatternButSector);
        q = q1;
        d.print(log, "erase4", name);
        all &= d.ok();
    }

    // Step 6.
    let (r, c) = timed(|| Scratch(&mut q).erase_64k(SCRATCH));
    all &= erase_line(log, "final_erase64", Op::BlockErase, r, c);
    for (via, name) in [(Via::Read03, "read03"), (Via::Window, "window")] {
        let (q1, d) = check(q, via, Expect::Erased);
        q = q1;
        d.print(log, "final", name);
        all &= d.ok();
    }

    // The registers after it all: WEL 0, WIP 0, no failure flag.
    let mut sr = [0u8];
    let mut scur = [0u8];
    let e1 = q.read(&Read::single(0x05), &mut sr).is_err();
    let e2 = q.read(&Read::single(0x2B), &mut scur).is_err();
    let ok = !e1 && !e2 && sr[0] == 0x40 && scur[0] & 0x60 == 0;
    let _ = writeln!(
        log,
        "{PREFIX} {IMAGE} status sr={:#04x} (want 0x40: QE only) scur={:#04x} exit_fallbacks={} {}\r",
        sr[0],
        scur[0],
        qspi::exit_fallbacks(),
        verdict(ok)
    );
    all &= ok;
    (q, all)
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    board.banner(IMAGE);
    let q = Qspi::init(qspi::PRESCALER_SAFE);
    let mut buf = [0u8; 6144];
    let mut log = Log {
        out: &mut board.uart,
        buf: &mut buf,
        len: 0,
    };
    let _ = writeln!(
        log,
        "{PREFIX} {IMAGE} scratch={SCRATCH:#08x}..{:#08x} (the only block written)\r",
        SCRATCH + BLOCK_BYTES
    );
    let (_q, all) = nor_test(&mut log, q);
    baud_table(&mut log);
    let _ = writeln!(log, "{PREFIX} {IMAGE} result {}\r", verdict(all));
    let _ = writeln!(
        log,
        "{PREFIX} {IMAGE} echo ready: bytes are echoed; ~<digit> sets the baud rate\r"
    );
    let len = log.len;
    let report = &buf[..len];
    let out = &mut board.uart;

    // Time in 10 ms ticks (each `read_timeout` that times out), so the
    // DWT counter's 9.1 s wrap does not matter.
    let (mut rx, mut tilde) = (0u32, false);
    let (mut idle_ticks, mut since_report) = (0u32, 0u32);
    loop {
        match out.read_timeout(CPU_HZ / 100) {
            Some(b) => {
                rx += 1;
                idle_ticks = 0;
                if tilde && b.is_ascii_digit() {
                    let bps = RATES[usize::from(b - b'0')];
                    let s = baud_setting(uart::kernel_hz(), bps);
                    let _ = writeln!(out, "\r\n{PREFIX} {IMAGE} baud -> {bps} {s:?}\r");
                    if let Err(e) = out.set_baud(bps) {
                        let _ = writeln!(out, "{PREFIX} {IMAGE} baud refused {e:?}\r");
                    }
                    let _ = writeln!(out, "{PREFIX} {IMAGE} baud now {}\r", uart::baud());
                } else if tilde && b == b'!' {
                    // The overrun check: 50 ms without reading, so bytes
                    // sent meanwhile overflow RDR (or the 16-byte RX FIFO)
                    // and set ORE (RM0433 48.5.6 "Overrun error").
                    board::wait_since(cycles(), CPU_HZ / 20);
                    let e = out.rx_errors();
                    let _ = writeln!(
                        out,
                        "\r\n{PREFIX} {IMAGE} stalled 50ms overruns_before={}\r",
                        e.overruns
                    );
                } else {
                    out.write_byte(b);
                }
                tilde = b == b'~';
                led::toggle();
            }
            None => {
                idle_ticks = idle_ticks.saturating_add(1);
                since_report = since_report.saturating_add(1);
            }
        }
        // Idle for 3 s and 10 s since the last report: print it again.
        if idle_ticks >= 300 && since_report >= 1000 {
            since_report = 0;
            report.iter().for_each(|&c| out.write_byte(c));
            let e = out.rx_errors();
            let _ = writeln!(
                out,
                "{PREFIX} {IMAGE} echo rx={rx} overruns={} framing={} noise={} baud={}\r",
                e.overruns,
                e.framing,
                e.noise,
                uart::baud()
            );
        }
    }
}
