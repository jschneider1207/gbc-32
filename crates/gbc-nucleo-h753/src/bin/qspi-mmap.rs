//! The `qspi-mmap` image (plan Stage 2 M2): the shield's NOR in
//! memory-mapped mode (0xEB, 1-4-4, [`qspi::QUAD_IO_READ`]), checked
//! against indirect reads, plus its read latency. Read commands only:
//! nothing is written to the NOR, so the checks are only as strong as its
//! current contents (the `data` line says how varied they are).
//!
//! Every 2 s, one run, with the D-cache on:
//!
//! - `compare`: [`SEGMENTS`] (start and end of the device, page and 64 KiB
//!   boundaries, fixed-seed LCG offsets; about 88 KiB) read with 0x03 at
//!   33 MHz as the reference, then with 0xEB in indirect mode at 99.2 MHz,
//!   then through the window (slice compares, as the core reads its ROM).
//!   Each round of up to 8 KiB leaves memory-mapped mode and enters it
//!   again, so the switch both ways (`Mapped::exit`) runs many times.
//! - `unaligned`: at every offset from 8 bytes before to 40 bytes after a
//!   few cache-line-aligned bases (so across two line boundaries each),
//!   byte, u16 and u32 reads in the core's way (`from_le_bytes` of a
//!   slice), `read_unaligned`, raw LDRH/LDR, and a 16-byte copy (the core's
//!   VRAM DMA block), against the 0x03 reference; with each access's lines
//!   invalidated first (cold), with them cached (warm), and with the
//!   window non-cacheable (uncached).
//! - `latency`: DWT cycles for one cold u32 read (its line invalidated),
//!   a cold 32-byte line, one uncached u32 read and one uncached read of
//!   the next word (prefetched), and a cold 64 KiB copy from the window.
//! - `result run=<n> PASS|FAIL`.
//!
//! After [`RUNS`] runs (about 80 s, so a serial read started after the
//! flash sees several), the fault test: a read of 0x9080_0000, just past
//! the 8 MiB window inside the QUADSPI bank, which the MPU must refuse
//! (the fault handler's line, `cfsr=0x00000082 mmfar=0x90800000`, with
//! the MPU still on in the handler, MPU_CTRL.HFNMIENA).
#![no_std]
#![no_main]

use core::fmt::Write;
use core::hint::black_box;

use cortex_m::peripheral::SCB;
use gbc_nucleo_h753::qspi::{self, Mapped, NOR_BYTES, QUAD_IO_READ, QUAD_IO_READ_XIP, Qspi, Read};
use gbc_nucleo_h753::{self as board, CPU_HZ, Uart, cycles, led, pac, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "qspi-mmap";
const NOR: u32 = NOR_BYTES;
/// Runs before the fault test.
const RUNS: u32 = 40;
/// The reference buffer: one compare round.
const ROUND_BYTES: usize = 8192;
/// Samples per latency figure.
const SAMPLES: usize = 64;

/// The 0x03 reference command.
const READ_03: Read = Read::single(0x03);

/// A fixed-seed LCG (Numerical Recipes constants): the same offsets on
/// every run and every build.
struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // The high bits are the better ones.
        self.0.rotate_right(8)
    }
}

const N_FIXED: usize = 6;
const N_PAGES: usize = 32;
const N_RANDOM: usize = 64;
const N_SEGMENTS: usize = N_FIXED + N_PAGES + N_RANDOM;

/// (offset, length) of every compared segment, each at most 4096 bytes.
const SEGMENTS: [(u32, u16); N_SEGMENTS] = segments();

