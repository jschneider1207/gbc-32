//! Audio out (plan Stage 2 M4, decision D4 (a)): DAC1, both channels, at
//! 32,768 Hz (ADR 0021), fed by DMA1 from a circular buffer in AXI SRAM.
//! Facts, register values and measurements: `docs/reference/audio-dac.md`.
//!
//! # Scheme
//!
//! TIM6 overflows at the sample rate; its update event is TRGO, which
//! triggers both DAC channels at once ("simultaneous trigger", RM0433 Rev 9
//! 26.4.14): each trigger moves DHR1/DHR2 to DOR1/DOR2 and, through
//! channel 1's DMA request (DMAEN1 only, 26.4.8: one request and one
//! stream serve both channels in dual mode), has DMA1 stream 0 write the
//! next frame to DAC_DHR12RD (one 32-bit word: left in bits 11:0, right in
//! 27:16). The stream runs in circular mode over [`FRAMES`] frames; its
//! half-transfer and transfer-complete interrupts ([`on_dma`]) refill the
//! half the DMA has just left while it reads the other ("ping-pong" over
//! one circular buffer, not the stream's DBM mode, which would need two
//! address registers for no gain).
//!
//! Left is DAC1_OUT1 on PA4, right DAC1_OUT2 on PA5, both buffered (line
//! level; RM0433 26.4.11, DAC_MCR MODEx = 000).
//!
//! # Clocks and rate
//!
//! TIM6 is on APB1 (D2PPRE1 /2, 118 MHz). RM0433 8.5.6 Table 57: with
//! TIMPRE = 0 (reset value, never changed) and D2PPRE1 = /2, the timer
//! kernel clock is rcc_hclk1 = [`TIM_HZ`] = 236 MHz. PSC 0, ARR 7201: one
//! update per 7,202 clocks, 236,000,000 / 7,202 = 32,768.675 Hz, +20.6 ppm
//! against 32,768 Hz (the nearest divider; 7,201 is +159.5 ppm). The HSI's
//! own tolerance comes on top ([`crate::clock`]: 63.7 to 64.3 MHz at
//! 30 °C, -2% to +1% over temperature; this board +0.36%).
//!
//! # The buffer
//!
//! [`FRAMES`] = 512 frames of one `u32` (2 KiB), aligned to its size, in
//! `.axisram` (DMA1 reaches AXI SRAM but not the TCMs, RM0433 Table 3), on
//! MPU region [`crate::mpu::REGION_AUDIO_BUF`] Normal non-cacheable, so
//! the ISR's stores reach the SRAM without cache maintenance. Each half
//! ([`HALF`], 256 frames) is 7.81 ms: the refill has that long after its
//! interrupt before the DMA wraps into it. Halves of 256 keep the
//! interrupt rate at 128 per second; the emulator's frame (about 550
//! samples) is buffered upstream in the sample ring, so the DMA buffer
//! only has to ride out interrupt latency and the main loop, and the
//! output latency it adds is at most 15.6 ms.
//!
//! # Start and stop without a click
//!
//! The DAC is enabled at code 0 by [`AudioDac::new`] (with the buffer on,
//! the output cannot go below about 0.2 V, DS12117 Table 176, so the step
//! from the idle pin is at most that). [`AudioDac::start`] ramps both
//! channels from 0 to mid-scale ([`MID`], about 1.65 V) over
//! [`RAMP_FRAMES`] (250 ms) with a smoothstep (3t² - 2t³: no step and no
//! slope step at either end, so an AC-coupled load sees a slow sub-audio
//! swing instead of a thump) before the source is called. The source then
//! should start at mid-scale (silence). [`AudioDac::stop`] ramps from the
//! last frame the source wrote back to 0 the same way, plays a whole half
//! of zeros, then stops TIM6 and the stream; the DAC stays enabled at 0.
//!
//! # Errors
//!
//! The stream's TEIF (bus error; the stream disables itself), DMEIF and
//! FEIF (direct mode: a memory port not granted in time, RM0433 15.3.20),
//! and the DAC's DMAUDR1/DMAUDR2 (a trigger before the previous request
//! was served; the DAC then stops requesting, 26.4.8) are counted in
//! [`stats`]. After an underrun the DAC issues no new request until it is
//! re-initialised (26.4.8), and after TEIF the stream is off: either way
//! the refills stop, and with them anything waiting for one. Recovery
//! (M4 review P-2) is [`AudioDac::watchdog`], called by the main loop
//! while it waits (each WFI wake: TE, 60 per second, still comes): with
//! no refill for longer than its bound it counts the flags the ISR never
//! saw, re-initialises the stream and the DAC's request, holding the
//! output's current level, and counts a restart ([`Stats::restarts`]).
//! [`AudioDac::stop`] times out on a stalled stream and shuts it down
//! anyway. ES0392 2.10.2 (DMAUDR missed when a trigger coincides
//! with a DMA acknowledge) only concerns mixed software and hardware
//! triggers; only TIM6 triggers here.
//!
//! # Interrupt
//!
//! DMA1 stream 0, NVIC interrupt 11, DMA1_STR0. The image defines the
//! vector and calls [`on_dma`] with its source:
//!
//! ```ignore
//! #[interrupt]
//! fn DMA1_STR0() {
//!     gbc_nucleo_h753::audio_dac::on_dma(|_half, frames| ring.pop_into(frames));
//! }
//! ```
//!
//! # Samples in, DAC words out (ADR 0044)
//!
//! [`on_dma`]'s source writes board-independent frames
//! (`gbc_firmware::audio::PcmFrame::to_bits` words, signed 16-bit PCM,
//! what the sample ring holds) into a scratch half; `on_dma` then
//! converts them into the DMA half as `DAC_DHR12RD` words with
//! `gbc_firmware::audio::u12_lanes` (12-bit codes `2048 + n/16`, rounded
//! and clamped; DHR12RD's channels sit in the same 16-bit lanes as the
//! frame's, so no shuffle). This is the only
//! place the DAC's register layout and resolution meet the audio path;
//! the codes are bit-identical to the ones the shared code produced
//! before ADR 0044. [`on_dma_codes`] takes DAC words directly (the
//! dac-probe image's test patterns).
//!
//! An image that links this lib without [`AudioDac`] needs no handler:
//! the interrupt is only unmasked by [`AudioDac::start`].

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use crate::{clock, cycles, mpu, pac};