const fn segments() -> [(u32, u16); N_SEGMENTS] {
    let mut s = [(0u32, 0u16); N_SEGMENTS];
    // The start, the end (its last byte: ES0392 2.7.5, the FSIZE margin),
    // the first 64 KiB boundary, the middle, and two odd-sized ones.
    s[0] = (0, 4096);
    s[1] = (NOR - 4096, 4096);
    s[2] = (0x1_0000 - 2048, 4096);
    s[3] = (NOR / 2 - 2048, 4096);
    s[4] = (NOR - 3, 3);
    s[5] = (0x12_3457, 777);
    // Page boundaries (256 B pages): 128 bytes on each side, shifted by
    // 0..6 so the window reads start unaligned too.
    let mut x = 0x2545_F491u32;
    let mut i = 0;
    while i < N_PAGES {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let page = 1 + (x >> 8) % (NOR / 256 - 2);
        s[N_FIXED + i] = (page * 256 - 128 + (i as u32 % 7), 256);
        i += 1;
    }
    // Random offsets (any byte), 1020 to 1024 bytes.
    let mut i = 0;
    while i < N_RANDOM {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let len = 1024 - (i as u32 % 5);
        s[N_FIXED + N_PAGES + i] = ((x >> 8) % (NOR - len + 1), len as u16);
        i += 1;
    }
    s
}

/// What one run found.
#[derive(Default)]
struct Tally {
    segments: u32,
    bytes: u32,
    /// Window bytes that differ from the 0x03 reference.
    mismatches: u32,
    /// Indirect 0xEB bytes that differ from the 0x03 reference.
    eb_mismatches: u32,
    /// Indirect read or mode switch errors.
    errors: u32,
    /// The first differing window address, if any.
    first_bad: Option<u32>,
    /// Reference bytes that are 0xFF, and which byte values occur.
    ff: u32,
    seen: [u32; 8],
    rounds: u32,
    /// Bit k: leaving round k needed EN = 0 (`Mapped::exit`).
    fallback_rounds: u32,
}

fn hex(out: &mut Uart, bytes: &[u8]) {
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
}

/// The SCB, for cache maintenance by address.
fn scb() -> SCB {
    // SAFETY: one thread, no interrupt; the handle is used only for
    // D-cache invalidation by address of lines of the read-only QUADSPI
    // window (never dirty, so invalidating loses nothing) and stays local.
    unsafe { cortex_m::Peripherals::steal().SCB }
}

/// Invalidates the D-cache lines that hold any of `addr..addr + len` of
/// the window, so the next read of them misses.
fn invalidate(addr: usize, len: usize) {
    let start = addr & !31;
    let end = (addr + len).next_multiple_of(32);
    // SAFETY: the lines are of the window, which is read-only (MPU AP
    // read-only), so none is dirty and the invalidate drops no write.
    unsafe { scb().invalidate_dcache_by_address(start, end - start) };
}

/// One compare round over `segs`: the 0x03 reference, indirect 0xEB
/// against it, then the window against it.
fn round(mut q: Qspi, segs: &[(u32, u16)], t: &mut Tally, refbuf: &mut [u8]) -> Qspi {
    let mut pos = 0;
    let _ = q.set_prescaler(qspi::PRESCALER_SAFE);
    for &(off, len) in segs {
        let len = usize::from(len);
        if q.read(&READ_03.at(off), &mut refbuf[pos..pos + len])
            .is_err()
        {
            t.errors += 1;
        }
        pos += len;
    }
    let _ = q.set_prescaler(qspi::PRESCALER_MMAP);
    let mut pos = 0;
    let mut chunk = [0u8; 256];
    for &(off, len) in segs {
        let len = usize::from(len);
        let mut k = 0;
        while k < len {
            let n = (len - k).min(chunk.len());
            if q.read(&QUAD_IO_READ.at(off + k as u32), &mut chunk[..n])
                .is_err()
            {
                t.errors += 1;
            }
            let want = &refbuf[pos + k..pos + k + n];
            t.eb_mismatches += want.iter().zip(&chunk[..n]).filter(|(a, b)| a != b).count() as u32;
            k += n;
        }
        for &b in &refbuf[pos..pos + len] {
            t.ff += u32::from(b == 0xFF);
            t.seen[usize::from(b >> 5)] |= 1 << (b & 31);
        }
        pos += len;
    }
    let m = match q.memory_mapped(&QUAD_IO_READ_XIP) {
        Ok(m) => m,
        Err((q, _)) => {
            t.errors += 1;
            return q;
        }
    };
    let win = m.bytes();
    let mut pos = 0;
    for &(off, len) in segs {
        let (o, len) = (off as usize, usize::from(len));
        let want = &refbuf[pos..pos + len];
        // The slice compare is the check (the core's way of reading); the
        // byte loop only counts, for the report.
        if win[o..o + len] != *want {
            for (i, (a, b)) in win[o..o + len].iter().zip(want).enumerate() {
                if a != b {
                    t.mismatches += 1;
                    t.first_bad.get_or_insert(off + i as u32);
                }
            }
        }
        t.segments += 1;
        t.bytes += len as u32;
        pos += len;
    }
    t.rounds += 1;
    m.exit()
}

fn compare(out: &mut Uart, mut q: Qspi) -> (Qspi, bool) {
    let mut t = Tally::default();
    let mut refbuf = [0u8; ROUND_BYTES];
    let mut start = 0;
    while start < SEGMENTS.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < SEGMENTS.len() && bytes + usize::from(SEGMENTS[end].1) <= ROUND_BYTES {
            bytes += usize::from(SEGMENTS[end].1);
            end += 1;
        }
        let f = qspi::exit_fallbacks();
        q = round(q, &SEGMENTS[start..end], &mut t, &mut refbuf);
        if qspi::exit_fallbacks() != f {
            t.fallback_rounds |= 1 << t.rounds.saturating_sub(1);
        }
        start = end;
    }
    let distinct: u32 = t.seen.iter().map(|w| w.count_ones()).sum();
    let ok = t.mismatches == 0 && t.eb_mismatches == 0 && t.errors == 0;
    let _ = writeln!(
        out,
        "{PREFIX} qspi-mmap data ff_bytes={} of={} distinct_values={distinct}\r",
        t.ff, t.bytes
    );
    let _ = write!(
        out,
        "{PREFIX} qspi-mmap compare segments={} bytes={} rounds={} mismatches={} \
         eb_mismatches={} errors={} fallback_rounds={:#x} first_bad=",
        t.segments, t.bytes, t.rounds, t.mismatches, t.eb_mismatches, t.errors, t.fallback_rounds
    );
    let _ = match t.first_bad {
        Some(a) => write!(out, "{a:#08x}"),
        None => write!(out, "none"),
    };
    let _ = writeln!(out, " {}\r", if ok { "PASS" } else { "FAIL" });
    (q, ok)
}

/// LDRH from `p`, whatever its alignment.
fn ldrh(p: *const u8) -> u16 {
    let v: u32;
    // SAFETY: `p..p + 2` is inside the mapped window (the caller's range),
    // which is readable Normal memory; with CCR.UNALIGN_TRP = 0 (printed in
    // the `config` line) an unaligned LDRH is allowed there (Armv7-M
    // A3.2.1). The asm only reads.
    unsafe {
        core::arch::asm!("ldrh {v}, [{p}]", p = in(reg) p, v = out(reg) v,
            options(readonly, nostack, preserves_flags));
    }
    v as u16
}

/// LDR from `p`, whatever its alignment.
fn ldr(p: *const u8) -> u32 {
    let v: u32;
    // SAFETY: as `ldrh`, for `p..p + 4`.
    unsafe {
        core::arch::asm!("ldr {v}, [{p}]", p = in(reg) p, v = out(reg) v,
            options(readonly, nostack, preserves_flags));
    }
    v
}

/// The cache states of the unaligned test.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Cold,
    Warm,
    Uncached,
}

/// Checks the access kinds at every offset `base - 8 .. base + 40` against
/// `want`, the reference of `base - 8 .. base + 56`. Returns (checked,
/// mismatches).
fn unaligned_at(win: &[u8], base: usize, want: &[u8; 64], st: State) -> (u32, u32) {
    let (mut n, mut bad) = (0u32, 0u32);
    let mut check = |ok: bool| {
        n += 1;
        bad += u32::from(!ok);
    };
    let cold = |o: usize| {
        if st == State::Cold {
            invalidate(qspi::WINDOW + o, 16);
        }
    };
    for i in 0..48 {
        let o = base - 8 + i;
        let w = &want[i..i + 16];
        let w16 = u16::from_le_bytes([w[0], w[1]]);
        let w32 = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        let p = win[o..o + 16].as_ptr();
        cold(o);
        check(black_box(win[o]) == w[0]);
        cold(o);
        check(u16::from_le_bytes(black_box(&win[o..o + 2]).try_into().unwrap()) == w16);
        cold(o);
        check(u32::from_le_bytes(black_box(&win[o..o + 4]).try_into().unwrap()) == w32);
        cold(o);
        // SAFETY: `p..p + 16` is inside `win`; read_unaligned has no
        // alignment requirement.
        check(unsafe { black_box(p.cast::<u16>()).read_unaligned() } == w16);
        cold(o);
        // SAFETY: as above.
        check(unsafe { black_box(p.cast::<u32>()).read_unaligned() } == w32);
        cold(o);
        check(ldrh(black_box(p)) == w16);
        cold(o);
        check(ldr(black_box(p)) == w32);
        cold(o);
        let mut block = [0u8; 16];
        block.copy_from_slice(black_box(&win[o..o + 16]));
        check(black_box(block) == *w);
    }
    (n, bad)
}