/// The TIM6 kernel clock, nominal (module docs).
pub const TIM_HZ: u32 = clock::HCLK_HZ;
/// Timer clocks per sample: PSC 0, ARR [`TIM_DIV`] - 1.
pub const TIM_DIV: u32 = 7_202;
/// The nominal sample rate, in mHz: 32,768,675 (32,768.675 Hz).
pub const RATE_MILLIHZ: u32 = ((TIM_HZ as u64 * 1000 + TIM_DIV as u64 / 2) / TIM_DIV as u64) as u32;
const _: () = assert!(TIM_HZ == 236_000_000 && RATE_MILLIHZ == 32_768_675);
/// The rate the core produces samples at (ADR 0021).
pub const TARGET_HZ: u32 = 32_768;

/// Frames in the circular buffer.
pub const FRAMES: usize = 512;
/// Frames per half (one refill).
pub const HALF: usize = FRAMES / 2;
/// Mid-scale, the 12-bit code of silence.
pub const MID: u16 = 2048;
/// The largest 12-bit code.
pub const MAX: u16 = 4095;
/// The start and stop ramp, in frames (2^13: 250 ms).
pub const RAMP_FRAMES: u32 = 1 << RAMP_SHIFT;
const RAMP_SHIFT: u32 = 13;

/// One DAC frame for DAC_DHR12RD: `left` (channel 1, PA4) in bits 11:0,
/// `right` (channel 2, PA5) in bits 27:16, both 12-bit codes (higher bits
/// are dropped).
#[inline(always)]
pub const fn frame(left: u16, right: u16) -> u32 {
    (left as u32 & 0xFFF) | (right as u32 & 0xFFF) << 16
}
/// Both channels at mid-scale.
pub const SILENCE: u32 = frame(MID, MID);

/// The circular buffer, aligned to and as large as its MPU region.
#[repr(C, align(2048))]
struct Buffer([u32; FRAMES]);
const BUFFER_BYTES: usize = FRAMES * 4;
const _: () = assert!(core::mem::size_of::<Buffer>() == BUFFER_BYTES);
const _: () = assert!(core::mem::align_of::<Buffer>() == BUFFER_BYTES);

struct Slot(UnsafeCell<Buffer>);
// SAFETY: the buffer is written by `start` (stream disabled, interrupt
// masked) and otherwise only by `on_dma`, one half at a time, the half the
// DMA is not reading; `AudioDac::new`'s guard makes `start` single-owner.
unsafe impl Sync for Slot {}
// SAFETY: `.axisram` is the NOLOAD AXI SRAM section memory.x
// (memory_x::firmware_memory_x) defines for DMA buffers; not zeroed at
// boot, which is sound for `u32` words. `start` fills both halves before
// the stream is enabled.
#[unsafe(link_section = ".axisram.audio_dac")]
static BUF: Slot = Slot(UnsafeCell::new(Buffer([0; FRAMES])));

fn buffer_addr() -> usize {
    BUF.0.get() as usize
}

/// Half `h` (0 or 1) of the buffer.
///
/// # Safety
/// No other reference to that half may exist, and the DMA must not be
/// reading it (see [`Slot`]).
unsafe fn half_mut(h: usize) -> &'static mut [u32] {
    debug_assert!(h < 2);
    // SAFETY: in bounds (h < 2); exclusive per the caller's contract.
    unsafe { core::slice::from_raw_parts_mut((BUF.0.get() as *mut u32).add(h * HALF), HALF) }
}

// RCC enables.
const RCC_AHB1_DMA1EN: u32 = 1 << 0; // RM0433 8.7.41: DMA1 and DMAMUX1
const RCC_AHB4_GPIOAEN: u32 = 1 << 0; // 8.7.43
const RCC_APB1L_TIM6EN: u32 = 1 << 4; // 8.7.45
const RCC_APB1L_DAC12EN: u32 = 1 << 29; // 8.7.45

// DAC_CR (RM0433 26.7.1): channel 1 in bits 15:0, channel 2 the same
// layout in 31:16.
const CR_EN1: u32 = 1 << 0;
const CR_TEN1: u32 = 1 << 1;
const CR_TSEL1_SHIFT: u32 = 2;
const CR_DMAEN1: u32 = 1 << 12;
const CR_CH2_SHIFT: u32 = 16;
/// TSELx = 5: dac_chx_trg5 = tim6_trgo (RM0433 Table 229).
pub const TSEL_TIM6_TRGO: u32 = 5;
/// DAC_CR without DMAEN1: both channels enabled, triggered by TIM6 TRGO.
pub const DAC_CR: u32 = (CR_EN1 | CR_TEN1 | TSEL_TIM6_TRGO << CR_TSEL1_SHIFT)
    | (CR_EN1 | CR_TEN1 | TSEL_TIM6_TRGO << CR_TSEL1_SHIFT) << CR_CH2_SHIFT;
const _: () = assert!(DAC_CR == 0x0017_0017);
/// DAC_MCR: MODE1 = MODE2 = 000, normal mode, buffer on, external pin only
/// (RM0433 26.7.16).
pub const DAC_MCR: u32 = 0;
// DAC_SR (26.7.14): DMAUDR1 bit 13, DMAUDR2 bit 29 (write 1 to clear).
const SR_DMAUDR1: u32 = 1 << 13;
const SR_DMAUDR2: u32 = 1 << 29;