/// Line-aligned bases of the unaligned test: the start (from 8), low
/// addresses, the end of the device (up to its last byte), and random.
fn unaligned_bases() -> [usize; 8] {
    let mut r = Lcg(0x0BAD_5EED);
    let mut b = [64, 0x1000, 0x2_0020, NOR as usize - 64, 0, 0, 0, 0];
    for x in &mut b[4..] {
        *x = 32 + (r.next() as usize % (NOR as usize / 32 - 4)) * 32;
    }
    b
}

fn unaligned(out: &mut Uart, q: Qspi) -> (Qspi, bool) {
    let bases = unaligned_bases();
    let mut refs = [[0u8; 64]; 8];
    let mut q = q;
    let mut errors = 0;
    let _ = q.set_prescaler(qspi::PRESCALER_SAFE);
    for (r, &b) in refs.iter_mut().zip(&bases) {
        errors += u32::from(q.read(&READ_03.at(b as u32 - 8), r).is_err());
    }
    let _ = q.set_prescaler(qspi::PRESCALER_MMAP);
    let mut m: Mapped = match q.memory_mapped(&QUAD_IO_READ_XIP) {
        Ok(m) => m,
        Err((q, _)) => {
            let _ = writeln!(out, "{PREFIX} qspi-mmap unaligned enter=err FAIL\r");
            return (q, false);
        }
    };
    let mut res = [(0u32, 0u32); 3];
    for (k, st) in [State::Cold, State::Warm, State::Uncached]
        .into_iter()
        .enumerate()
    {
        if st == State::Uncached {
            for &b in &bases {
                invalidate(qspi::WINDOW + b - 8, 64);
            }
            m.set_cacheable(false);
        }
        let win = m.bytes();
        for (r, &b) in refs.iter().zip(&bases) {
            let (n, bad) = unaligned_at(win, b, r, st);
            res[k].0 += n;
            res[k].1 += bad;
        }
        m.set_cacheable(true);
    }
    let bad: u32 = res.iter().map(|r| r.1).sum();
    let ok = bad == 0 && errors == 0;
    let _ = writeln!(
        out,
        "{PREFIX} qspi-mmap unaligned bases={} offsets_per_base=48 kinds=8 cold={}/{} warm={}/{} \
         uncached={}/{} (mismatches/checked) errors={errors} {}\r",
        bases.len(),
        res[0].1,
        res[0].0,
        res[1].1,
        res[1].0,
        res[2].1,
        res[2].0,
        if ok { "PASS" } else { "FAIL" }
    );
    (m.exit(), ok)
}

/// min/median/max of `s` (sorted in place).
fn stats(s: &mut [u32]) -> (u32, u32, u32) {
    s.sort_unstable();
    (s[0], s[s.len() / 2], s[s.len() - 1])
}

/// Cycles of `f` between two cycle-counter reads, after a DSB so every
/// load `f` issued has completed.
#[inline(always)]
fn timed(f: impl FnOnce()) -> u32 {
    let t0 = cycles();
    f();
    cortex_m::asm::dsb();
    cycles().wrapping_sub(t0)
}

fn latency(out: &mut Uart, q: Qspi) -> Qspi {
    let mut m = match q.memory_mapped(&QUAD_IO_READ_XIP) {
        Ok(m) => m,
        Err((q, _)) => {
            let _ = writeln!(out, "{PREFIX} qspi-mmap latency enter=err\r");
            return q;
        }
    };
    let mut r = Lcg(0x1A7E_0C1E);
    // Random word or line addresses far apart (the QUADSPI's prefetch
    // never already holds them).
    let mut word = || qspi::WINDOW + (r.next() as usize % (NOR as usize / 32 - 1)) * 32 + 4;
    // The measurement's own cost: the same sequence on a DTCM word.
    let local = 0u32;
    let mut s = [0u32; SAMPLES];
    for x in &mut s {
        *x = timed(|| {
            // SAFETY: a volatile read of a live local.
            black_box(unsafe { (&raw const local).read_volatile() });
        });
    }
    let base = stats(&mut s).0;
    let sub = |s: &mut [u32]| {
        for x in s.iter_mut() {
            *x = x.saturating_sub(base);
        }
        stats(s)
    };
    let mut figs = [(0u32, 0u32, 0u32); 4];
    // (i) one cold u32 read: its line invalidated, so a line fill.
    for x in &mut s {
        let a = word();
        invalidate(a, 4);
        *x = timed(|| {
            // SAFETY: `a..a + 4` is inside the mapped window, 4-aligned.
            black_box(unsafe { (a as *const u32).read_volatile() });
        });
    }
    figs[0] = sub(&mut s);
    // (ii) a cold 32-byte line, word by word.
    for x in &mut s {
        let a = word() & !31;
        invalidate(a, 32);
        *x = timed(|| {
            for k in 0..8 {
                // SAFETY: inside the window, 4-aligned.
                black_box(unsafe { (a as *const u32).add(k).read_volatile() });
            }
        });
    }
    figs[1] = sub(&mut s);
    // (iii) uncached: one u32 read, then the next word (prefetched).
    for &b in &[0usize, 1] {
        m.set_cacheable(false);
        for x in &mut s {
            let a = word();
            // SAFETY: inside the window, 4-aligned.
            let first = || black_box(unsafe { (a as *const u32).read_volatile() });
            if b == 0 {
                *x = timed(|| {
                    first();
                });
            } else {
                first();
                cortex_m::asm::dsb();
                *x = timed(|| {
                    // SAFETY: as above, the next word.
                    black_box(unsafe { (a as *const u32).add(1).read_volatile() });
                });
            }
        }
        m.set_cacheable(true);
        figs[2 + b] = sub(&mut s);
    }
    // (iv) a cold 64 KiB copy, 4 KiB at a time into a DTCM buffer.
    let win = m.bytes();
    let from = 0x10_0000;
    invalidate(qspi::WINDOW + from, 64 << 10);
    let mut buf = [0u8; 4096];
    let copy = timed(|| {
        for k in 0..16 {
            buf.copy_from_slice(&win[from + k * 4096..from + (k + 1) * 4096]);
            black_box(&mut buf);
        }
    });
    let kb_s = u64::from(CPU_HZ) * 64 * 1000 / 1024 / u64::from(copy.max(1));
    let _ = writeln!(
        out,
        "{PREFIX} qspi-mmap latency cpu_hz={CPU_HZ} qspi_hz={} overhead={base} \
         (cycles min/median/max of {SAMPLES}, overhead subtracted) cold_u32={}/{}/{} \
         cold_line32={}/{}/{} uncached_u32={}/{}/{} uncached_next_u32={}/{}/{} \
         copy64k_cold_cycles={copy} copy64k_mb_s={}.{:03}\r",
        m.clock_hz(),
        figs[0].0,
        figs[0].1,
        figs[0].2,
        figs[1].0,
        figs[1].1,
        figs[1].2,
        figs[2].0,
        figs[2].1,
        figs[2].2,
        figs[3].0,
        figs[3].1,
        figs[3].2,
        kb_s / 1000,
        kb_s % 1000
    );
    m.exit()
}