// TIM6 (RM0433 42.4).
const TIM_CR1_CEN: u32 = 1 << 0;
const TIM_CR1_ARPE: u32 = 1 << 7;
/// TIM6_CR2 MMS = 010: the update event is TRGO (42.4.2).
pub const TIM_CR2: u32 = 0b010 << 4;
const TIM_EGR_UG: u32 = 1 << 0;

/// DMAMUX1 channel 0 feeds DMA1 stream 0 (RM0433 Table 103:
/// dmamux1_req_out0 = dma1_str0); request 67 is dac_ch1_dma (Table 124).
pub const DMAMUX_REQ_DAC_CH1: u32 = 67;
const STREAM: usize = 0;

// DMA_SxCR (RM0433 15.5.5).
const SCR_EN: u32 = 1 << 0;
const SCR_DMEIE: u32 = 1 << 1;
const SCR_TEIE: u32 = 1 << 2;
const SCR_HTIE: u32 = 1 << 3;
const SCR_TCIE: u32 = 1 << 4;
const SCR_DIR_M2P: u32 = 0b01 << 6;
const SCR_CIRC: u32 = 1 << 8;
const SCR_MINC: u32 = 1 << 10;
const SCR_PSIZE_WORD: u32 = 0b10 << 11;
const SCR_MSIZE_WORD: u32 = 0b10 << 13;
const SCR_PL_HIGH: u32 = 0b10 << 16;
/// The stream configuration, without EN: memory to peripheral, circular,
/// memory increment, word to word, single transfers (no burst), priority
/// high, interrupts on half and full transfer and on every error.
pub const DMA_SCR: u32 = SCR_DIR_M2P
    | SCR_CIRC
    | SCR_MINC
    | SCR_PSIZE_WORD
    | SCR_MSIZE_WORD
    | SCR_PL_HIGH
    | SCR_HTIE
    | SCR_TCIE
    | SCR_TEIE
    | SCR_DMEIE;
const _: () = assert!(DMA_SCR == 0x0002_555E);
/// DMA_SxFCR: direct mode (DMDIS = 0), FEIE (bit 7).
pub const DMA_SFCR: u32 = 1 << 7;
// DMA_LISR / LIFCR, stream 0 (15.5.1, 15.5.3).
const ISR_FEIF: u32 = 1 << 0;
const ISR_DMEIF: u32 = 1 << 2;
const ISR_TEIF: u32 = 1 << 3;
const ISR_HTIF: u32 = 1 << 4;
const ISR_TCIF: u32 = 1 << 5;
const ISR_ALL: u32 = ISR_FEIF | ISR_DMEIF | ISR_TEIF | ISR_HTIF | ISR_TCIF;

// The phases (`on_dma` and the main thread).
const IDLE: u8 = 0;
const RAMP_UP: u8 = 1;
const PLAY: u8 = 2;
const RAMP_DOWN: u8 = 3;
const DRAIN: u8 = 4;
const STOPPED: u8 = 5;
static PHASE: AtomicU8 = AtomicU8::new(IDLE);
static STOP_REQ: AtomicBool = AtomicBool::new(false);
/// Frames of the current ramp done (ISR-owned while running).
static RAMP_POS: AtomicU32 = AtomicU32::new(0);
/// Refills of zeros in DRAIN.
static DRAINED: AtomicU32 = AtomicU32::new(0);
/// The last frame the source wrote: where the stop ramp starts.
static LAST_FRAME: AtomicU32 = AtomicU32::new(SILENCE);

// Statistics (`stats`).
static REFILLS: AtomicU32 = AtomicU32::new(0);
static LAST_AT: AtomicU32 = AtomicU32::new(0);
static LATE: AtomicU32 = AtomicU32::new(0);
/// The half a late refill wrote, whose second interrupt is still to come
/// ([`NO_HALF`]: none; M4 review P-3).
static SKIP_HALF: AtomicU8 = AtomicU8::new(NO_HALF);
const NO_HALF: u8 = 2;
static SPURIOUS: AtomicU32 = AtomicU32::new(0);
static TE: AtomicU32 = AtomicU32::new(0);
static DME: AtomicU32 = AtomicU32::new(0);
static FE: AtomicU32 = AtomicU32::new(0);
static UDR1: AtomicU32 = AtomicU32::new(0);
static UDR2: AtomicU32 = AtomicU32::new(0);
static RESTARTS: AtomicU32 = AtomicU32::new(0);
static ISR_MIN: AtomicU32 = AtomicU32::new(u32::MAX);
static ISR_MAX: AtomicU32 = AtomicU32::new(0);

fn dac() -> &'static pac::dac::RegisterBlock {
    // SAFETY: a shared reference to the memory-mapped register block,
    // valid for the whole program. DAC1 belongs to this module; the main
    // thread writes CR and the data registers only while the stream is
    // off or (DMAEN1 only) with the interrupt masked, the ISR only writes
    // SR (write 1 to clear).
    unsafe { &*pac::DAC::ptr() }
}

fn dma() -> &'static pac::dma1::RegisterBlock {
    // SAFETY: as `dac`: stream 0 and its LISR/LIFCR bits belong to this
    // module; the main thread configures the stream only while it is
    // disabled and its interrupt masked, the ISR only reads LISR and NDTR
    // and writes LIFCR's stream 0 bits.
    unsafe { &*pac::DMA1::ptr() }
}

fn tim() -> &'static pac::tim6::RegisterBlock {
    // SAFETY: as `dac`; TIM6 belongs to this module, main thread only.
    unsafe { &*pac::TIM6::ptr() }
}