/// The registers that say how memory-mapped mode is set up, read while it
/// is on, and the NOR's first bytes through the window.
fn config(out: &mut Uart, mut q: Qspi) -> Qspi {
    let _ = q.set_prescaler(qspi::PRESCALER_MMAP);
    let m = match q.memory_mapped(&QUAD_IO_READ_XIP) {
        Ok(m) => m,
        Err((q, e)) => {
            let _ = writeln!(out, "{PREFIX} qspi-mmap config enter=err:{e:?}\r");
            return q;
        }
    };
    // SAFETY: read-only accesses to QUADSPI registers (CR, DCR, CCR, ABR,
    // SR), and to the core's MPU and SCB registers; the MPU_RNR write
    // selects region 6 for the RBAR/RASR reads and changes no region.
    let (cr, dcr, ccr, abr, sr, ctrl, rbar, rasr, ccr_core) = unsafe {
        let q = &*pac::QUADSPI::ptr();
        let mpu = &*cortex_m::peripheral::MPU::PTR;
        mpu.rnr.write(board::mpu::REGION_QSPI);
        (
            q.cr.read().bits(),
            q.dcr.read().bits(),
            q.ccr.read().bits(),
            q.abr.read().bits(),
            q.sr.read().bits(),
            mpu.ctrl.read(),
            mpu.rbar.read(),
            mpu.rasr.read(),
            (*SCB::PTR).ccr.read(),
        )
    };
    let _ = write!(
        out,
        "{PREFIX} qspi-mmap config clk_hz={} cr={cr:#010x} dcr={dcr:#010x} ccr={ccr:#010x} \
         abr={abr:#04x} sr={sr:#06x} mpu_ctrl={ctrl:#x} r6_rbar={rbar:#010x} r6_rasr={rasr:#010x} \
         unalign_trp={} first16=",
        m.clock_hz(),
        (ccr_core >> 3) & 1
    );
    hex(out, &m.bytes()[..16]);
    let _ = writeln!(out, "\r");
    m.exit()
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    let mut q = Qspi::init(qspi::PRESCALER_SAFE);
    let mut n = 0u32;
    loop {
        let t0 = cycles();
        if n.is_multiple_of(4) {
            board.banner(IMAGE);
        }
        let out = &mut board.uart;
        let _ = writeln!(out, "{PREFIX} qspi-mmap run n={n} of={RUNS}\r");
        // Mode switches that needed EN = 0 (`Mapped::exit`), per phase.
        let mut fb = [0u32; 4];
        let mut f0 = qspi::exit_fallbacks();
        let mut tick = |k: usize| {
            let f = qspi::exit_fallbacks();
            fb[k] = f - f0;
            f0 = f;
        };
        q = config(out, q);
        tick(0);
        let (q1, ok_c) = compare(out, q);
        tick(1);
        let (q2, ok_u) = unaligned(out, q1);
        tick(2);
        q = latency(out, q2);
        tick(3);
        let _ = writeln!(
            out,
            "{PREFIX} qspi-mmap result run={n} exit_fallbacks={} (config={} compare={} \
             unaligned={} latency={}) {}\r",
            qspi::exit_fallbacks(),
            fb[0],
            fb[1],
            fb[2],
            fb[3],
            if ok_c && ok_u { "PASS" } else { "FAIL" }
        );
        wait_since(t0, CPU_HZ);
        led::toggle();
        wait_since(t0, 2 * CPU_HZ);
        led::toggle();
        n += 1;
        if n == RUNS {
            break;
        }
    }
    // The fault test: past the window, inside the QUADSPI bank.
    let m = q
        .memory_mapped(&QUAD_IO_READ_XIP)
        .unwrap_or_else(|_| panic!("memory-mapped mode for the fault test"));
    let past = qspi::WINDOW + NOR as usize;
    let _ = writeln!(
        board.uart,
        "{PREFIX} qspi-mmap faulttest addr={past:#010x} window_last={:#04x} \
         expect=hardfault cfsr=0x00000082 mmfar={past:#010x}\r",
        m.bytes()[NOR as usize - 1]
    );
    // SAFETY: a volatile byte read of an address no Rust object lives at,
    // which the MPU makes no-access: the load faults precisely (MemManage,
    // escalated to HardFault) before any data is read, and the fault
    // handler never returns, so no value is ever produced from it. If it
    // does not fault, the line below says so.
    let v = unsafe { (past as *const u8).read_volatile() };
    loop {
        let _ = writeln!(
            board.uart,
            "{PREFIX} qspi-mmap faulttest FAIL no fault, read {v:#04x}\r"
        );
        wait_since(cycles(), CPU_HZ);
    }
}