/// What the driver counted since the last [`reset_stats`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    /// Refills (half and full transfer interrupts handled).
    pub refills: u32,
    /// Cycle counter at the last refill interrupt's entry.
    pub last_at: u32,
    /// Refills missed: interrupts that found both HTIF and TCIF set, and
    /// single-flag interrupts entered about a half late, whose flag
    /// names the other half than the one the DMA has left (M4 review
    /// P-3).
    pub late: u32,
    /// Interrupts with neither (error only, or spurious).
    pub spurious: u32,
    /// Stream errors: transfer (TEIF), direct mode (DMEIF), FIFO (FEIF).
    pub te: u32,
    pub dme: u32,
    pub fe: u32,
    /// DAC DMA underruns, channel 1 and 2 (DMAUDR1/2).
    pub udr1: u32,
    pub udr2: u32,
    /// Stalls recovered by [`AudioDac::watchdog`] (each also lost audio:
    /// the run is not clean).
    pub restarts: u32,
    /// The interrupt's cost, entry to exit, in CPU cycles (min, max; min
    /// is `u32::MAX` before the first).
    pub isr_min: u32,
    pub isr_max: u32,
}

/// A consistent snapshot of the counters (taken with interrupts off).
pub fn stats() -> Stats {
    cortex_m::interrupt::free(|_| Stats {
        refills: REFILLS.load(Ordering::Relaxed),
        last_at: LAST_AT.load(Ordering::Relaxed),
        late: LATE.load(Ordering::Relaxed),
        spurious: SPURIOUS.load(Ordering::Relaxed),
        te: TE.load(Ordering::Relaxed),
        dme: DME.load(Ordering::Relaxed),
        fe: FE.load(Ordering::Relaxed),
        udr1: UDR1.load(Ordering::Relaxed),
        udr2: UDR2.load(Ordering::Relaxed),
        restarts: RESTARTS.load(Ordering::Relaxed),
        isr_min: ISR_MIN.load(Ordering::Relaxed),
        isr_max: ISR_MAX.load(Ordering::Relaxed),
    })
}

/// Clears the interrupt's min/max cost (the counters keep counting).
pub fn reset_isr_cost() {
    cortex_m::interrupt::free(|_| {
        ISR_MIN.store(u32::MAX, Ordering::Relaxed);
        ISR_MAX.store(0, Ordering::Relaxed);
    });
}

/// A snapshot of the playback position, taken with interrupts off:
/// (refills, stream NDTR, DAC_DOR1, DAC_DOR2). NDTR counts the frames left
/// before the wrap (512 down to 1): the DMA reads half 0 while it is above
/// [`HALF`].
pub fn position() -> (u32, u16, u16, u16) {
    cortex_m::interrupt::free(|_| {
        let r = REFILLS.load(Ordering::Relaxed);
        let n = dma().st[STREAM].ndtr.read().bits() as u16;
        let d1 = dac().dor1.read().bits() as u16;
        let d2 = dac().dor2.read().bits() as u16;
        (r, n, d1, d2)
    })
}

/// The smoothstep of the ramp: 0 at `t` = 0 to 65,536 at `t` =
/// [`RAMP_FRAMES`], 3s² - 2s³ with s = t / RAMP_FRAMES, in integers
/// (t²(3R - 2t) is at most 2^41, times 2^16 over R³ = 2^39: a shift).
fn smooth(t: u32) -> u32 {
    let t = u64::from(t.min(RAMP_FRAMES));
    let r = u64::from(RAMP_FRAMES);
    ((t * t * (3 * r - 2 * t)) >> (3 * RAMP_SHIFT - 16)) as u32
}

/// `v` scaled by `s` / 65,536.
fn scale(v: u32, s: u32) -> u16 {
    ((v * s) >> 16) as u16
}

/// Fills `out` with the next frames of the ramp from `from` (per channel)
/// towards `to`; returns whether the ramp is complete. `RAMP_POS` holds
/// the frames done.
fn ramp(out: &mut [u32], from: u32, to: u32) -> bool {
    let (fl, fr) = (from & 0xFFF, from >> 16 & 0xFFF);
    let (tl, tr) = (to & 0xFFF, to >> 16 & 0xFFF);
    let mut pos = RAMP_POS.load(Ordering::Relaxed);
    for f in out.iter_mut() {
        // Frame k of the ramp gets smooth(k + 1): the last is exactly `to`.
        pos = (pos + 1).min(RAMP_FRAMES);
        let s = smooth(pos);
        // from + (to - from) s, either direction, without signed maths.
        let ch = |a: u32, b: u32| {
            if b >= a {
                a as u16 + scale(b - a, s)
            } else {
                a as u16 - scale(a - b, s)
            }
        };
        *f = frame(ch(fl, tl), ch(fr, tr));
    }
    RAMP_POS.store(pos, Ordering::Relaxed);
    pos >= RAMP_FRAMES
}

fn bump(c: &AtomicU32) {
    c.store(c.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

/// Counts stream 0's error flags in `flags` (from LISR) and the DAC's
/// underrun flags, which it clears (DAC_SR).
fn count_errors(flags: u32) {
    if flags & ISR_TEIF != 0 {
        bump(&TE);
    }
    if flags & ISR_DMEIF != 0 {
        bump(&DME);
    }
    if flags & ISR_FEIF != 0 {
        bump(&FE);
    }
    let sr = dac().sr.read().bits() & (SR_DMAUDR1 | SR_DMAUDR2);
    if sr != 0 {
        // SAFETY: DAC_SR's DMAUDRx bits are write-1-to-clear.
        dac().sr.write(|w| unsafe { w.bits(sr) });
        if sr & SR_DMAUDR1 != 0 {
            bump(&UDR1);
        }
        if sr & SR_DMAUDR2 != 0 {
            bump(&UDR2);
        }
    }
}

/// The DMA1 stream 0 interrupt with a source of board-independent frames:
/// [`on_dma_codes`], with `fill` writing [`HALF`]
/// `gbc_firmware::audio::PcmFrame::to_bits` words into a scratch half on
/// the ISR's stack (DTCM, 1 KiB), which are then converted into the DMA
/// half as [`frame`]s (module docs, "Samples in, DAC words out"). The
/// source may read what it wrote (the feed hash, the PC tee) before it
/// returns; the DMA only ever sees DAC words.
///
/// The scratch keeps the ISR's cost where it was (ADR 0044): the ring's
/// pop, the hash and the tee now touch DTCM instead of the non-cacheable
/// DMA buffer (before, the hash and the tee read the buffer back), and the
/// buffer still gets exactly one store per frame, now from the
/// conversion (about 8 instructions a frame).
#[inline(always)]
pub fn on_dma<F: FnOnce(usize, &mut [u32])>(fill: F) {
    on_dma_codes(|h, out| {
        let mut pcm = [0u32; HALF];
        fill(h, &mut pcm);
        for (o, &p) in out.iter_mut().zip(&pcm) {
            *o = gbc_firmware::audio::u12_lanes(p);
        }
    });
}

/// The DMA1 stream 0 interrupt: counts and clears the flags, then fills
/// the half the DMA is not reading: from the start ramp, from `fill`
/// (`fill(half, frames)`: half 0 or 1, [`HALF`] frames of [`frame`]s, DAC
/// words), from the stop ramp or with zeros, by phase (module docs).
///
/// The free half is chosen from NDTR, not from which flag is set, so a
/// late interrupt (both flags) still refills the half the DMA has left.
///
/// An interrupt entered about a half late, with the next HT/TC landing
/// between its LISR read and its NDTR read, sees one flag (HT: half 0 is
/// free) but NDTR says the DMA has moved on (it reads half 0, half 1 is
/// free): the half its flag named was missed. It refills the free half
/// and counts [`Stats::late`]; the second flag's interrupt, already
/// pending, then finds the same half free and is skipped: refilling it
/// again would pop [`HALF`] frames over the unplayed first refill
/// (M4 review P-3; before, neither was counted). On time, the flag and
/// NDTR always agree (the latency is microseconds, a half 7.8 ms).
pub fn on_dma_codes<F: FnOnce(usize, &mut [u32])>(fill: F) {
    let t0 = cycles();
    let d = dma();
    let flags = d.lisr.read().bits() & ISR_ALL;
    // SAFETY: LIFCR is write-1-to-clear; only stream 0's bits.
    d.lifcr.write(|w| unsafe { w.bits(flags) });
    count_errors(flags);
    let ht_tc = flags & (ISR_HTIF | ISR_TCIF);
    if ht_tc == 0 {
        bump(&SPURIOUS);
        cortex_m::asm::dsb();
        return;
    }
    if ht_tc == ISR_HTIF | ISR_TCIF {
        bump(&LATE);
    }
    let ndtr = d.st[STREAM].ndtr.read().bits() as usize;
    // NDTR in 1..=HALF: the DMA reads half 1, half 0 is free. Above, or 0
    // (the reload instant): it reads half 0.
    let h = if ndtr != 0 && ndtr <= HALF { 0 } else { 1 };
    let skip = SKIP_HALF.swap(NO_HALF, Ordering::Relaxed);
    // Not once STOPPED: `halt` clears the stream's EN, which sets TCIF
    // (a disabled stream ends its transfer, RM0433 15.3.19) and enters
    // here once more before the interrupt is masked, with NDTR wherever
    // the DMA stopped (measured: 512 and 256); that refill of zeros is
    // harmless and no refill was missed.
    if ht_tc != ISR_HTIF | ISR_TCIF && PHASE.load(Ordering::Relaxed) != STOPPED {
        let named = if ht_tc == ISR_HTIF { 0 } else { 1 };
        if skip == h as u8 {
            // The second interrupt of a late pair: its half is filled.
            LAST_AT.store(t0, Ordering::Relaxed);
            cortex_m::asm::dsb();
            return;
        }
        if named != h {
            bump(&LATE);
            SKIP_HALF.store(h as u8, Ordering::Relaxed);
        }
    }
    // SAFETY: the DMA reads the other half (NDTR above); only this ISR
    // writes the buffer while the stream runs.
    let out = unsafe { half_mut(h) };
    match PHASE.load(Ordering::Relaxed) {
        RAMP_UP => {
            if ramp(out, 0, SILENCE) {
                PHASE.store(PLAY, Ordering::Relaxed);
            }
        }
        PLAY if STOP_REQ.load(Ordering::Acquire) => {
            RAMP_POS.store(0, Ordering::Relaxed);
            PHASE.store(RAMP_DOWN, Ordering::Relaxed);
            if ramp(out, LAST_FRAME.load(Ordering::Relaxed), 0) {
                PHASE.store(DRAIN, Ordering::Relaxed);
            }
        }
        PLAY => {
            fill(h, out);
            LAST_FRAME.store(out[HALF - 1], Ordering::Relaxed);
        }
        RAMP_DOWN => {
            if ramp(out, LAST_FRAME.load(Ordering::Relaxed), 0) {
                PHASE.store(DRAIN, Ordering::Relaxed);
            }
        }
        _ => {
            // DRAIN: zeros. The first refill of zeros goes in while the
            // DMA plays the ramp's last half, the second while it plays
            // the first zeros; at the third, a whole half of zeros has
            // played (DOR lags the DMA by a frame or two, so stopping at
            // the second could freeze the ramp's last code): STOPPED.
            out.fill(0);
            let n = DRAINED.load(Ordering::Relaxed) + 1;
            DRAINED.store(n, Ordering::Relaxed);
            if n >= 3 {
                PHASE.store(STOPPED, Ordering::Release);
            }
        }
    }
    // The stores (non-cacheable, maybe still in the write buffer) are done
    // long before the DMA reaches this half (7.8 ms); the DSB also keeps
    // the LIFCR clear from racing the exception return.
    cortex_m::asm::dsb();
    bump(&REFILLS);
    LAST_AT.store(t0, Ordering::Relaxed);
    let c = cycles().wrapping_sub(t0);
    if c < ISR_MIN.load(Ordering::Relaxed) {
        ISR_MIN.store(c, Ordering::Relaxed);
    }
    if c > ISR_MAX.load(Ordering::Relaxed) {
        ISR_MAX.store(c, Ordering::Relaxed);
    }
}

/// The stop ramp did not finish: the stream had stalled (an underrun or a
/// transfer error); [`AudioDac::stop`] shut it down anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StopTimeout {
    /// The phase it was in (1 ramp up, 2 play, 3 ramp down, 4 drain).
    pub phase: u8,
    pub ndtr: u16,
    pub lisr: u32,
    pub dac_sr: u32,
}

/// DAC1, TIM6 and DMA1 stream 0 (module docs). One per program.
pub struct AudioDac {
    _private: (),
}

impl AudioDac {
    /// Clocks, PA4/PA5 analog, the buffer's MPU region, DMAMUX1, DAC1 (both
    /// channels enabled at code 0, triggered by TIM6, DMA not yet enabled)
    /// and TIM6 (configured, stopped). Panics on a second call.
    // No `Default`: a singleton that panics on a second call (as `LcdDma`).
    #[allow(clippy::new_without_default)]
    pub fn new() -> AudioDac {
        static TAKEN: AtomicBool = AtomicBool::new(false);
        assert!(
            !TAKEN.swap(true, Ordering::AcqRel),
            "AudioDac::new called twice"
        );
        mpu::dma_buffer_window(mpu::REGION_AUDIO_BUF, buffer_addr(), BUFFER_BYTES);
        // SAFETY: RCC enables by read-modify-write from the main thread,
        // as the other drivers do; no interrupt touches RCC.
        let rcc = unsafe { &*pac::RCC::ptr() };
        // SAFETY: setting enable bits; the other bits kept.
        unsafe {
            rcc.ahb1enr
                .modify(|r, w| w.bits(r.bits() | RCC_AHB1_DMA1EN));
            rcc.ahb4enr
                .modify(|r, w| w.bits(r.bits() | RCC_AHB4_GPIOAEN));
            rcc.apb1lenr
                .modify(|r, w| w.bits(r.bits() | RCC_APB1L_TIM6EN | RCC_APB1L_DAC12EN));
        }
        let _ = rcc.ahb1enr.read();
        let _ = rcc.ahb4enr.read();
        let _ = rcc.apb1lenr.read();

        // PA4, PA5: analog (MODER 11, no pull; RM0433 26.4.2 and 11.3.13:
        // the DAC output needs the pin in analog mode). Analog is also the
        // reset state of both; written anyway.
        // SAFETY: GPIOA is shared (joystick.rs read-modify-writes PA0's
        // MODER/PUPDR fields), but both run only from the main thread during
        // init and no interrupt handler touches GPIOA (the ISRs here are the
        // DAC DMA and the LCD TE EXTI), so the read-modify-writes cannot
        // interleave; this one changes pins 4 and 5's fields only.
        let gpioa = unsafe { &*pac::GPIOA::ptr() };
        // SAFETY: as above; 0b00 = no pull, 0b11 = analog.
        unsafe {
            gpioa.pupdr.modify(|r, w| w.bits(r.bits() & !(0b1111 << 8)));
            gpioa.moder.modify(|r, w| w.bits(r.bits() | 0b1111 << 8));
        }

        // A stream left running by an earlier image (a debugger reset may
        // not reset DMA1): disable it and wait (RM0433 15.3.19 step 1).
        let st = &dma().st[STREAM];
        // SAFETY: EN cleared, other bits kept.
        st.cr.modify(|r, w| unsafe { w.bits(r.bits() & !SCR_EN) });
        let mut n = 0u32;
        while st.cr.read().bits() & SCR_EN != 0 && n < 1_000_000 {
            n += 1;
        }
        // SAFETY: stream 0's write-1-to-clear flags.
        dma().lifcr.write(|w| unsafe { w.bits(ISR_ALL) });
        // DMAMUX1 channel 0 (to DMA1 stream 0): request dac_ch1_dma, no
        // synchronisation, no event (SE = 0, SPOL = 00: ES0392 2.5.4 does
        // not apply).
        // SAFETY: DMAMUX1's clock is DMA1EN's (RM0433 8.7.41); channel 0
        // belongs to this module; DMAREQ_ID 67 is a documented request.
        unsafe {
            (*pac::DMAMUX1::ptr()).ccr[STREAM].write(|w| w.bits(DMAMUX_REQ_DAC_CH1));
        }

        let dac = dac();
        // SAFETY: the DAC is (re)configured with both channels disabled:
        // TSELx and MODEx can only be written then (26.7.1, 26.7.16).
        unsafe {
            dac.cr.write(|w| w.bits(0));
            // ES0392 2.10.1: one data register write before DAC_MCR.
            // Code 0 on both: where the start ramp begins.
            dac.dhr12rd.write(|w| w.bits(frame(0, 0)));
            dac.mcr.write(|w| w.bits(DAC_MCR));
            dac.sr.write(|w| w.bits(SR_DMAUDR1 | SR_DMAUDR2));
            // Triggers first, then the enables (TSEL is fixed once ENx
            // is set).
            dac.cr
                .write(|w| w.bits(DAC_CR & !(CR_EN1 | CR_EN1 << CR_CH2_SHIFT)));
            dac.cr.write(|w| w.bits(DAC_CR));
        }
        // tWAKEUP: at most 7.5 us with the buffer on (DS12117 Table 176).
        crate::wait_since(cycles(), crate::CPU_HZ / 100_000);

        // TIM6: TRGO on update, ARR preloaded, one update per TIM_DIV
        // clocks; UG loads PSC and ARR (and makes one TRGO, harmless: the
        // DAC takes triggers since TEN, and DHR = DOR = 0 already).
        let t = tim();
        // SAFETY: documented values (MMS 010; any 16-bit PSC/ARR; UG).
        unsafe {
            t.cr1.write(|w| w.bits(TIM_CR1_ARPE));
            t.cr2.write(|w| w.bits(TIM_CR2));
            t.psc.write(|w| w.bits(0));
            t.arr.write(|w| w.bits(TIM_DIV - 1));
            t.egr.write(|w| w.bits(TIM_EGR_UG));
            t.sr.write(|w| w.bits(0));
        }
        PHASE.store(IDLE, Ordering::Release);
        AudioDac { _private: () }
    }

    /// Starts playback: both halves filled with the start ramp, the
    /// stream enabled, DMAEN1 set, the interrupt unmasked, TIM6 started.
    /// `on_dma` calls the source once the ramp is done. No effect while
    /// already playing.
    pub fn start(&mut self) {
        let p = PHASE.load(Ordering::Acquire);
        if p != IDLE && p != STOPPED {
            return;
        }
        STOP_REQ.store(false, Ordering::Relaxed);
        RAMP_POS.store(0, Ordering::Relaxed);
        DRAINED.store(0, Ordering::Relaxed);
        LAST_FRAME.store(SILENCE, Ordering::Relaxed);
        // The stream is disabled (IDLE/STOPPED) and the interrupt masked
        // (`new`, or `stop`), so the whole buffer is ours.
        // SAFETY: see above; one half at a time.
        let done = ramp(unsafe { half_mut(0) }, 0, SILENCE);
        // SAFETY: as above.
        let done = done | ramp(unsafe { half_mut(1) }, 0, SILENCE);
        PHASE.store(if done { PLAY } else { RAMP_UP }, Ordering::Relaxed);
        arm(frame(0, 0));
    }

    /// Whether playback is running (from `start` until `stop` returns).
    pub fn running(&self) -> bool {
        !matches!(PHASE.load(Ordering::Acquire), IDLE)
    }

    /// The phase: 0 idle, 1 start ramp, 2 playing (the source is called),
    /// 3 stop ramp, 4 draining zeros, 5 stopped (until `stop` returns).
    pub fn phase(&self) -> u8 {
        PHASE.load(Ordering::Acquire)
    }

    /// Ramps down to code 0 and stops (module docs): waits for the ramp
    /// and a whole half of zeros (about 280 ms), then stops TIM6, the stream and
    /// DMAEN1, and masks the interrupt. The DAC stays enabled at code 0.
    /// If the stream had stalled, gives up after twice that and shuts down
    /// anyway. No effect when idle.
    pub fn stop(&mut self) -> Result<(), StopTimeout> {
        if PHASE.load(Ordering::Acquire) == IDLE {
            return Ok(());
        }
        STOP_REQ.store(true, Ordering::Release);
        // Ramp + 3 halves, twice, at the nominal rate.
        let frames = u64::from(RAMP_FRAMES) + 3 * HALF as u64;
        let timeout = (2 * frames * u64::from(crate::CPU_HZ) / u64::from(TARGET_HZ)) as u32;
        let t0 = cycles();
        let mut result = Ok(());
        while PHASE.load(Ordering::Acquire) != STOPPED {
            if cycles().wrapping_sub(t0) > timeout {
                result = Err(StopTimeout {
                    phase: PHASE.load(Ordering::Relaxed),
                    ndtr: dma().st[STREAM].ndtr.read().bits() as u16,
                    lisr: dma().lisr.read().bits() & ISR_ALL,
                    dac_sr: dac().sr.read().bits(),
                });
                break;
            }
        }
        halt();
        PHASE.store(IDLE, Ordering::Release);
        result
    }

    /// The configuration as it reads now, for the report: DAC CR, MCR, SR,
    /// CCR (the factory offset trims); TIM6 CR1, CR2, PSC, ARR; DMAMUX1
    /// C0CR; stream 0 CR, NDTR, PAR, M0AR, FCR.
    pub fn registers(&self) -> [u32; 14] {
        let (dac, t, st) = (dac(), tim(), &dma().st[STREAM]);
        // SAFETY: a read of DMAMUX1 channel 0's register.
        let mux = unsafe { (*pac::DMAMUX1::ptr()).ccr[STREAM].read().bits() };
        [
            dac.cr.read().bits(),
            dac.mcr.read().bits(),
            dac.sr.read().bits(),
            dac.ccr.read().bits(),
            t.cr1.read().bits(),
            t.cr2.read().bits(),
            t.psc.read().bits(),
            t.arr.read().bits(),
            mux,
            st.cr.read().bits(),
            st.ndtr.read().bits(),
            st.par.read().bits(),
            st.m0ar.read().bits(),
            st.fcr.read().bits(),
        ]
    }

    /// The stall check (M4 review P-2, module docs "Errors"), for the main
    /// loop to call while it waits (each wake): while playing (start ramp
    /// or source), if no refill came for more than `max_gap` CPU cycles
    /// (below the cycle counter's 9.1 s wrap), counts the flags the ISR
    /// never saw (DAC_SR's DMAUDRx, the stream's errors), then shuts the
    /// path down as [`AudioDac::stop`] does and re-initialises it (26.4.8:
    /// "clear the DMAUDRx flag ..., clear the DMAEN bit of the used DMA
    /// stream and re-initialize both DMA and DAC channel x"), both halves
    /// holding the output's current level (no click; the first refill
    /// comes a half later), and counts a restart in [`Stats::restarts`].
    /// Returns whether it restarted. [`STALL_CYCLES`] is the usual bound.
    pub fn watchdog(&mut self, max_gap: u32) -> bool {
        if !matches!(PHASE.load(Ordering::Acquire), RAMP_UP | PLAY) {
            return false;
        }
        // LAST_AT first, then the counter (M7 review): called with the
        // interrupt enabled, so `on_dma` can store a newer LAST_AT (its
        // entry `t0`) between the two reads. Read the other way round,
        // `now` would be older than that `t0`, the difference would wrap
        // to ~4.29e9 and a healthy stream would be restarted as a stall.
        // In this order the ISR's `t0` was read before our load and our
        // `now` after it (one core, so program order of the interrupt),
        // giving `now - last` >= 0. Acquire keeps the compiler and CPU
        // from moving the counter's volatile read before the load; the
        // ISR's Relaxed store needs no Release (same core, nothing else
        // published with it).
        let last = LAST_AT.load(Ordering::Acquire);
        if cycles().wrapping_sub(last) <= max_gap {
            return false;
        }
        // No refill interrupt from here on: the buffer and the stream are
        // the main thread's.
        cortex_m::peripheral::NVIC::mask(pac::Interrupt::DMA1_STR0);
        count_errors(dma().lisr.read().bits() & ISR_ALL);
        halt();
        let level = frame(
            dac().dor1.read().bits() as u16,
            dac().dor2.read().bits() as u16,
        );
        for h in 0..2 {
            // SAFETY: the stream is disabled and the interrupt masked.
            unsafe { half_mut(h) }.fill(level);
        }
        arm(level);
        bump(&RESTARTS);
        true
    }

    /// The buffer's address (AXI SRAM) and size in bytes.
    pub fn buffer(&self) -> (usize, usize) {
        (buffer_addr(), BUFFER_BYTES)
    }
}

/// No refill for this long (50 ms, six refill periods of 7.8 ms) is a
/// stall for [`AudioDac::watchdog`]; a refill interrupt takes at most
/// about 8,300 cycles (17 us, `isr_max` of a paced run).
pub const STALL_CYCLES: u32 = crate::CPU_HZ / 20;

/// Stops TIM6, then the stream (waiting for it), then the DAC's DMA
/// request, and masks and unpends the interrupt; the DAC stays enabled.
fn halt() {
    // Triggers off first, then the stream (and wait for it), only then
    // the DAC's request (RM0433 15.3.19 warning).
    let t = tim();
    // SAFETY: CEN cleared (ARPE kept).
    t.cr1.write(|w| unsafe { w.bits(TIM_CR1_ARPE) });
    let st = &dma().st[STREAM];
    // SAFETY: EN cleared, other bits kept.
    st.cr.modify(|r, w| unsafe { w.bits(r.bits() & !SCR_EN) });
    let mut n = 0u32;
    while st.cr.read().bits() & SCR_EN != 0 && n < 1_000_000 {
        n += 1;
    }
    cortex_m::peripheral::NVIC::mask(pac::Interrupt::DMA1_STR0);
    // SAFETY: DMAEN1 cleared (DAC_CR without it), stream flags and the
    // NVIC pending bit cleared: no stale interrupt on the next start.
    unsafe {
        dac().cr.write(|w| w.bits(DAC_CR));
        dma().lifcr.write(|w| w.bits(ISR_ALL));
    }
    cortex_m::peripheral::NVIC::unpend(pac::Interrupt::DMA1_STR0);
}

/// Starts the path with the buffer as it is: DHR = `first`, the stream
/// configured and enabled, DMAEN1, the interrupt unmasked, TIM6 started.
/// The refill clock ([`Stats::last_at`]) starts now, for
/// [`AudioDac::watchdog`].
fn arm(first: u32) {
    LAST_AT.store(cycles(), Ordering::Relaxed);
    SKIP_HALF.store(NO_HALF, Ordering::Relaxed);
    let dac = dac();
    // The first trigger moves DHR (`first`, as DOR) out; its request
    // then brings frame 0 (26.4.8: DHR must hold data before the
    // first trigger).
    // SAFETY: a data write; the DMA is not running.
    dac.dhr12rd.write(|w| unsafe { w.bits(first) });
    let st = &dma().st[STREAM];
    let dhr12rd = &dac.dhr12rd as *const _ as u32;
    // SAFETY: the stream is disabled (`new` or `halt` waited for EN =
    // 0), so its registers accept the configuration (15.3.19): the
    // DAC's dual data register, the buffer, FRAMES words, direct mode,
    // DMA_SCR; flags cleared before EN (15.4 note).
    unsafe {
        dma().lifcr.write(|w| w.bits(ISR_ALL));
        st.par.write(|w| w.bits(dhr12rd));
        st.m0ar.write(|w| w.bits(buffer_addr() as u32));
        st.ndtr.write(|w| w.bits(FRAMES as u32));
        st.fcr.write(|w| w.bits(DMA_SFCR));
        st.cr.write(|w| w.bits(DMA_SCR));
        st.cr.write(|w| w.bits(DMA_SCR | SCR_EN));
    }
    // SAFETY: underrun flags cleared (write 1), then DMAEN1 set on top
    // of DAC_CR (TSEL unchanged while enabled).
    unsafe {
        dac.sr.write(|w| w.bits(SR_DMAUDR1 | SR_DMAUDR2));
        dac.cr.write(|w| w.bits(DAC_CR | CR_DMAEN1));
    }
    cortex_m::asm::dsb();
    // SAFETY: the image defines DMA1_STR0 (module docs; without it the
    // lib's DefaultHandler reports it and stops); `on_dma` touches only
    // this module's atomics, stream 0's flags and NDTR, DAC_SR and the
    // free half of the buffer.
    unsafe { cortex_m::peripheral::NVIC::unmask(pac::Interrupt::DMA1_STR0) };
    let t = tim();
    // SAFETY: CNT reset, then CEN (ARPE kept).
    unsafe {
        t.cnt.write(|w| w.bits(0));
        t.cr1.write(|w| w.bits(TIM_CR1_ARPE | TIM_CR1_CEN));
    }
}
