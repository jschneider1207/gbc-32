//! The memory bus: everything the CPU reaches ([`Sys`]), the per-run-call
//! context the CPU drives ([`Ctx`]), the memory map and the I/O registers.
//! Design: ADR 0008.
//!
//! Each CPU bus call is one M-cycle: the access happens at the start of the
//! cycle (it sees the state as of the current clock), then [`Ctx::tick`]
//! advances the clock by one M-cycle and runs scheduled events (the TIMA
//! reload, a serial bit, the PPU's next sync) when the clock reaches the
//! earliest deadline in `Sys::sched` (the event table, ADR 0011). Nothing
//! else runs per M-cycle: the timer and the serial bit clock are computed
//! from the clock when their registers are accessed ([`crate::timer`]),
//! and the PPU is otherwise caught up only when one of its registers is
//! written (ADR 0002).
//!
//! The per-M-cycle state ([`Hot`]: the clock countdown) is copied into the
//! [`Ctx`] for the duration of a run call, so the tick reaches it through
//! the one context pointer the CPU holds instead of through `Ctx::sys`
//! (ADR 0008, amendments).

use crate::apu::{self, StereoSample};
use crate::cart::Cart;
use crate::config::Config;
use crate::cpu::{Bus, INT_MASK, StopKind};
use crate::dma::{self, Conflict, OamDma};
use crate::hdma::{self, VramDma};
use crate::joypad::{InputQueue, p1_interrupt, p1_lines};
use crate::memory::Memory;
use crate::ppu::{self, Frame, Ppu, View, edges};
use crate::rom::Rom;
use crate::schedule::{EventKind, NEVER, Schedule};
use crate::serial::{INT_SERIAL, Serial};
use crate::timer::Timer;
use crate::{Buttons, Emulator, Events, RunResult};

/// IF bits 5-7 don't exist (they read 1). Bit 5: a delayed LYC-write STAT
/// interrupt is waiting ([`Ctx::lyc_irq_delay`]); bit 6: the parity of
/// the M-cycle (clock bit 2) of the write that requested it.
const LYC_IRQ_PENDING: u8 = 0x20;
const LYC_IRQ_PHASE: u8 = 0x40;

/// Everything the bus reaches except the RAM arrays (in [`Memory`]) and the
/// CPU itself. Lives in `Hw`, so it counts toward the 1 KiB limit there.
#[repr(C)]
pub(crate) struct Sys<'a> {
    // Fields the inlined CPU bus path reads (`read`, `write_mapped`, the
    // interrupt checks) come first, so their offsets stay small: Thumb-2
    // has 16-bit loads only for small offsets (ADR 0011 step 1 measured
    // +1 KB in `Cpu::run` with the event table at the front).
    pub(crate) cart: Cart<'a>,
    /// Offsets of the VRAM bank at 8000 and the WRAM bank at D000.
    vram_off: usize,
    wram_off: usize,
    /// IE (FFFF), all 8 bits.
    pub(crate) ie: u8,
    /// IF (FF0F), low 5 bits.
    pub(crate) iflags: u8,
    /// The cartridge runs in CGB mode (header 0143 bit 7); otherwise DMG
    /// compatibility mode, where the CGB-only registers are locked (KEY0 =
    /// 04; Pan Docs: Power Up Sequence; SameBoy `cgb_mode`).
    pub(crate) cgb_mode: bool,
    /// VBK (bit 0) and SVBK (bits 0-2) as written.
    pub(crate) vbk: u8,
    pub(crate) svbk: u8,
    /// KEY1 bit 0 (speed switch armed).
    pub(crate) key1: u8,
    /// P1/JOYP select bits (4-5) as written.
    pub(crate) joyp_select: u8,
    pub(crate) buttons: Buttons,
    pub(crate) config: Config,
    pub(crate) serial: Serial,
    pub(crate) ppu: Ppu,
    /// DIV/TIMA/TMA/TAC and the system counter (off the bus hot path).
    pub(crate) timer: Timer,
    /// The clock between run calls. During a run call the live
    /// copy is [`Ctx::hot`] (this one is stale until the `Ctx` is dropped),
    /// so code running inside a call must use that one.
    pub(crate) hot: Hot,
    /// The event table: when each kind of scheduled work must next run
    /// ([`Ctx::run_events`]).
    pub(crate) sched: Schedule,
    /// Scheduled button changes; the earliest is the `Input` deadline
    /// (ADR 0012).
    pub(crate) inputs: InputQueue,
    /// CGB VRAM DMA (FF51-FF55, ADR 0017).
    pub(crate) vdma: VramDma,
}

impl Sys<'_> {
    #[inline(never)]
    pub(crate) const fn new() -> Self {
        Self {
            hot: Hot::new(),
            sched: Schedule::new(),
            inputs: InputQueue::new(),
            vdma: VramDma::new(),
            ppu: Ppu::new(),
            timer: Timer::new(),
            serial: Serial::new(),
            cart: Cart::empty(),
            config: Config {
                dmg_palettes: crate::DmgPalettes::GRAYSCALE,
            },
            buttons: Buttons::EMPTY,
            ie: 0,
            iflags: 0,
            joyp_select: 0,
            cgb_mode: false,
            vbk: 0,
            svbk: 0,
            vram_off: 0,
            wram_off: 0x1000,
            key1: 0,
        }
    }

    /// Sets the held buttons and the P1 select bits (4-5), requesting the
    /// joypad interrupt if a P10-P13 line goes low ([`p1_interrupt`]).
    pub(crate) fn set_joypad(&mut self, buttons: Buttons, select: u8) {
        let old = p1_lines(self.buttons, self.joyp_select);
        self.buttons = buttons;
        self.joyp_select = select & 0x30;
        self.iflags |= p1_interrupt(old, p1_lines(self.buttons, self.joyp_select));
    }

    /// Sets the `Input` deadline for the earliest scheduled button change
    /// at the current M-cycle length `mcycle` (ADR 0012). The caller
    /// reschedules the countdown if it is running.
    pub(crate) fn schedule_inputs(&mut self, mcycle: u64) {
        self.sched
            .set(EventKind::Input, self.inputs.deadline(mcycle));
    }

    /// Sets the `DivApu` deadline at clock `now`: the next falling edge of
    /// the system counter's DIV-APU bit while the APU is `powered`, else
    /// [`NEVER`] (ADR 0021 section 4). Called wherever the counter is
    /// rebased or the APU powers on. The caller reschedules. Out of line:
    /// off `speed_switch` and the event handlers (code-size budgets).
    #[inline(never)]
    pub(crate) fn set_div_apu_deadline(&mut self, powered: bool, now: u64) {
        let at = if powered {
            let k = self.timer.div_apu_bit() + 1;
            // Bit 13 (double speed) with the delay armed: 2 clocks late.
            let late = (u64::from(self.timer.apu_ds_delay) & u64::from(k - 13)) << 1;
            self.timer.clock_at(((self.timer.sc(now) >> k) + 1) << k) + late
        } else {
            NEVER
        };
        self.sched.set(EventKind::DivApu, at);
    }

    /// Power-on state after the boot ROM, for the inserted cartridge. The
    /// cartridge RAM, RTC, config and held buttons are kept.
    pub(crate) fn reset(&mut self, mem: &mut Memory) {
        let cart = core::mem::replace(&mut self.cart, Cart::empty());
        let (config, buttons) = (self.config, self.buttons);
        *self = Self::new();
        self.cart = cart;
        self.cart.reset();
        self.config = config;
        self.buttons = buttons;
        self.cgb_mode = self.cart.rom.get(0x143).is_some_and(|f| f & 0x80 != 0);
        self.timer = Timer::post_boot(self.cgb_mode);
        // P1 select bits: in DMG compatibility mode the boot ROM reads the
        // joypad (palette selection) and leaves both select lines high,
        // so P1 reads FF (Mooneye `misc/boot_hwio-C`, verified on CGB). In
        // CGB mode it doesn't touch P1: CF with nothing held (Pan Docs:
        // Power Up Sequence, "Hardware registers", CGB P1 = C7 or CF).
        if !self.cgb_mode {
            self.joyp_select = 0x30;
        }
        self.ppu.reset(self.cgb_mode);
        self.sched.set(EventKind::Ppu, self.ppu.next_event(false));
        // IF = E1: the boot ROM leaves VBlank requested (Pan Docs: Power
        // Up Sequence, "Hardware registers").
        self.iflags = 0x01;
        // SC reads 7F in CGB mode and 7E in DMG compatibility mode (Pan
        // Docs: Power Up Sequence, "Hardware registers": CGB/AGB 7F;
        // Mooneye `misc/boot_hwio-C`, a DMG-mode ROM verified on CGB,
        // expects 7E). In CGB mode bits 0-1 are readable, so both are set.
        if self.cgb_mode {
            self.serial.sc = 0x03;
        }
        for &(addr, value) in POST_BOOT_IO {
            mem.io[usize::from(addr & 0x7F)] = value;
        }
        boot_logo(&mut mem.vram, self.cart.rom);
        // NR52 = F1 is APU state (ADR 0021 section 7).
        mem.apu.post_boot();
        self.set_div_apu_deadline(true, 0);
        // FF55 reads FF: no VRAM DMA active (`VramDma::read_hdma5`).
        mem.io[0x55] = 0xFF;
        // CGB mode: the boot ROM fades all 8 BG palettes to white (7FFF)
        // before starting the game (SameBoy BootROMs/cgb_boot.asm,
        // `Preboot`: 32 brightening steps over hBgPalettes, all 64 bytes).
        // OBJ palette RAM is not written (left at zero here; random on
        // hardware). DMG compatibility mode: palette RAM is unreadable
        // (BCPD/OCPD read FF), so its contents don't matter.
        if self.cgb_mode {
            for c in mem.bg_pal.as_chunks_mut::<2>().0 {
                *c = 0x7FFFu16.to_le_bytes();
            }
        }
        self.ppu
            .resolve_palettes(mem, &self.config.dmg_palettes, self.cgb_mode);
        // OPRI bit 0: the boot ROM selects coordinate priority (1) for
        // DMG compatibility mode, and leaves 0 (OAM index priority) in CGB
        // mode (Pan Docs: FF6C - OPRI; SameBoy Core/memory.c, OPRI write
        // allowed while the boot ROM runs). Mooneye `boot_hwio-C` expects
        // FF6C = FF in DMG compatibility mode.
        mem.io[0x6C] = u8::from(!self.cgb_mode);
        // BCPS = 88 and OCPS = 90 (auto-increment, index 8 and 16) in DMG
        // compatibility mode: where the boot ROM's palette setup leaves
        // them (Mooneye `misc/boot_hwio-C` reads C8 and D0, bit 6 reading
        // 1). The CGB-mode values are unverified (no test); left at 0.
        if !self.cgb_mode {
            mem.io[0x68] = 0x88;
            mem.io[0x6A] = 0x90;
        }
    }
}

/// The tiles the boot ROM leaves in VRAM bank 0: the cartridge header's
/// logo (0104-0133) as tiles 1-24 and the (R) symbol as tile 25 (0x19),
/// bit plane 0 only. Each logo byte is two rows of 4 pixels; each pixel
/// is doubled in both directions (SameBoy BootROMs/cgb_boot.asm, MIT:
/// `DoubleBitsAndWriteRowTwice`, `ReadTrademarkSymbol`; "required for
/// DMG compatibility. This is done by the original CGB Boot ROM as
/// well"). Its final VRAM DMAs clear only tile map rows 5-14 and bank 1
/// tiles 0-64. Mealybug `m3_obp0_change`,
/// `m3_lcdc_obj_en_change` use tile 0x19 as an object (CGB C photos).
/// Cold: once per reset.
#[inline(never)]
fn boot_logo(vram: &mut [u8; 0x4000], rom: Rom<'_>) {
    // Tile 0x19, plane 1 zero. The glyph bytes come from SameBoy
    // BootROMs/cgb_boot.asm (MIT, Copyright (c) 2015-2025 Lior Halphon).
    const TRADEMARK: [u8; 16] = [
        0x3C, 0, 0x42, 0, 0xB9, 0, 0xA5, 0, 0xB9, 0, 0xA5, 0, 0x42, 0, 0x3C, 0,
    ];
    // A nibble with bit k doubled to bits 2k and 2k + 1.
    const DOUBLED: [u8; 16] = [
        0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC,
        0xFF,
    ];
    // Each nibble (a row of 4 pixels) is two rows of tile data.
    for i in 0..96 {
        let b = rom.get(0x104 + i / 2).unwrap_or(0);
        let d = DOUBLED[usize::from(if i & 1 == 0 { b >> 4 } else { b & 0x0F })];
        vram[0x10 + 4 * i] = d;
        vram[0x12 + 4 * i] = d;
    }
    vram[0x190..0x1A0].copy_from_slice(&TRADEMARK);
}

/// The index step of a BCPD/OCPD write in CGB mode: returns the palette RAM
/// byte the index register (BCPS/OCPS) selects in bits 0-5, then, if its
/// bit 7 (auto-increment) is set, increments bits 0-5, wrapping 3F to 00
/// and keeping bit 7 (Pan Docs: LCD Color Palettes, FF68 BCPS; SameBoy
/// `write_high_memory`, GB_IO_BGPD). It happens even when the PPU blocks
/// the write (Pan Docs: "even if the write fails due to CRAM being
/// inaccessible"; SameBoy `cgb_palettes_blocked`).
fn palette_index_step(index: &mut u8) -> usize {
    let i = usize::from(*index & 0x3F);
    if *index & 0x80 != 0 {
        *index = (*index & 0x80) | (index.wrapping_add(1) & 0x3F);
    }
    i
}

/// OR masks for reads of FF10-FF2F: write-only bits (lengths, frequency
/// low bytes, trigger) and unused bits and registers read 1 (Pan Docs:
/// Audio Registers; SameBoy Core/apu.c `read_mask`; Mooneye
/// `misc/bits/unused_hwio-C` and `misc/boot_hwio-C`).
const SOUND_READ_MASK: [u8; 0x20] = [
    0x80, 0x3F, 0x00, 0xFF, 0xBF, // NR10-NR14
    0xFF, 0x3F, 0x00, 0xFF, 0xBF, // FF15 (unused), NR21-NR24
    0x7F, 0xFF, 0x9F, 0xFF, 0xBF, // NR30-NR34
    0xFF, 0xFF, 0x00, 0x00, 0xBF, // FF1F (unused), NR41-NR44
    0x00, 0x00, 0x70, // NR50-NR52
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // FF27-FF2F (unused)
];

/// Post-boot values of the stub I/O registers kept in `Memory::io` (Pan
/// Docs: Power Up Sequence, "Hardware registers", CGB column).
const POST_BOOT_IO: &[(u16, u8)] = &[
    (0xFF10, 0x80),
    (0xFF11, 0xBF),
    (0xFF12, 0xF3),
    (0xFF13, 0xFF),
    (0xFF14, 0xBF),
    (0xFF16, 0x3F),
    (0xFF18, 0xFF),
    (0xFF19, 0xBF),
    (0xFF1A, 0x7F),
    (0xFF1B, 0xFF),
    (0xFF1C, 0x9F),
    (0xFF1D, 0xFF),
    (0xFF1E, 0xBF),
    (0xFF20, 0xFF),
    (0xFF23, 0xBF),
    (0xFF24, 0x77),
    (0xFF25, 0xF3),
    (0xFF47, 0xFC),
];

/// The state the bus touches on every M-cycle: the clock, as a countdown.
/// Kept by value in [`Ctx`] during a run call and in [`Sys::hot`] between
/// calls.
///
/// The clock is not counted directly. `budget` counts the base clocks
/// left until `deadline` (the earlier of `Sys::sched.next()` and the run
/// call's target), and the tick only subtracts the M-cycle length and
/// tests the sign; the u64 clock is `deadline - budget`, exact at every
/// point (ADR 0008, amendments).
#[derive(Clone, Copy)]
pub(crate) struct Hot {
    /// Base clocks until `deadline`; zero or below when it has passed (by
    /// less than one M-cycle).
    budget: i32,
    /// Base clocks per M-cycle: 4, or 2 in CGB double speed.
    pub(crate) mcycle: i32,
    /// The clock at which `budget` reaches 0. At most `i32::MAX` clocks
    /// ahead of the clock it was set at, so `budget` always fits.
    deadline: u64,
    /// FE00 while an OAM DMA transfer runs (from the FF46 write to its
    /// `Dma` deadline), else 0. CPU accesses dispatch on `addr | dma`, so
    /// during a transfer every address takes the E000-FFFF arm, the only
    /// one that tests the flag (`read_high`, `write_mapped`) and goes to
    /// the conflict path: one OR per access and no extra branch on the
    /// fast path (ADR 0011: conflicts in the access, not the tick).
    pub(crate) dma: u16,
}

impl Hot {
    pub(crate) const fn new() -> Self {
        Self {
            budget: 0,
            mcycle: 4,
            deadline: 0,
            dma: 0,
        }
    }

    /// Base clocks since the last load or reset.
    #[inline]
    pub(crate) fn clock(&self) -> u64 {
        self.deadline.wrapping_sub(self.budget as i64 as u64)
    }

    /// This clock with [`Self::dma`] clear: reads through it are
    /// conflict-free (`peek`, the DMA's own reads).
    #[inline]
    pub(crate) fn without_dma(&self) -> Self {
        Self { dma: 0, ..*self }
    }
}

/// The context of one run call: what the CPU's bus reaches, plus the
/// caller's buffers and the events raised so far. The CPU gets one pointer
/// to it, so a lazy PPU/APU catch-up on any bus access reaches the frame
/// and audio buffers without threading them through every call (ADR 0008).
///
/// Built with [`Ctx::new`]; dropping it stores [`Ctx::hot`] back into
/// [`Sys::hot`].
pub(crate) struct Ctx<'r, 'a> {
    /// The live per-M-cycle state, by value so the tick doesn't load
    /// `sys` (copied from and back to `Sys::hot`).
    pub(crate) hot: Hot,
    pub(crate) mem: &'r mut Memory,
    pub(crate) sys: &'r mut Sys<'a>,
    pub(crate) frame: &'r mut Frame,
    /// The caller's audio buffer and how much of it is written.
    pub(crate) audio: &'r mut [StereoSample],
    pub(crate) audio_len: usize,
    pub(crate) events: Events,
    /// Events that end the run call.
    pub(crate) stop_on: Events,
    /// The run call ends once the clock reaches this (`u64::MAX`: no
    /// target, or already reached).
    target: u64,
    /// The run call ends after the current instruction ([`Bus::run_done`]).
    pub(crate) done: bool,
}

impl Drop for Ctx<'_, '_> {
    fn drop(&mut self) {
        // Bring OAM up to the clock, so it is exact between run calls.
        if self.hot.dma != 0 {
            self.dma_sync(self.clock());
        }
        self.sys.hot = self.hot;
    }
}

impl<'r, 'a> Ctx<'r, 'a> {
    /// The context for a run call that ends when the clock reaches
    /// `target` or an event in `stop_on` is raised.
    pub(crate) fn new(
        mem: &'r mut Memory,
        sys: &'r mut Sys<'a>,
        frame: &'r mut Frame,
        audio: &'r mut [StereoSample],
        stop_on: Events,
        target: u64,
    ) -> Self {
        let mut ctx = Self {
            hot: sys.hot,
            mem,
            sys,
            frame,
            audio,
            audio_len: 0,
            events: Events::EMPTY,
            stop_on,
            target,
            done: false,
        };
        ctx.start_audio();
        ctx.reschedule();
        ctx
    }
}

impl Ctx<'_, '_> {
    /// Base clocks since the last load or reset.
    #[inline]
    pub(crate) fn clock(&self) -> u64 {
        self.hot.clock()
    }

    /// Advances everything but the CPU by one M-cycle: only the clock
    /// countdown. Everything else is an event (ADR 0011).
    #[inline(always)]
    fn tick(&mut self) {
        let h = &mut self.hot;
        h.budget -= h.mcycle;
        if h.budget <= 0 {
            self.boundary();
        }
    }

    /// A serial byte completed (`done`): the serial interrupt.
    fn serial_done(&mut self, done: bool) {
        if done {
            self.sys.iflags |= INT_SERIAL;
            self.raise(Events::SERIAL_TX);
        }
    }

    /// Sets the Serial deadline to the bit clock's next shift (the
    /// flip-flop's next 1 -> 0 edge, `serial` module docs) while a
    /// transfer runs. The caller reschedules ([`Self::schedule_serial`]).
    fn set_serial_deadline(&mut self) {
        let at = self.serial_deadline(self.clock());
        self.sys.sched.set(EventKind::Serial, at);
    }

    /// The Serial deadline at clock `now`: the bit clock's next shift
    /// while a transfer runs, else [`NEVER`].
    #[inline(always)]
    fn serial_deadline(&self, now: u64) -> u64 {
        let s = &*self.sys;
        match s.serial.next_shift(s.timer.sc(now)) {
            None => NEVER,
            Some(v) => s.timer.clock_at(v),
        }
    }

    /// [`Self::set_serial_deadline`], and reschedules.
    #[inline(never)]
    fn schedule_serial(&mut self) {
        self.set_serial_deadline();
        self.reschedule();
    }

    /// Sets the Timer deadline after a timer state change. The caller
    /// reschedules ([`Self::schedule_timer`]).
    fn set_timer_deadline(&mut self) {
        let s = &mut *self.sys;
        s.sched.set(EventKind::Timer, s.timer.deadline());
    }

    /// [`Self::set_timer_deadline`], and reschedules.
    fn schedule_timer(&mut self) {
        self.set_timer_deadline();
        self.reschedule();
    }

    /// Writes to SC, DIV, TIMA, TMA and TAC (by the low 7 address bits):
    /// they move the Timer and Serial deadlines. Out of line so the
    /// register logic stays off `write_high` (ADR 0011).
    #[inline(never)]
    fn write_timer_io(&mut self, reg: usize, value: u8) {
        let now = self.clock();
        let s = &mut *self.sys;
        match reg {
            0x02 => {
                let c = s.timer.sc(now);
                s.serial.write_sc(value, s.cgb_mode, c);
                self.schedule_serial();
                return;
            }
            0x04 => return self.div_reset(),
            0x05 => s.timer.write_tima(now, value),
            0x06 => s.timer.write_tma(now, value),
            _ => s.timer.write_tac(now, value),
        }
        self.schedule_timer();
    }

    /// A DIV write or STOP: every set counter bit falls (the TIMA glitch
    /// in [`Timer::write_div`], a serial flip-flop toggle in
    /// [`Serial::div_reset`]; Pan Docs FF04), then both deadlines move
    /// with the rebased counter.
    fn div_reset(&mut self) {
        let now = self.clock();
        let old = self.sys.timer.write_div(now);
        let new = self.sys.timer.sc(now);
        let done = self.sys.serial.div_reset(old, new);
        self.serial_done(done);
        self.apu_div_reset(old);
        self.set_timer_deadline();
        self.schedule_serial();
    }

    /// STOP's DIV reset: with nothing pending (STOP is two bytes), first
    /// the TIMA edge of AGE's later reset ([`Timer::stop_edge`], ADR 0020).
    /// Out of line: keeps `stop_begin` within its code-size budget.
    #[inline(never)]
    fn stop_div_reset(&mut self) {
        if self.sys.ie & self.sys.iflags & INT_MASK == 0 {
            let now = self.clock();
            self.sys.timer.stop_edge(now);
        }
        self.div_reset();
    }

    /// The APU part of a DIV write or STOP (`old`: the counter before it):
    /// if the DIV-APU bit was 1, the reset is its falling edge and the
    /// frame sequencer steps now (SameBoy `GB_set_internal_div_counter`,
    /// `triggers & apu_bit`); then the deadline moves with the rebased
    /// counter. The caller reschedules.
    #[inline(never)]
    fn apu_div_reset(&mut self, old: u64) {
        if !self.mem.apu.powered() {
            return;
        }
        let now = self.clock();
        if old >> self.sys.timer.div_apu_bit() & 1 != 0 {
            self.apu_sync(now);
            self.mem.apu.step();
        }
        self.set_apu_deadline(now);
    }

    /// Sets the `DivApu` deadline at clock `now`, after a frame-sequencer
    /// step, a sweep event or a counter rebase: `SWEEP_LAG` clocks on while
    /// a sweep step or overflow-check tick is pending (`Sweep::due`; a
    /// rebase inside the lag, at most 2 clocks
    /// into it in double speed, moves it by that much), else the next
    /// DIV-APU edge.
    #[inline(never)]
    fn set_apu_deadline(&mut self, now: u64) {
        let apu = &self.mem.apu;
        if apu.sweep.due {
            self.sys.sched.set(EventKind::DivApu, now + apu::SWEEP_LAG);
        } else {
            self.sys.set_div_apu_deadline(apu.powered(), now);
        }
    }

    /// Brings the APU up to `now`, writing completed samples into the
    /// caller's buffer (ADR 0021 section 3).
    #[inline(never)]
    pub(crate) fn apu_sync(&mut self, now: u64) {
        let Memory { apu, io, .. } = &mut *self.mem;
        apu.sync(now, io, self.audio, &mut self.audio_len);
    }

    /// The end of a run call that started at clock `start`: the samples
    /// completed in it are written, a partial window carries over (ADR 0021
    /// section 3), and the context is dropped. Out of line: once per call,
    /// off `Emulator::run`.
    #[inline(never)]
    pub(crate) fn finish(mut self, start: u64) -> RunResult {
        self.apu_sync(self.clock());
        RunResult {
            events: self.events,
            clocks: self.clock() - start,
            audio_samples: self.audio_len,
        }
    }

    /// The start of a run call: the APU is brought to the clock (advance
    /// only, if a caller left it behind), and with buffer room the
    /// `AudioBuffer` deadline is where the last slot's window ends. The
    /// caller reschedules. Out of line: once per call, off `Emulator::run`.
    #[inline(never)]
    fn start_audio(&mut self) {
        let now = self.clock();
        let apu = &mut self.mem.apu;
        if apu.out.synced < now {
            apu.sync(now, &self.mem.io, &mut [], &mut 0);
        }
        let at = match self.audio.len() {
            0 => NEVER,
            room => self.mem.apu.buffer_deadline(room),
        };
        self.sys.sched.set(EventKind::AudioBuffer, at);
    }

    /// A DIV-APU falling edge (`EventKind::DivApu`, due at `now`), or the
    /// pending sweep event `SWEEP_LAG` clocks after the last one
    /// (`Sweep::due`: the step, or a tick of its overflow check).
    #[inline(never)]
    fn div_apu_event(&mut self, now: u64) {
        self.apu_sync(now);
        let apu = &mut self.mem.apu;
        if apu.sweep.due {
            apu.sweep.clock(&mut apu.sq[0]);
        } else {
            apu.step();
        }
        self.set_apu_deadline(now);
    }

    /// The last free audio slot's window ended (`EventKind::AudioBuffer`):
    /// write it and end the call. The deadline stays `NEVER`.
    #[inline(never)]
    fn audio_buffer_event(&mut self, now: u64) {
        self.apu_sync(now);
        self.raise(Events::AUDIO_BUFFER_FULL);
    }

    /// A write to FF10-FF3F (by the low 7 address bits): the APU is synced
    /// to the access first, then the write applies (ADR 0021 section 3).
    /// While the APU is off, FF10-FF25 ignore writes on CGB, the NRx1
    /// lengths included (SameBoy `GB_apu_write`: only DMG accepts them);
    /// NR52 and wave RAM stay writable. Out of line, off `write_high`.
    #[inline(never)]
    fn write_apu(&mut self, reg: usize, value: u8) {
        let now = self.clock();
        self.apu_sync(now);
        match reg {
            0x26 => self.write_nr52(value, now),
            0x10..=0x25 if !self.mem.apu.powered() => {}
            // CH3 and wave RAM (also while off).
            0x1A..=0x1E | 0x30..=0x3F => apu::write_wave(self.mem, reg, value),
            // Channel registers: stored, then decoded by the channels.
            0x10..=0x25 => {
                self.mem.io[reg] = value;
                let edge = self.sys.sched.deadline(EventKind::DivApu);
                // The M-cycle length (bit 1: double speed), plus 1 with the
                // APU's delay after a switch to double speed (`Apu::write`).
                let ds = self.hot.mcycle as u8 | u8::from(self.sys.timer.apu_ds_delay);
                self.mem.apu.write(reg, value, now, ds, edge);
            }
            // FF27-FF2F: unused, stored.
            _ => self.mem.io[reg] = value,
        }
    }

    /// NR52: only bit 7 (power) is writable (Pan Docs: FF26 - NR52; ADR
    /// 0021 section 7). Off zeroes FF10-FF25 and the APU state but the
    /// output stream; on starts the frame sequencer. Synced to `now`.
    fn write_nr52(&mut self, value: u8, now: u64) {
        let on = value & 0x80 != 0;
        if on == self.mem.apu.powered() {
            return;
        }
        if on {
            // The power-on skip tests the counter as of the end of the
            // write's M-cycle, 4 counter ticks on in either speed: a write
            // in the M-cycle at whose end bit 12 (13 in double speed) rises
            // counts as high (gambatte `ch2_late_reset_nr52_2*` and
            // `ch2_init_reset_length_counter_timing_nr52_*`, CGB; SameBoy
            // `cycle_write` advances the pending cycles, then writes).
            let sc = self.sys.timer.sc(now) + 4;
            let high = sc >> self.sys.timer.div_apu_bit() & 1 != 0;
            self.mem.apu.power_on(now, high);
        } else {
            self.mem.apu.power_off();
            self.mem.io[0x10..=0x25].fill(0);
        }
        self.sys.timer.apu_ds_delay = false;
        // The first edge is the next one after the end of the write's
        // M-cycle, consistent with the skip test above: an edge at that
        // end falls while the APU is still off (SameBoy `cycle_write` runs
        // the pending cycles, then `GB_apu_init`). One M-cycle is
        // 4 counter ticks in either speed. Power-off ignores it.
        self.sys
            .set_div_apu_deadline(on, now + self.hot.mcycle as u64);
        self.reschedule();
    }

    /// Records `events`, ending the run call after this instruction if
    /// one of them is a stop event.
    #[inline]
    pub(crate) fn raise(&mut self, events: Events) {
        self.events |= events;
        if events.intersects(self.stop_on) {
            self.done = true;
        }
    }

    /// Sets the countdown to the earlier of the earliest scheduled event
    /// and the target, capped at `i32::MAX` clocks ahead (reaching a
    /// capped deadline just reschedules). Called whenever a deadline
    /// changes while the countdown runs.
    fn reschedule(&mut self) {
        self.reschedule_at(self.clock());
    }

    /// [`Self::reschedule`], `now` being the current clock.
    #[inline(always)]
    fn reschedule_at(&mut self, now: u64) {
        let deadline = self.sys.sched.next().min(self.target);
        let ahead = deadline.saturating_sub(now).min(i32::MAX as u64);
        self.hot.deadline = now + ahead;
        self.hot.budget = ahead as i32;
    }

    /// Idle M-cycles until the countdown reaches zero (the first M-cycle
    /// boundary at or after the deadline): `ceil(budget / mcycle)`, and at
    /// least one.
    #[inline]
    fn mcycles_to_deadline(&self) -> u32 {
        // `mcycle` is 4 or 2: a shift by 2 or 1, not a division (no
        // divide-by-zero panic path on thumbv7em).
        debug_assert!(self.hot.mcycle == 2 || self.hot.mcycle == 4);
        let m = self.hot.mcycle as u32;
        (self.hot.budget.max(1) as u32 + m - 1) >> (m >> 1)
    }

    /// The countdown reached zero: run what is due at the current clock
    /// and set the next deadline.
    #[inline(never)]
    fn boundary(&mut self) {
        let now = self.clock();
        if now >= self.sys.sched.next() {
            self.run_events(now);
        }
        if now >= self.target {
            self.target = u64::MAX;
            self.done = true;
        }
        // No handler moves the clock.
        self.reschedule_at(now);
    }

    /// Runs every kind of scheduled work whose deadline is at or before
    /// `now`, in [`EventKind`] order, so events on the same clock run in a
    /// fixed order (ADR 0011). Each handler sets its next deadline. The
    /// caller checked that something is due (`now >= sched.next()`).
    fn run_events(&mut self, now: u64) {
        while let Some(kind) = self.sys.sched.take_first_due(now) {
            match kind {
                EventKind::Timer => {
                    let s = &mut *self.sys;
                    s.iflags |= s.timer.reload(now);
                    s.sched.set_inline(EventKind::Timer, s.timer.deadline());
                }
                EventKind::Serial => self.serial_event(now),
                EventKind::Ppu => self.sync_ppu(),
                EventKind::Input => self.apply_inputs(now),
                EventKind::Dma => {
                    if self.sys.ppu.dma.tail < dma::LEN {
                        self.dma_tail_end();
                    } else {
                        self.dma_sync(now);
                        self.hot.dma = 0;
                    }
                }
                EventKind::VramDma => self.vdma_hblank(),
                EventKind::DivApu => self.div_apu_event(now),
                EventKind::AudioBuffer => self.audio_buffer_event(now),
            }
            if now < self.sys.sched.next() {
                break;
            }
        }
    }

    /// A serial bit shift is due at `now` (`EventKind::Serial`). Out of
    /// line, off `boundary`: only while a transfer runs. `boundary`
    /// reschedules after the event loop.
    #[cold]
    #[inline(never)]
    fn serial_event(&mut self, now: u64) {
        let done = self.sys.serial.shift();
        self.serial_done(done);
        let at = self.serial_deadline(now);
        self.sys.sched.set_inline(EventKind::Serial, at);
    }

    /// Applies the scheduled button changes due at the M-cycle boundary
    /// `now`: those at a clock inside the M-cycle that starts at `now` or
    /// earlier (rounded down, ADR 0012), so accesses from `now` on see
    /// them. Then schedules the next one. Out of line, off `boundary`.
    #[cold]
    #[inline(never)]
    fn apply_inputs(&mut self, now: u64) {
        let m = self.hot.mcycle as u64;
        let s = &mut *self.sys;
        if let Some(buttons) = s.inputs.take_due(now + m - 1) {
            s.set_joypad(buttons, s.joyp_select);
        }
        s.schedule_inputs(m);
    }

    /// Catches the PPU up to the current clock and reschedules; delivers a
    /// delayed LYC-write interrupt ([`Self::lyc_irq_delay`]) once due.
    #[inline(never)]
    fn sync_ppu(&mut self) {
        let now = self.clock();
        self.sync_ppu_to(now);
        if self.sys.iflags & LYC_IRQ_PENDING != 0 {
            self.lyc_irq_due(now);
        }
    }

    /// Single speed: the STAT interrupt a LYC write requests (LY = LYC
    /// newly true with the LYC select set) reaches IF 5 dots after the
    /// write's access, one dot into the next M-cycle: an interrupt check
    /// or IF read at the end of the write's M-cycle doesn't see it, one an
    /// M-cycle later does. gambatte (behaviour only, GPL)
    /// `LCD::lycRegChange`: CGB single speed sets
    /// `memevent_oneshot_statirq` at `cc + 5`, double speed and DMG flag
    /// the interrupt at once. CGB04C
    /// `lycEnable/lyc_ff45_trigger_delay_2` (the interrupt is dispatched
    /// one instruction later than at the write: out2, DMG out0).
    ///
    /// The request waits in IF's unused bits ([`LYC_IRQ_PENDING`], with the
    /// parity of the write's M-cycle in [`LYC_IRQ_PHASE`]; `Hw` has no
    /// room for a field) and the PPU deadline is moved to the due dot.
    /// Out of line, off `write_ppu`.
    #[cold]
    #[inline(never)]
    fn lyc_irq_delay(&mut self, now: u64) {
        let phase = if now & 4 != 0 { LYC_IRQ_PHASE } else { 0 };
        let s = &mut *self.sys;
        s.iflags = s.iflags & !ppu::INT_STAT | LYC_IRQ_PENDING | phase;
        // Not rescheduled from the walk here: `sync_ppu` at the due
        // boundary (or an earlier PPU deadline) does that.
        let due = s.sched.deadline(EventKind::Ppu).min(now + 5);
        s.sched.set(EventKind::Ppu, due);
        self.reschedule();
    }

    /// A PPU event with a delayed LYC-write interrupt waiting: at the
    /// M-cycle boundary 8 dots after the write's access (same M-cycle
    /// parity) it reaches IF; at the one 4 dots after it (a walk point
    /// came first), it waits one more M-cycle. Single speed only, so the
    /// boundaries are 4 dots apart.
    #[cold]
    #[inline(never)]
    fn lyc_irq_due(&mut self, now: u64) {
        let s = &mut *self.sys;
        let phase = if now & 4 != 0 { LYC_IRQ_PHASE } else { 0 };
        if s.iflags & LYC_IRQ_PHASE == phase {
            s.iflags = s.iflags & INT_MASK | ppu::INT_STAT;
        } else {
            let due = s.sched.deadline(EventKind::Ppu).min(now + 1);
            s.sched.set(EventKind::Ppu, due);
        }
    }

    /// Catches the PPU up to clock `at` (the current clock, or one dot
    /// later for a register write timed there) and reschedules. A running
    /// OAM DMA is brought up to date first: the PPU may read OAM.
    fn sync_ppu_to(&mut self, at: u64) {
        self.run_ppu_to_call(at);
        self.schedule_ppu();
    }

    /// [`Self::sync_ppu_to`] without the rescheduling.
    #[inline(always)]
    fn run_ppu_to(&mut self, at: u64) {
        if self.hot.dma != 0 {
            self.dma_sync(self.clock());
        }
        let double = self.hot.mcycle == 2;
        let s = &mut *self.sys;
        let events = s.ppu.sync(at, self.mem, self.frame, &mut s.iflags, double);
        self.raise(events);
        if self.sys.vdma.hblank {
            self.vdma_arm(at, true);
        }
    }

    /// [`Self::run_ppu_to`] as a call, for the callers other than
    /// `write_palette_data` (the one hot enough to inline it, for raster
    /// palette effects), for the code-size budgets.
    #[inline(never)]
    fn run_ppu_to_call(&mut self, at: u64) {
        self.run_ppu_to(at);
    }

    /// Sets the Ppu deadline and reschedules.
    fn schedule_ppu(&mut self) {
        let s = &mut *self.sys;
        s.sched
            .set(EventKind::Ppu, s.ppu.next_event(self.hot.mcycle == 2));
        self.reschedule();
    }

    /// A speed switch (after `hot.mcycle` changed): the walk's line-0 and
    /// line-153 points differ per speed (`ppu::edges::ly0_pulse`), so the PPU
    /// re-seeks its cursor and the deadline moves (to single speed they
    /// come earlier).
    #[cold]
    #[inline(never)]
    fn ppu_speed_changed(&mut self) {
        self.sys.ppu.speed_changed(self.hot.mcycle == 2);
        self.schedule_ppu();
    }

    /// Writes to the PPU registers FF40-FF45 and FF47-FF4B. The PPU is
    /// synced to the write first (ADR 0002): the write lands between the
    /// update points before and after it, and moves the next deadline.
    /// Out of line, off `write_io`.
    #[inline(never)]
    fn write_ppu(&mut self, reg: usize, value: u8) {
        let now = self.clock();
        let double = self.hot.mcycle == 2;
        match reg {
            0x40 => {
                self.run_ppu_to_call(now);
                // Double speed: the fetcher sees the write one dot earlier
                // (as SCX, SCY below; SameBoy `GB_CONFLICT_LCDC_CGB_DOUBLE`
                // writes at `pending_cycles - 2`, 8 MHz ticks).
                let at = now - u64::from(double);
                let s = &mut *self.sys;
                s.ppu.flush(at, self.mem, self.frame);
                s.ppu.hold_fetch(at, self.mem, reg, value, false);
                // Turning the LCD off outside mode 0 starts an HBlank
                // transfer block (SameBoy `GB_lcd_off`).
                if s.vdma.hblank && value & 0x80 == 0 && s.ppu.stat(now, double) & 3 != 0 {
                    s.vdma.pending = true;
                    self.done = true;
                }
                let s = &mut *self.sys;
                if (value ^ s.ppu.lcdc) & 0x04 != 0 {
                    s.ppu.obj_size_write(now, double, self.mem);
                    s.ppu.obj_size_fetch(at, value, self.mem);
                }
                let win = (value ^ s.ppu.lcdc) & 0x20 != 0;
                s.ppu.write_lcdc(now, value, &mut s.iflags, double);
                s.ppu.check_wy_at(now, self.mem, double);
                if win {
                    s.ppu.window_write(at, self.mem, None);
                }
            }
            0x41 => {
                // CGB: in two phases one dot apart; for the first, the LYC
                // select (double speed: the mode-0 select) keeps its old
                // value (SameBoy `GB_CONFLICT_STAT_CGB`, `_DOUBLE`). When
                // the write sets that select, the selects it clears stay
                // set for the first phase too: switching from an active
                // source to an active LY=LYC keeps the line high, with no
                // interrupt (gambatte `miscmstatirq/lycstatwirq_trigger_*`
                // CGB `E0`, `statChangeTriggersStatIrqCgb`; behavior only,
                // GPL). SameBoy clears them at once, a one-dot low glitch.
                let keep = if double { 0x08 } else { 0x40 };
                let old = self.sys.ppu.stat_select();
                let mut first = (value & !keep) | (old & keep);
                if value & !old & keep != 0 {
                    first |= old;
                }
                self.run_ppu_to_call(now);
                let s = &mut *self.sys;
                s.ppu
                    .write_stat(now, first, &mut s.iflags, double, View::StatPhase1);
                let at = now + edges::STAT_PHASE2.delay(double);
                // Single speed, phase 2 on line 0's mode-2 pulse: mode 1
                // still holds the line there (gambatte's line 0 starts a dot
                // later, `Ppu::early_point`), so a write that keeps it up
                // requests nothing. The pulse itself runs first (a mode 2
                // the write selects is past it). CGB04C
                // `miscmstatirq/lycstatwirq_trigger_ly00_10_50_1` (`10 ->
                // 50`, LYC = 0: E0), `m2enable/late_enable_m1disable_ly0_2`.
                // (Synced to `now`, the dot before, for phase 1.)
                let pulse = self.sys.ppu.early_point(at, double) == ppu::EARLY_PULSE;
                let high = self.sys.ppu.stat_line_high();
                self.run_ppu_to_call(at);
                let s = &mut *self.sys;
                let old = s.iflags;
                s.ppu
                    .write_stat(at, value, &mut s.iflags, double, View::StatPhase2);
                if pulse && high && s.ppu.stat_line_high() {
                    s.iflags &= old | !ppu::INT_STAT;
                }
            }
            0x44 => {} // LY is read-only.
            0x45 => {
                // Single speed: one dot into the M-cycle (SameBoy
                // `GB_CONFLICT_WRITE_CPU`); double speed: at the access
                // (`GB_CONFLICT_READ_OLD`).
                let at = now + edges::LYC_WRITE.delay(double);
                self.run_ppu_to_call(at);
                let s = &mut *self.sys;
                let old = s.iflags;
                s.ppu.write_lyc(at, value, &mut s.iflags, double);
                if !double && s.iflags & !old & ppu::INT_STAT != 0 {
                    self.lyc_irq_delay(now);
                    return;
                }
            }
            // SCY SCX BGP OBP0 OBP1 WY WX: the pixels already output in
            // mode 3 are drawn with the old value first. Where in the
            // M-cycle the write lands (SameBoy `cgb_conflict_map`,
            // single speed): SCY, SCX and WY at the access
            // (`GB_CONFLICT_READ_OLD`); the palettes one dot earlier on
            // CGB C (`GB_CONFLICT_PALETTE_CGB`, `pending_cycles - 1`;
            // Mealybug `m3_bgp_change`, `m3_obp0_change` CGB C
            // screenshots); WX one dot later (`GB_CONFLICT_WRITE_CPU`).
            // Double speed: SCX and SCY (and LCDC above) one dot before the
            // access (SameBoy `GB_CONFLICT_SCX_DMG_AND_CGB_DOUBLE`:
            // `pending_cycles - 2` 8 MHz ticks; SameBoy keeps SCY at the
            // access, but gambatte `scy_during_m3_*ds*` want the same dot
            // as SCX: docs/design/mid-line-ppu.md step D), the others at
            // the access (approximation).
            _ => {
                let at = match reg {
                    0x42 | 0x43 if double => now - 1,
                    _ if double => now,
                    0x47..=0x49 => now - 1,
                    0x4B => now + 1,
                    _ => now,
                };
                self.run_ppu_to_call(now.max(at));
                let s = &mut *self.sys;
                if matches!(reg, 0x42 | 0x43) {
                    s.ppu.scroll_write(at, self.mem, self.frame, reg, value);
                } else {
                    s.ppu.flush(at, self.mem, self.frame);
                }
                if reg == 0x4A {
                    s.ppu.wy_write(now, self.mem, double);
                }
                self.mem.io[reg] = value;
                match reg {
                    0x47..=0x49 => s.ppu.write_palette(
                        reg,
                        value,
                        &s.config.dmg_palettes,
                        s.cgb_mode,
                        &mut self.mem.bg_pairs,
                    ),
                    // The WY condition and the window start in mode 3
                    // (`Ppu::wy_written`; WY lands at the access).
                    0x4A => s.ppu.wy_written(now, self.mem, double),
                    0x4B => s.ppu.window_write(at, self.mem, Some(ppu::WX_ON_LEAD)),
                    _ => {}
                }
            }
        }
        // The selects, LYC or LCDC may move the next deadline.
        self.schedule_ppu();
    }

    /// A CPU write at the current clock (no M-cycle; the caller ticks).
    /// During an OAM DMA transfer every address takes the last arm
    /// ([`Hot::dma`]), where the flag test sends it to the conflict path,
    /// off the RAM fast path.
    #[inline(never)]
    pub(crate) fn write_mapped(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match (addr | self.hot.dma) >> 13 {
            0..=3 => self.sys.cart.write_rom(addr, value, self.hot.clock()),
            4 => self.write_vram(addr, value),
            5 => self.sys.cart.write_ram(addr, value, self.hot.clock()),
            6 => *wram_mut(self.mem, self.sys, a) = value,
            _ if self.hot.dma != 0 => self.write_during_dma(addr, value),
            _ if addr < 0xFE00 => *wram_mut(self.mem, self.sys, a) = value,
            _ => self.write_high(addr, value),
        }
    }

    /// [`Self::write_mapped`] without the conflict path, for
    /// [`Self::write_during_dma`].
    #[inline(always)]
    fn write_to(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match addr >> 13 {
            0..=3 => self.sys.cart.write_rom(addr, value, self.hot.clock()),
            4 => self.write_vram(addr, value),
            5 => self.sys.cart.write_ram(addr, value, self.hot.clock()),
            6 => *wram_mut(self.mem, self.sys, a) = value,
            _ if addr < 0xFE00 => *wram_mut(self.mem, self.sys, a) = value,
            _ => self.write_high(addr, value),
        }
    }

    /// FE00-FFFF.
    #[inline(never)]
    fn write_high(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match addr {
            0xFE00..=0xFEFF => self.write_oam(addr, value),
            0xFF00..=0xFF7F => self.write_io(a & 0x7F, value),
            0xFF80..=0xFFFE => self.mem.hram[a - 0xFF80] = value,
            0xFFFF => self.sys.ie = value,
            _ => {}
        }
    }

    /// A CPU write to VRAM: dropped while the PPU blocks VRAM
    /// ([`Ppu::vram_blocked`]). Out of line: the check needs the PPU's
    /// position.
    #[inline(never)]
    fn write_vram(&mut self, addr: u16, value: u8) {
        if self
            .sys
            .ppu
            .vram_blocked(self.clock(), self.hot.mcycle == 2)
        {
            return;
        }
        let a = usize::from(addr);
        self.mem.vram[(self.sys.vram_off | (a & 0x1FFF)) & 0x3FFF] = value;
    }

    /// A CPU write to FE00-FEFF: dropped while the PPU blocks OAM
    /// ([`Ppu::oam_blocked`]), FEA0-FEFF included (Pan Docs: Memory Map,
    /// "FEA0-FEFF range").
    #[inline(never)]
    fn write_oam(&mut self, addr: u16, value: u8) {
        if self
            .sys
            .ppu
            .oam_blocked(self.clock(), self.hot.mcycle == 2, true)
        {
            return;
        }
        let a = usize::from(addr);
        if addr < 0xFEA0 {
            self.mem.oam[a & 0xFF] = value;
            self.sys.ppu.oam_written();
        } else {
            self.mem.extra_oam[extra_oam_index(a)] = value;
        }
    }

    /// FF00-FFFF, as `write_high` without a transfer.
    fn write_ff(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match addr {
            0xFF00..=0xFF7F => self.write_io(a & 0x7F, value),
            0xFF80..=0xFFFE => self.mem.hram[a - 0xFF80] = value,
            _ => self.sys.ie = value,
        }
    }

    /// A BCPD (`obj` false) or OCPD (`obj` true) write in CGB mode: steps
    /// the index; unless the PPU blocks palette RAM
    /// ([`Ppu::palette_blocked`]), stores the byte at BCPS/OCPS and
    /// re-resolves the color.
    /// Out of line: rare next to the other I/O writes (hi-color raster
    /// effects do it a few times a line).
    #[inline(never)]
    fn write_palette_data(&mut self, obj: bool, value: u8) {
        let now = self.clock();
        self.run_ppu_to(now);
        let s = &mut *self.sys;
        let i = palette_index_step(&mut self.mem.io[if obj { 0x6A } else { 0x68 }]);
        // Synced to `now`: drawing (latched, so past the window start, and
        // before the mode-3 end) means blocked, and when not drawing there
        // are no pixels to flush with the old colors.
        if s.ppu.drawing() || s.ppu.palette_blocked(now, self.hot.mcycle == 2) {
            return;
        }
        if obj {
            self.mem.obj_pal[i] = value;
            s.ppu.write_ocpd(i, self.mem);
        } else {
            self.mem.bg_pal[i] = value;
            s.ppu.write_bcpd(i, self.mem);
        }
    }

    /// IF (FF0F): lands one T-cycle into the M-cycle (SameBoy
    /// `GB_CONFLICT_WRITE_CPU` in `cgb_conflict_map`, both speeds), so a
    /// PPU interrupt requested at the access dot (double speed: half a dot
    /// after it, a whole dot on our grid) is cleared by it, one later is
    /// not. CGB04C `m2int_m0irq/m2int_m0irq_scx3_ifw_{1,2,3,4,ds_1,ds_2}`
    /// (the mode-0 interrupt with SCX = 3 against an IF write one M-cycle
    /// apart). A delayed LYC-write interrupt still waiting stays
    /// ([`Self::lyc_irq_delay`]).
    #[inline(never)]
    fn write_if(&mut self, value: u8) {
        let at = self.clock() + 1;
        if self.sys.sched.deadline(EventKind::Ppu) <= at
            && self.sys.ppu.early_point(at, self.hot.mcycle == 2) == 0
        {
            // No reschedule: the PPU deadline, now passed, runs
            // `sync_ppu` at the next boundary, which sets the next one
            // (and delivers a waiting delayed LYC-write interrupt).
            self.run_ppu_to_call(at);
        }
        let s = &mut *self.sys;
        s.iflags = value & INT_MASK | s.iflags & !INT_MASK;
    }

    /// [`Bus::push_acknowledge`] of a push onto IF itself (SP = FF10): the
    /// vector comes from IF as it was before the write, the write and the
    /// IF read being the same access (SameBoy `cycle_write_if` returns the
    /// old IF; gambatte cgb04c `irq_precedence/if_and_ie_0_vector_4`:
    /// SP = FF11 writes 0x80 to IF and the timer vector still runs). Out
    /// of line: in the inlined dispatch, the old-IF select cost ~1.5% Ir
    /// in every bench ROM (codegen of `Cpu::run`, measured).
    #[cold]
    #[inline(never)]
    fn push_acknowledge_if(&mut self, value: u8, ie: u8) -> u8 {
        let old = self.sys.iflags;
        self.write_mapped(0xFF0F, value);
        let pending = ie & old & INT_MASK;
        let bit = pending.isolate_lowest_one();
        // The bit is cleared as in `push_acknowledge`.
        const PPU: u8 = crate::ppu::INT_VBLANK | crate::ppu::INT_STAT;
        if bit & PPU != 0 {
            let mid = self.clock() + self.hot.mcycle as u64 / 2;
            if self.sys.sched.deadline(EventKind::Ppu) <= mid {
                self.run_ppu_to_call(mid);
            }
            self.sys.iflags &= !bit;
        }
        self.idle();
        self.sys.iflags &= !(bit & !PPU);
        pending
    }

    /// FF00-FF7F, by the low 7 address bits.
    fn write_io(&mut self, reg: usize, value: u8) {
        let s = &mut *self.sys;
        let cgb = s.cgb_mode;
        match reg {
            0x00 => s.set_joypad(s.buttons, value),
            0x01 => s.serial.sb = value,
            0x02 | 0x04..=0x07 => self.write_timer_io(reg, value),
            0x0F => self.write_if(value),
            0x10..=0x3F => self.write_apu(reg, value),
            0x40..=0x45 | 0x47..=0x4B => self.write_ppu(reg, value),
            0x46 => self.oam_dma(value),
            0x51..=0x54 if cgb => s.vdma.write_addr(reg - 0x50, value),
            0x55 if cgb => self.write_hdma5(value),
            0x4D if cgb => s.key1 = value & 0x01,
            0x4F if cgb => {
                s.vbk = value & 0x01;
                s.vram_off = usize::from(s.vbk) * 0x2000;
            }
            // CGB palettes. BCPS/OCPS (the index registers) are writable on
            // a CGB in either mode, BCPD/OCPD only in CGB mode (SameBoy
            // `write_high_memory`); blocked around mode 3
            // ([`Ppu::palette_blocked`]).
            0x68 | 0x6A => self.mem.io[reg] = value,
            0x69 | 0x6B if cgb => self.write_palette_data(reg == 0x6B, value),
            // OPRI: writable in CGB mode only (after the boot ROM), and only
            // stored: SameBoy applies a write to the object priority mode
            // only while the boot ROM runs or in KEY0's PGB mode (Core/
            // memory.c, GB_IO_OPRI; Pan Docs: OPRI), so after boot the
            // mode stays what reset chose (OAM index in CGB mode).
            0x6C if cgb => self.mem.io[reg] = value,
            0x70 if cgb => {
                s.svbk = value & 0x07;
                // Bank 0 selects bank 1 (Pan Docs: SVBK).
                s.wram_off = usize::from(s.svbk.max(1)) * 0x1000;
            }
            // Undocumented CGB registers (SameBoy Core/memory.c,
            // `write_high_memory`: GB_IO_PSWX/PSWY/PSW/PGB store the
            // value; the read masks are in `read_io`).
            0x72..=0x75 => self.mem.io[reg] = value,
            _ => {}
        }
    }

    /// FF46: starts an OAM DMA transfer from `value << 8` (Pan Docs: OAM
    /// DMA Transfer). The FF46 write's M-cycle and the next one are the
    /// start delay; byte `i` is transferred in the M-cycle `2 + i` after the
    /// write's. A write while a transfer is running restarts it: the old
    /// one stops, but keeps CPU OAM reads blocked through the new start
    /// delay unless it was in its last M-cycle (SameBoy `GB_IO_DMA`,
    /// `dma_restarting`; Mooneye `oam_dma_restart`).
    ///
    /// The PPU is brought up to the write first (its OAM reads so far saw
    /// the old state), and OAM is kept as it is now for the PPU's reads of
    /// bytes not written yet (`Memory::oam_dma_old`).
    #[inline(never)]
    fn oam_dma(&mut self, value: u8) {
        let now = self.clock();
        if self.sys.ppu.dma.tail < dma::LEN {
            self.dma_tail_end();
        }
        self.dma_ppu_event(now);
        self.mem.io[0x46] = value;
        let m = self.hot.mcycle as u64;
        let restart = self.hot.dma != 0 && {
            self.dma_sync(now);
            now + m < self.sys.ppu.dma.end
        };
        // The old transfer still moves a byte in the next M-cycle, if it
        // is past its own start delay by then, but already on the new
        // source's bus and address (gambatte: the FF46 write sets the new
        // source at once and the old position runs on until the new
        // start; cgb04c `oamdma_src8000_srcchange0000_busyinc`).
        let old = self.sys.ppu.dma.start;
        let start = now + 2 * m;
        let end = start + u64::from(dma::LEN) * m;
        let mut d = OamDma {
            base: u16::from(value) << 8,
            start,
            block_from: if restart { now } else { start },
            end,
            mcycle: m as u8,
            ppu_live: true,
            ..OamDma::new()
        };
        d.set_ppu_window();
        let mut deadline = end;
        if restart && now + m >= old {
            // Until the `Dma` deadline at the real `start`
            // (`dma_tail_end`), `start` is that of a transfer at byte
            // `tail` in the next M-cycle, so the CPU's conflict path
            // (`dma_conflict`) needs no test for it; `copied` keeps the
            // lazy copy from moving bytes meanwhile.
            let tail = ((now + m - old) >> m.trailing_zeros()) as u8;
            d.tail = tail;
            d.start = now + m - u64::from(tail) * m;
            d.copied = tail + 1;
            deadline = start;
        }
        self.sys.ppu.dma = d;
        self.mem.oam_dma_old = self.mem.oam;
        self.hot.dma = 0xFE00;
        self.sys.sched.set(EventKind::Dma, deadline);
        self.reschedule();
    }

    /// The `Dma` deadline at a restarted transfer's real start (or
    /// anything that needs the real state before it): undoes the restart
    /// tail's stand-in `start` (see [`Self::oam_dma`]). Cold.
    #[cold]
    #[inline(never)]
    fn dma_tail_end(&mut self) {
        let d = &mut self.sys.ppu.dma;
        let m = u64::from(d.mcycle);
        d.start += (u64::from(d.tail) + 1) * m;
        d.copied = 0;
        d.tail = dma::LEN;
        self.sys.sched.set(EventKind::Dma, d.end);
    }

    /// Before the OAM DMA state changes at `now` (an FF46 write, a halt
    /// during a transfer): the PPU catches up, so its OAM reads up to now
    /// (OAM scan entries, object fetches) see the old state
    /// ([`OamDma::ppu_at`] only describes clocks after the last change).
    #[inline(never)]
    fn dma_ppu_event(&mut self, now: u64) {
        self.run_ppu_to_call(now);
        let s = &mut *self.sys;
        s.ppu.dma_event(now, self.mem, self.frame);
        self.schedule_ppu();
    }

    /// Copies the OAM DMA bytes due by the CPU access at `now` (an M-cycle
    /// boundary). Called before anything can observe OAM or change a
    /// source byte not yet copied: a CPU write during the transfer, a
    /// restart, the end of a run call and the `Dma` deadline.
    fn dma_sync(&mut self, now: u64) {
        let d = self.sys.ppu.dma;
        // Paused while halted: no progress past the pause.
        let due = d.due(now.min(d.paused), self.hot.mcycle as u64);
        for i in d.copied..due {
            let src = d.base + u16::from(i);
            // CGB: a source at E000-FFFF reads FF (SameBoy `GB_dma_run`).
            self.mem.oam[usize::from(i)] = if src >= 0xE000 {
                0xFF
            } else {
                read::<false>(self.mem, self.sys, &self.hot.without_dma(), src)
            };
        }
        if due > d.copied {
            self.sys.ppu.oam_written();
        }
        self.sys.ppu.dma.copied = d.copied.max(due);
        // Only a transfer from VRAM records any (`dma_read_clear`).
        if d.base & 0xE000 == 0x8000 {
            self.dma_apply_read_clears();
        }
    }

    /// Clears the OAM bytes recorded by [`dma_read_clear`] (all copied by
    /// now: each was due by its read) and empties the record.
    #[cold]
    #[inline(never)]
    fn dma_apply_read_clears(&mut self) {
        let mem = &mut *self.mem;
        if mem.dma_read_clears.apply(&mut mem.oam) {
            self.sys.ppu.oam_written();
        }
    }

    /// A CPU write while an OAM DMA transfer runs: OAM writes are dropped
    /// for the whole transfer, start delay included (SameBoy
    /// `write_high_memory`), and the rest follows [`dma::conflict`] for
    /// CGB C. A write that conflicts with the DMA never reaches its
    /// target; what it does to OAM follows gambatte's cgb04c captures
    /// (ADR 0028). The transfer is synced first, because the write may
    /// change a source byte or its mapping.
    #[inline(never)]
    fn write_during_dma(&mut self, addr: u16, value: u8) {
        let now = self.clock();
        self.dma_sync(now);
        if addr >= 0xFE00 {
            if addr >= 0xFF00 {
                self.write_ff(addr, value);
            }
            return;
        }
        match dma_conflict(&self.sys.ppu.dma, now, self.hot.mcycle, addr) {
            (Conflict::None, _) => self.write_to(addr, value),
            (Conflict::Wram(a), _) => self.write_to(a, value),
            (Conflict::Open | Conflict::Dma | Conflict::DmaVram, i) => {
                // CGB C (gambatte cgb04c `oamdma_src*_busypush*`, ADR
                // 0028): the CPU's data replaces the OAM byte just
                // transferred, except that a VRAM source gets 00 there
                // and a WRAM source leaves it. SameBoy instead clears it
                // for any source below A000 and drops the rest.
                let base = self.sys.ppu.dma.base;
                if !(0xC000..0xE000).contains(&base) {
                    self.mem.oam[usize::from(i)] = if base & 0xE000 == 0x8000 { 0 } else { value };
                    self.sys.ppu.oam_written();
                }
            }
        }
    }

    /// HALT during a transfer: the DMA makes no progress while the CPU is
    /// halted (SameBoy `GB_dma_run` returns early while `halted`, and
    /// `GB_advance_cycles` overwrites `dma_cycles`, so the halted cycles
    /// are dropped, not banked). Called at the end of HALT's fetch
    /// M-cycle, whose own step still counts. OAM stays blocked (the
    /// transfer is still active) and the `Dma` deadline is cleared, so the
    /// halted skip doesn't stop at it. `docs/reference/oam-dma.md`.
    ///
    /// For the PPU (synced first: a halt changes what its OAM reads see):
    /// with bytes left, the transfer stays active and halted until the
    /// wake-up; halted in the start delay, it hasn't started for the PPU;
    /// with every byte written, its last step isn't held and the PPU sees
    /// it end on time (gambatte `oamdma_late_halt_stat_2`, cgb04c).
    #[inline(never)]
    fn dma_pause(&mut self) {
        let now = self.clock();
        if now >= self.sys.ppu.dma.end {
            return;
        }
        self.dma_ppu_event(now);
        self.dma_sync(now);
        let d = &mut self.sys.ppu.dma;
        d.paused = now;
        d.halt_from = now;
        d.halt_to = NEVER;
        d.halt_dest = d.copied;
        if now <= d.ppu_from {
            d.ppu_from = NEVER;
            d.ppu_to = NEVER;
        } else if d.copied < dma::LEN {
            d.ppu_to = NEVER;
        }
        self.sys.sched.set(EventKind::Dma, NEVER);
        self.reschedule();
    }

    /// The wake-up from HALT, at the start of the wake-up M-cycle: the
    /// transfer resumes there. SameBoy drops every halted M-cycle,
    /// including the wake-up one, then takes exactly one step on wake-up
    /// (`GB_cpu_run`: `dma_cycles = 4; GB_dma_run`): the same count, by the
    /// next CPU access, as resuming one M-cycle early. Everything still
    /// ahead moves out by the paused time. The PPU's OAM scan sees the CPU
    /// halted through the wake-up M-cycle (SameBoy clears `halted` after
    /// it); what it sees up to now doesn't change, so it needn't sync.
    #[inline(never)]
    fn dma_resume(&mut self) {
        let now = self.clock();
        let m = self.hot.mcycle as u64;
        let d = &mut self.sys.ppu.dma;
        let shift = now - d.paused;
        if d.block_from > d.paused {
            d.block_from += shift;
        }
        d.start += shift;
        d.end += shift;
        d.paused = NEVER;
        d.halt_to = now + m;
        let held = d.ppu_to == NEVER;
        if d.ppu_from == NEVER {
            d.set_ppu_window();
        } else if held {
            d.ppu_to = d.end - dma::PPU_LEAD;
        }
        self.sys.sched.set(EventKind::Dma, d.end);
        self.reschedule();
    }

    /// A speed switch while the transfer is paused ([`Self::dma_pause`]):
    /// what is still ahead keeps its M-cycle count at the new M-cycle
    /// length `m`, around the pause (SameBoy counts the DMA in CPU cycles,
    /// `dma_cycles`). Unverified on hardware (`docs/reference/oam-dma.md`).
    #[cold]
    #[inline(never)]
    fn dma_retime(&mut self, m: u64) {
        let d = &mut self.sys.ppu.dma;
        let p = d.paused;
        // Clocks from `p` (on the old M-cycle grid, as `p` is) scaled to
        // the new M-cycle length: halved to double speed, else doubled.
        let scale = |x: u64| if m == 2 { x >> 1 } else { x << 1 };
        let at = |t: u64| {
            if t >= p {
                p + scale(t - p)
            } else {
                p - scale(p - t)
            }
        };
        if d.block_from > p {
            d.block_from = at(d.block_from);
        }
        d.start = at(d.start);
        d.end = d.start + u64::from(dma::LEN) * m;
        d.mcycle = m as u8;
    }

    /// The CGB speed switch of a STOP ([`StopKind::Switch`]), after its second
    /// byte if read; returns whether the CPU waits ([`Bus::speed_switch`]).
    /// After SameBoy `stop()`
    /// and `GB_advance_cycles` (MIT; ADR 0020 has the derivation):
    ///
    /// - KEY1 bit 0 clears and the speed toggles.
    /// - Nothing pending: the CPU halts for 0x20008 T-cycles (0x8002
    ///   M-cycles; `speed_switch_halt_countdown`) by the timer, which keeps
    ///   running (Pan Docs: FF4D - KEY1). The rest of the machine is frozen
    ///   for its first 5 T-cycles (`speed_switch_freeze`), so it sees
    ///   0x8001 M-cycles at the new speed (exact to double speed, one clock
    ///   long to single speed, where SameBoy itself avoids "odd mode"): the
    ///   timer runs one M-cycle ahead, which cancels STOP's second byte,
    ///   whose M-cycle it doesn't count (`timers_run` returns while
    ///   `stopped`). An interrupt ends the halt early. OAM DMA is paused
    ///   through it (`GB_dma_run` returns while `halted`), as in HALT.
    /// - An interrupt pending: no halt; to double speed the next two
    ///   M-cycles take 6 clocks for the PPU (one more M-cycle here, which
    ///   the timer doesn't count).
    /// - IME off: the timer starts 4 T-cycles late (`enter_stop_mode`:
    ///   `div_cycles = -4`, the DIV reset held). On CGB B/C the same hold
    ///   applies with an interrupt pending and IME on (below; SameBoy,
    ///   modelling CGB E, has none there).
    fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        self.sys.key1 = 0;
        let m = 6 - self.hot.mcycle as u64;
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        let dma_paused = self.sys.ppu.dma.paused != NEVER;
        if dma_paused {
            // Unlike HALT, the transfer's last step (its end) is held for
            // the PPU even with every byte written: gambatte's STOP brings
            // the transfer only up to this M-cycle (HALT: one further),
            // and its cgb04c `oamdma_late_speedchange_stat_1` (the switch
            // in the last byte's M-cycle) misses the object the PPU finds
            // in `_2` (the switch in the end's M-cycle: no pause).
            self.sys.ppu.dma.ppu_to = NEVER;
        }
        if dma_paused {
            self.dma_retime(m);
        }
        self.hot.mcycle = m as i32;
        self.ppu_speed_changed();
        let now = self.clock();
        // The held DIV reset (4 T-cycles): with IME off (SameBoy), and on
        // CGB B/C also with an interrupt pending. AGE `spsw-interrupts`
        // (pending, IME on, both directions): the first DIV increment and
        // the 16 kHz TIMA increment after the switch come one M-cycle
        // later on CGB B/C (`OFS_B`, verified on CPU CGB B and C) than on
        // CGB E, which SameBoy's model (no hold with IME on) matches.
        let mut freeze = if ime && !pending { 0 } else { 4 };
        if pending && m == 2 {
            freeze += 4;
        }
        // To single speed the halt is one clock short of whole M-cycles
        // (below); the timer counts that clock, so it stays on the CPU's
        // M-cycle grid.
        let short = i64::from(m == 4 && !pending);
        // T-cycles to clocks at the new speed.
        self.sys
            .timer
            .speed_switch(now, (freeze * m / 4) as i64 - short);
        // The DIV-APU bit changes (12 <-> 13) with the counter speed, and
        // a switch to double speed toggles the frame sequencer's delay
        // (`Timer::apu_ds_delay`).
        self.set_apu_deadline(now);
        if !pending && self.sys.vdma.hblank {
            self.vdma_stop();
        }
        if short != 0 {
            // The halt's odd clocks (`switch_end` below), idled first (the
            // CPU halts at `now`, so after `vdma_halt`): the CPU's M-cycle
            // grid moves by 3 (-1) clocks, and the input, serial and timer
            // deadlines below follow the new grid.
            self.hot.budget -= 3;
            if self.hot.budget <= 0 {
                self.boundary();
            }
        }
        // The M-cycle grid changes, and with it the boundary a scheduled
        // button change rounds down to (ADR 0012); `schedule_serial`
        // reschedules the countdown.
        self.sys.schedule_inputs(m);
        self.set_timer_deadline();
        self.schedule_serial();
        if !pending {
            // To single speed, Gambatte's PPU gains a clock at the switch
            // (`PPU::speedChange`: `p_.now -= isDoubleSpeed()`), and SameBoy
            // counts 0x20003 T-cycles at single speed after its 5-cycle
            // freeze: the PPU sees one clock less than 0x8001 M-cycles from
            // `now` to the next fetch (3 clocks, idled above, then 0x7FFF
            // M-cycles and the wake-up), so the CPU's M-cycle grid moves by
            // a clock against the PPU's, as on hardware (gambatte
            // `lcd_offset` tests, offsets 1 and 3).
            self.sys.vdma.switch_end = (now + 0x8000 * m - short as u64) as u32;
            return true;
        }
        if dma_paused {
            self.dma_resume();
        }
        // The CPU idles one M-cycle to double speed.
        m == 2
    }

    /// Whether the speed-switch halt has run its length.
    #[inline]
    pub(crate) fn speed_switch_over(&self) -> bool {
        (self.clock() as u32).wrapping_sub(self.sys.vdma.switch_end) as i32 >= 0
    }

    /// As [`Bus::halt_end`], except (gambatte CGB C
    /// captures, ADR 0017 speed-switch amendment):
    /// - OAM DMA resumes one M-cycle later, at the end of the wake-up
    ///   M-cycle (`oamdma/oamdmasrcC0_speedchange_readC000`,
    ///   `dma/hdma_transition_speedchange_oamdma`: a WRAM read after the
    ///   switch sees the transfer one byte earlier).
    ///
    /// As after HALT, an HBlank in the wake-up M-cycle starts no HDMA
    /// block ([`Bus::wake_stalls`] clears `halted`).
    /// The wake-up's own block (HALT began outside the HBlank) is checked
    /// at the end of the wake-up M-cycle, as after HALT, also when the
    /// countdown ends it (gambatte `intevent_unhalt`;
    /// `dma/hdma_m3speedchange_late_m0wakeup_1`), and runs before the next
    /// instruction.
    #[inline]
    pub(crate) fn speed_switch_end(&mut self) {
        let d = &mut self.sys.ppu.dma;
        if d.paused != NEVER {
            d.paused -= self.hot.mcycle as u64;
        }
        self.halt_end();
    }

    /// The halted skip ([`Bus::idle_halted`]), stopping at the halt's
    /// last M-cycle.
    #[inline]
    pub(crate) fn idle_speed_switch(&mut self) {
        let m = self.hot.mcycle as u32;
        let left = self.sys.vdma.switch_end.wrapping_sub(self.clock() as u32) / m;
        let n = self.mcycles_to_deadline().min(left);
        self.hot.budget -= (n * m) as i32;
        if self.hot.budget <= 0 {
            self.boundary();
        }
    }
}

/// CGB VRAM DMA (FF51-FF55; `hdma` module, ADR 0017).
impl Ctx<'_, '_> {
    /// Runs the CPU stall of the current transfer step (`VramDma::stall`
    /// idle M-cycles left), skipped a deadline at a time (ADR 0011), each
    /// deadline handled on the M-cycle it was before. Stops early at the
    /// first deadline that raises a stop event or reaches `limit` (the
    /// call's target; `u64::MAX` for a single step): the run call ends
    /// there and the next one resumes the stall. Without this, a GDMA
    /// stall (up to 4,100 clocks) crossing the `AudioBuffer` deadline
    /// would complete windows with no slot left, dropping them (ADR 0021,
    /// GDMA stall amendment).
    #[inline(never)]
    pub(crate) fn vdma_stall(&mut self, limit: u64) {
        let mut n = u32::from(self.sys.vdma.stall);
        while n != 0 {
            let k = self.mcycles_to_deadline().min(n);
            self.hot.budget -= (k * self.hot.mcycle as u32) as i32;
            n -= k;
            if self.hot.budget <= 0 {
                // Left after this deadline's M-cycle, for `vdma_hblank`:
                // an HBlank trigger before the stall's end is lost.
                self.sys.vdma.stall = n as u16;
                self.boundary();
                if self.events.intersects(self.stop_on) || self.clock() >= limit {
                    break;
                }
            }
        }
        self.sys.vdma.stall = n as u16;
    }

    /// Stores the FF55 read-back where `read_io` reads it.
    fn vdma_sync_reg(&mut self) {
        self.mem.io[0x55] = self.sys.vdma.read_hdma5();
    }

    /// HDMA's HBlank period ([`Ppu::hdma_period`]) at clock `at`: from this
    /// line's trigger ([`hdma::HBLANK_DELAY`] after the mode-0 STAT change)
    /// to [`hdma::HBLANK_UNTIL`] dots into the next line. Also with the
    /// LCD off (a transfer started then runs at once).
    fn hdma_period(&self, at: u64) -> bool {
        let p = &self.sys.ppu;
        !p.lcd_enabled() || p.hdma_period(at, hdma::HBLANK_DELAY, hdma::HBLANK_UNTIL)
    }

    /// A transfer step is due: the CPU stops after this instruction so
    /// the run loop can run it ([`Emulator::run`]).
    fn vdma_request(&mut self) {
        self.sys.vdma.pending = true;
        self.done = true;
    }

    /// FF55 write. An HBlank transfer that starts in HDMA's period as it
    /// is one M-cycle later (gambatte `enableHdma`: `isHdmaPeriod(cc +
    /// 4)`) does its first block now; otherwise it waits for the next
    /// trigger, this line's if still ahead (`dma/hdma_late_enable_*`: an
    /// enable at the HBlank's end starts none).
    #[inline(never)]
    fn write_hdma5(&mut self, value: u8) {
        let now = self.clock();
        if value & 0x80 != 0 {
            // Latch the current line, if due, before the transfer is
            // active.
            self.run_ppu_to_call(now);
            self.schedule_ppu();
        }
        let mode0 = self.hdma_period(now + self.hot.mcycle as u64);
        self.sys.vdma.write_hdma5(value, mode0);
        self.vdma_sync_reg();
        if self.sys.vdma.pending {
            self.done = true;
        }
        if self.sys.vdma.hblank {
            self.vdma_arm(now, false);
        }
    }

    /// Schedules the HBlank trigger of the PPU's current visible line, once
    /// per line. `fire_late`: a trigger at or before `now` fires at the
    /// next boundary (the PPU sync that latched the line ran late by less
    /// than an M-cycle); otherwise it is treated as passed.
    /// Out of line and cold: `run_ppu_to` calls it only with an HDMA
    /// active, and must stay small enough to inline into the PPU register
    /// and palette write paths.
    #[cold]
    #[inline(never)]
    fn vdma_arm(&mut self, now: u64, fire_late: bool) {
        let s = &mut *self.sys;
        let Some(start) = s.ppu.mode0_start() else {
            return;
        };
        if start == s.vdma.line {
            return;
        }
        s.vdma.line = start;
        let at = start + hdma::HBLANK_DELAY;
        if at > now || fire_late {
            s.sched.set(EventKind::VramDma, at);
            // `boundary` reschedules when called from an event handler.
            self.reschedule();
        }
    }

    /// The `VramDma` deadline: an HBlank starts a block, unless the CPU
    /// is halted (SameBoy: `!gb->halted`), the transfer was stopped, or a
    /// block's stall is still running: gambatte acknowledges the request
    /// at a transfer's end, so a trigger during it is lost (CGB C captures
    /// `dma/hdma_transition_{,ei_}halt_late_unhalt_scx1_{1,2}`).
    fn vdma_hblank(&mut self) {
        let v = &mut self.sys.vdma;
        if v.refetch == hdma::RAN {
            v.refetch = 0;
            if ((self.hot.clock() as u32).wrapping_sub(v.switch_end) as i32) < 0 {
                return;
            }
        }
        if v.hblank && !v.halted && v.stall == 0 && self.sys.ppu.lcd_enabled() {
            self.vdma_request();
        }
    }

    /// HALT begins during an HBlank transfer: HBlanks start no blocks
    /// until the wake-up, and the wake-up may start one (`wake_ok`) if the
    /// HALT began outside the HBlank (SameBoy `allow_hdma_on_wake`: STAT
    /// mode at the HALT). The HBlank here is HDMA's period
    /// ([`Self::hdma_period`]), which starts at this line's trigger, not
    /// at the mode-0 STAT change: a HALT between the two counts as before
    /// it (gambatte `dma/hdma_late_m3halt_m0unhalt_scx{1_3,2_2}`, CGB C
    /// captures; `hdma_late_m0halt_*` bound the other side).
    #[cold]
    fn vdma_halt(&mut self) {
        let in_hblank = self.hdma_period(self.clock());
        let v = &mut self.sys.vdma;
        v.halted = true;
        v.wake_ok = !in_hblank;
        // A block requested in HALT's M-cycle (gambatte `Memory::halt`).
        if v.pending {
            v.refetch = hdma::REQUESTED;
        }
    }

    /// The speed-switch halt begins during an HBlank transfer (after the
    /// speed change: `mcycle` is the new speed's). As [`Self::vdma_halt`],
    /// and gambatte samples HDMA for a STOP at the start of its last
    /// M-cycle (`Memory::stop(cc - 4)`), so a block due then was triggered
    /// in STOP's first M-cycle (ADR 0017, speed-switch amendment; CGB C
    /// captures `dma/hdma_late_m3speedchange_*_2`,
    /// `hdma_transition_speedchange_*`):
    /// - Triggered in the last M-cycle: it falls into the halt, which
    ///   loses it (`hdma_late_m3speedchange_{ly_scx1_1,tima_scx1_ds_2}`).
    /// - Triggered in the first: STOP's second byte runs as the next
    ///   opcode (`refetch`; `_inc_` ROMs). To double speed the block runs
    ///   in the halt, which ends the transfer with FF55 bit 7 set and its
    ///   length unchanged (gambatte `Memory::dma` while `halted()`;
    ///   `_hdma5_`, `_hdmadst10_`, `read_hdmadst00`); to single speed it
    ///   runs at the wake-up, before that opcode (`ldaaimm_scx1_ds`).
    #[cold]
    #[inline(never)]
    fn vdma_stop(&mut self) {
        let m = self.hot.mcycle as u64;
        let trigger = self.sys.vdma.line.wrapping_add(hdma::HBLANK_DELAY);
        if trigger + (6 - m) > self.clock() {
            self.sys.vdma.pending = false;
        } else if self.sys.vdma.pending {
            if m == 2 {
                let blocks = self.sys.vdma.blocks;
                self.vram_dma(false);
                let v = &mut self.sys.vdma;
                v.stall = 0;
                v.blocks = blocks;
                v.hblank = false;
                self.vdma_sync_reg();
            }
            self.sys.vdma.refetch = hdma::REQUESTED;
        }
        self.vdma_halt();
    }

    /// The wake-up from a HALT that began outside the HBlank: in HDMA's
    /// period it starts a block (SameBoy `GB_cpu_run`,
    /// `allow_hdma_on_wake`; gambatte `isHdmaPeriod` at the unhalt).
    #[cold]
    fn vdma_wake(&mut self) {
        self.sys.vdma.wake_ok = false;
        if self.sys.vdma.hblank && self.hdma_period(self.clock()) {
            self.vdma_request();
        }
    }

    /// Runs the pending transfer step at an instruction fetch: all blocks
    /// of a GDMA, or one HDMA block, and sets the CPU stall for it
    /// ([`hdma::stall_mcycles`], run by [`Self::vdma_stall`]). The bytes
    /// are copied at once at the start, after the PPU has drawn everything
    /// before it.
    ///
    /// Sources: ROM, cartridge RAM and WRAM are read; VRAM and E000-FFFF
    /// read FF (SameBoy reads the CPU data bus there, which this core does
    /// not model).
    ///
    /// `running`: the CPU is not in the speed-switch halt, so an OAM DMA
    /// transfer runs alongside ([`Self::oam_dma_during_vdma`]).
    #[inline(never)]
    pub(crate) fn vram_dma(&mut self, running: bool) {
        let now = self.clock();
        self.run_ppu_to_call(now);
        if self.sys.ppu.drawing() {
            self.sys.ppu.flush(now, self.mem, self.frame);
        }
        // An OAM DMA's bytes due so far read their source before the
        // VRAM DMA writes VRAM.
        if self.hot.dma != 0 {
            self.dma_sync(now);
        }
        let bank = self.sys.vram_off;
        let mut v = self.sys.vdma;
        let dest0 = v.dest;
        v.pending = false;
        let mut blocks = 0;
        loop {
            // Both counters are 16-aligned (FF52/FF54 bits 0-3 are
            // ignored, and a block adds 16), so a block is one 16-byte
            // run of VRAM: `dest & 0x1FFF` never wraps inside it.
            let block = vdma_block(self.mem, self.sys, v.src);
            let (vram, _) = self.mem.vram.as_chunks_mut::<16>();
            vram[((bank | usize::from(v.dest & 0x1FF0)) >> 4) & 0x3FF] = block;
            v.src = v.src.wrapping_add(16);
            v.dest = v.dest.wrapping_add(16);
            blocks += 1;
            v.blocks = v.blocks.wrapping_sub(1);
            if v.blocks == 0 || v.dest == 0 {
                v.hblank = false;
                break;
            }
            if v.hblank {
                break;
            }
        }
        // At most 1 + 128 * 16 M-cycles: fits. One less for the block a
        // HALT's or speed-switch STOP's latched opcode waited for
        // (`VramDma::refetch`).
        let m = self.hot.mcycle as u32;
        let full = hdma::stall_mcycles(blocks, m);
        v.stall = full as u16;
        v.refetch = if v.refetch == hdma::REQUESTED {
            v.stall -= 1;
            v.switch_end = (now as u32).wrapping_add(full * m);
            hdma::RAN
        } else {
            0
        };
        self.sys.vdma = v;
        self.vdma_sync_reg();
        if self.hot.dma != 0 && running {
            self.oam_dma_during_vdma(now, dest0, blocks);
        }
    }

    /// An OAM DMA transfer whose steps fall inside a VRAM DMA that started
    /// at `now` (`blocks` blocks to VRAM from `dest0`): each such step
    /// writes the VRAM DMA's byte of that moment, not its own, to OAM at
    /// the VRAM DMA's source address & FF (if below A0), and its own byte
    /// is never written. The VRAM DMA moves 2 bytes per M-cycle at single
    /// speed (the step after M-cycle `k` takes byte `2k - 1`), 1 at double
    /// speed (byte `k - 1`); its last M-cycle (no bytes) and the steps
    /// before `now` are normal. Source: gambatte cgb04c captures
    /// `dma/hdma_transition_oamdma_1`, `oamdma/oamdmasrcC000_hdmasrc0000`
    /// (gambatte `Memory::dma` does this; read for behaviour only).
    /// SameBoy skips the step's write too but writes nothing in its place.
    /// Not modelled: what the PPU reads of OAM meanwhile.
    #[cold]
    #[inline(never)]
    fn oam_dma_during_vdma(&mut self, now: u64, dest0: u16, blocks: u32) {
        if self.sys.ppu.dma.tail < dma::LEN {
            self.dma_tail_end();
        }
        let d = self.sys.ppu.dma;
        // Halted (paused): no steps.
        if d.paused != NEVER {
            return;
        }
        let m = u64::from(d.mcycle);
        let per = (self.hot.mcycle as u32 / 2).max(1);
        let len = blocks * 16;
        let bank = self.sys.vram_off;
        let src0 = self.sys.vdma.src.wrapping_sub(len as u16);
        let mut copied = d.copied;
        for k in 1..=len / per {
            let t = now + u64::from(k) * m;
            if t < d.start {
                continue;
            }
            let i = (t - d.start) / m;
            if i >= u64::from(dma::LEN) {
                break;
            }
            let b = (k * per - 1) as u16;
            let p = usize::from(src0.wrapping_add(b) & 0xFF);
            if p < usize::from(dma::LEN) {
                let a = usize::from(dest0.wrapping_add(b) & 0x1FFF);
                self.mem.oam[p] = self.mem.vram[(bank | a) & 0x3FFF];
            }
            copied = i as u8 + 1;
        }
        self.sys.ppu.dma.copied = copied;
        self.sys.ppu.oam_written();
    }
}

impl Bus for Ctx<'_, '_> {
    #[inline(always)]
    fn read(&mut self, addr: u16) -> u8 {
        let v = read::<true>(self.mem, self.sys, &self.hot, addr);
        self.tick();
        v
    }

    #[inline(always)]
    fn write(&mut self, addr: u16, value: u8) {
        self.write_mapped(addr, value);
        self.tick();
    }

    #[inline(always)]
    fn idle(&mut self) {
        self.tick();
    }

    #[inline(always)]
    fn interrupt_enable(&self) -> u8 {
        self.sys.ie
    }

    #[inline(always)]
    fn interrupt_flags(&self) -> u8 {
        self.sys.iflags
    }

    /// The PPU's bits (VBlank, STAT): its edges fall on dots, inside the
    /// M-cycle, so the PPU runs to the half-way point (SameBoy
    /// `GB_cpu_run`: `pending_cycles - 2`, then IF is cleared; 1 dot in
    /// double speed, 2 in single), the bit is cleared, and an edge after
    /// that requests the interrupt again (a PPU deadline it passed
    /// reschedules at the M-cycle's end). CGB04C (`_1` not absorbed, `_2`
    /// absorbed): `m2int_m2irq/m2int_m2irq_late_retrigger_{1,ds_1}`,
    /// `irq_precedence/late_m0irq_retrigger_{scx1_1,ds_1}`,
    /// `m1/lycint143_m1irq_late_retrigger_ds_1`,
    /// `m1/lycint_vblankirq_late_retrigger_ds_1`. The timer and serial
    /// raise theirs at M-cycle boundaries here: after the M-cycle, as
    /// before (`tima/tc00_irq_late_retrigger_ds_2`,
    /// `serial/start_wait_trigger_int8_read_if_ds_2` absorb).
    /// Inlined into the dispatch: out of line, `synth:halt-irq` ran 0.5%
    /// more instructions and `Cpu::run` was 1.1 KB larger (measured).
    #[inline(always)]
    fn push_acknowledge(&mut self, addr: u16, value: u8, ie: u8) -> u8 {
        const PPU: u8 = crate::ppu::INT_VBLANK | crate::ppu::INT_STAT;
        if addr == 0xFF0F {
            return self.push_acknowledge_if(value, ie);
        }
        self.write_mapped(addr, value);
        let pending = ie & self.sys.iflags & INT_MASK;
        let bit = pending.isolate_lowest_one();
        if bit & PPU != 0 {
            // Only if a PPU edge can fall there (its deadline).
            let mid = self.clock() + self.hot.mcycle as u64 / 2;
            if self.sys.sched.deadline(EventKind::Ppu) <= mid {
                self.run_ppu_to_call(mid);
            }
            self.sys.iflags &= !bit;
        }
        self.idle();
        self.sys.iflags &= !(bit & !PPU);
        pending
    }

    #[inline(always)]
    fn set_interrupt_flags(&mut self, value: u8) {
        self.sys.iflags = value & INT_MASK;
    }

    fn event(&mut self, event: Events) {
        self.raise(event);
    }

    #[inline(always)]
    fn run_done(&self) -> bool {
        self.done
    }

    /// The wake-up's HDMA block: SameBoy `GB_cpu_run` checks STAT for it
    /// after the wake-up M-cycle, and runs it after the next opcode fetch,
    /// before that instruction (ADR 0017, amendment). gambatte
    /// `dma/hdma_late_m0unhalt_{1,ds_1}`: a wake-up M-cycle that ends in
    /// the next line's mode 2 starts none.
    #[inline(always)]
    fn wake_stalls(&mut self) -> bool {
        // HBlanks start blocks again after the wake-up M-cycle, not in it
        // (SameBoy clears `halted` after it; gambatte CGB C captures
        // `dma/hdma_m0halt_late_m3unhalt_scx1_2`,
        // `hdma_m0speedchange_late_m3wakeup_scx2_2`).
        self.sys.vdma.halted = false;
        if self.sys.vdma.wake_ok {
            self.vdma_wake();
        }
        self.sys.vdma.pending
    }

    /// SameBoy `stop()`: a button held on a selected P1 line ends STOP at
    /// once, with no DIV reset and no speed switch (Pan Docs: "Using the
    /// STOP instruction"). Otherwise STOP resets DIV (Pan Docs: FF04).
    fn stop_begin(&mut self) -> StopKind {
        if p1_lines(self.sys.buttons, self.sys.joyp_select) != 0x0F {
            return StopKind::Joypad;
        }
        self.stop_div_reset();
        if self.sys.cgb_mode && self.sys.key1 & 0x01 != 0 {
            StopKind::Switch
        } else {
            StopKind::Stop
        }
    }

    fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        Ctx::speed_switch(self, ime, pending)
    }

    fn speed_switch_halt(&mut self) -> bool {
        if self.sys.ie & self.sys.iflags & INT_MASK != 0 || self.speed_switch_over() {
            self.speed_switch_end();
            true
        } else {
            self.idle_speed_switch();
            false
        }
    }

    /// The halted skip (ADR 0011): nothing but a deadline can change IE &
    /// IF, the run's end or a stopped CPU's wake-up, so consume the whole
    /// countdown in one go and handle the deadline, on the same M-cycle
    /// as idling one M-cycle at a time would.
    fn idle_halted(&mut self) {
        let n = self.mcycles_to_deadline();
        // Up to 2^31 clocks when the countdown is capped: in i64.
        let h = &mut self.hot;
        h.budget = (i64::from(h.budget) - i64::from(n) * i64::from(h.mcycle)) as i32;
        self.boundary();
    }

    fn stop_wakeup(&self) -> bool {
        !self.sys.buttons.is_empty()
    }

    /// The CPU's read, without the M-cycle's tick. Cold: only for a
    /// latched opcode.
    #[cold]
    #[inline(never)]
    fn fetch_peek(&mut self, addr: u16) -> u8 {
        read::<true>(self.mem, self.sys, &self.hot, addr)
    }

    /// `VramDma::refetch` is `REQUESTED`: kept while the block is still
    /// `pending` (it runs at the wake-up), cleared otherwise (a switch to
    /// double speed ran it at once).
    fn hdma_refetch(&mut self) -> bool {
        let v = &mut self.sys.vdma;
        // The common case first: nothing to clear (perf: every HALT).
        if v.refetch == 0 {
            return false;
        }
        let r = v.refetch == hdma::REQUESTED;
        if !v.pending {
            v.refetch = 0;
        }
        r
    }

    fn halt_begin(&mut self) {
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        if self.sys.vdma.hblank {
            self.vdma_halt();
        }
    }

    fn halt_end(&mut self) {
        if self.sys.ppu.dma.paused != NEVER {
            self.dma_resume();
        }
    }
}

/// A read as the CPU sees it at the current clock, through shared
/// references: the one read side effect (a conflicting read during an OAM
/// DMA from VRAM) is recorded in a `Cell` ([`dma_read_clear`]), so the CPU
/// and `peek` keep sharing one `read_high` (ADR 0028 amendment). Shared by
/// the CPU bus and
/// [`Emulator::peek`]. `hot` is the live clock: [`Ctx::hot`]
/// during a run call, [`Sys::hot`] otherwise. While an OAM DMA transfer
/// runs, every address goes to `read_high` and the conflict path
/// ([`Hot::dma`]); `peek` and the DMA's own reads clear it
/// ([`Hot::without_dma`]). Echo RAM is out of line in `read_high`, so
/// the inlined dispatch has no compare beyond the region.
///
/// `CPU` selects the CPU's view, where the PPU blocks VRAM and OAM
/// ([`read_vram`], [`read_oam`]); `peek` and the DMA's own reads use the
/// raw memory (SameBoy: DMA reads ignore `vram_read_blocked`).
#[inline(always)]
pub(crate) fn read<const CPU: bool>(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    let a = usize::from(addr);
    match (addr | hot.dma) >> 13 {
        0..=3 => sys.cart.read_rom(addr),
        4 if CPU => read_vram(mem, sys, hot, addr),
        4 => mem.vram[(sys.vram_off | (a & 0x1FFF)) & 0x3FFF],
        5 => sys.cart.read_ram(addr),
        6 => wram(mem, sys, a),
        _ => read_high(mem, sys, hot, addr),
    }
}

/// The conflict for a CPU access to `addr` (below FE00) at `now` during
/// the transfer `d`, and the index of the byte being transferred. No
/// conflict in the start delay. (The restart tail, [`OamDma::tail`], is
/// in `d` as a transfer that started earlier: nothing here for it, as
/// any test here cost every read during a transfer up to 3% Ir on
/// `synth:dma-storm`, measured.)
fn dma_conflict(d: &OamDma, now: u64, mcycle: i32, addr: u16) -> (Conflict, u8) {
    if now < d.start {
        return (Conflict::None, 0);
    }
    // `mcycle` is 2 or 4: a shift, not a u64 division (a libcall on
    // thumbv7em).
    debug_assert!(mcycle == 2 || mcycle == 4);
    let i = ((now - d.start) >> mcycle.trailing_zeros()) as u8;
    (dma::conflict(d.base, d.base + u16::from(i), addr), i)
}

/// A CPU read while an OAM DMA transfer runs (`Hot::dma`): OAM reads FF
/// (Pan Docs: OAM DMA bus conflicts; SameBoy `read_high_memory`), HRAM and
/// I/O are unaffected, and the rest follows [`dma::conflict`]. Side-effect
/// free like [`read`], except a conflicting read with a VRAM source
/// ([`dma_read_clear`]).
#[inline(never)]
fn read_during_dma(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    let now = hot.clock();
    let plain = hot.without_dma();
    if addr >= 0xFE00 {
        if addr < 0xFF00 && now >= sys.ppu.dma.block_from {
            return 0xFF;
        }
        if addr == 0xFF41 {
            return sys.ppu.stat_during_dma(now, hot.mcycle == 2);
        }
        return read_high(mem, sys, &plain, addr);
    }
    let addr = match dma_conflict(&sys.ppu.dma, now, hot.mcycle, addr) {
        (Conflict::None, _) => addr,
        (Conflict::Open, _) => return 0xFF,
        (Conflict::Wram(a), _) => a,
        (Conflict::DmaVram, i) => {
            dma_read_clear(mem, i);
            sys.ppu.dma.base + u16::from(i)
        }
        (Conflict::Dma, i) => sys.ppu.dma.base + u16::from(i),
    };
    read::<true>(mem, sys, &plain, addr)
}

/// A CPU read on the VRAM bus in the M-cycle of byte `i` of a transfer
/// from VRAM (`Conflict::DmaVram`): besides returning the byte the DMA reads,
/// on CGB C it clears OAM byte `i` (gambatte cgb04c
/// `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`, which gambatte
/// `Memory::nontrivial_read` reproduces; ADR 0028 amendment). Recorded
/// here and applied by the next `Ctx::dma_sync`, which copies byte `i`
/// first (it is due by now), so the lazy copy can't overwrite the 00.
/// Inline: a call here would cost every read during a transfer saved
/// registers (measured on `synth:dma-storm`).
#[inline(always)]
fn dma_read_clear(mem: &Memory, i: u8) {
    mem.dma_read_clears.record(i);
}

/// One VRAM DMA block's 16 source bytes from `src` (16-aligned, so the
/// block never crosses a region or bank boundary): ROM, cartridge RAM and
/// WRAM are read as [`read`] reads them, VRAM and E000-FFFF read FF. A
/// region that is one run of memory (the ROM bank window, plain cartridge
/// RAM, a WRAM bank) is copied without the per-byte dispatch.
#[inline(always)]
fn vdma_block(mem: &Memory, sys: &Sys, src: u16) -> [u8; 16] {
    debug_assert!(src & 0xF == 0);
    match src >> 13 {
        0..=3 | 5 => sys.cart.dma_block(src),
        6 => *wram_block(mem, sys, usize::from(src)),
        // VRAM and E000-FFFF: FF (see `Ctx::vram_dma`).
        _ => [0xFF; 16],
    }
}

/// 16 bytes of C000-DFFF from a 16-aligned `a`, as [`wram`] reads them.
#[inline(always)]
fn wram_block<'m>(mem: &'m Memory, sys: &Sys, a: usize) -> &'m [u8; 16] {
    let off = if a & 0x1000 == 0 {
        a & 0x0FF0
    } else {
        sys.wram_off | (a & 0x0FF0)
    };
    let (blocks, _) = mem.wram.as_chunks::<16>();
    &blocks[(off >> 4) & 0x7FF]
}

/// C000-DFFF and its echo: bank 0, then the SVBK bank.
#[inline(always)]
fn wram(mem: &Memory, sys: &Sys, a: usize) -> u8 {
    if a & 0x1000 == 0 {
        mem.wram[a & 0x0FFF]
    } else {
        mem.wram[(sys.wram_off | (a & 0x0FFF)) & 0x7FFF]
    }
}

#[inline(always)]
fn wram_mut<'m>(mem: &'m mut Memory, sys: &Sys, a: usize) -> &'m mut u8 {
    if a & 0x1000 == 0 {
        &mut mem.wram[a & 0x0FFF]
    } else {
        &mut mem.wram[(sys.wram_off | (a & 0x0FFF)) & 0x7FFF]
    }
}

/// FEA0-FEFF on CGB revision C: RAM with address bits 3-4 ignored (SameBoy
/// `GB_read_oam`/`write_oam`: `addr &= ~0x18`). TODO(M3): reads 0xFF while
/// the PPU blocks OAM (Pan Docs: "FEA0-FEFF range").
#[inline]
fn extra_oam_index(a: usize) -> usize {
    ((a & 0xFF) & !0x18) - 0xA0
}

/// The palette RAM byte BCPD (`reg` 0x69) or OCPD (0x6B) reads, at the
/// BCPS/OCPS index.
fn palette_byte(mem: &Memory, reg: usize) -> u8 {
    let ram = if reg == 0x6B {
        &mem.obj_pal
    } else {
        &mem.bg_pal
    };
    ram[usize::from(mem.io[reg - 1] & 0x3F)]
}

/// The rare I/O reads (`read_io`'s `_` arm), `reg` the low 7 address bits;
/// FF unless listed. Out of line, for `read_high`'s code-size budget.
#[inline(never)]
fn read_io_other(mem: &Memory, sys: &Sys, hot: &Hot, reg: usize) -> u8 {
    let cgb = sys.cgb_mode;
    match reg {
        // BCPD/OCPD (CGB mode) read the palette RAM byte at the index,
        // without incrementing it (SameBoy `read_high_memory`; Pan Docs:
        // FF69), and FF while the PPU blocks palette RAM
        // ([`Ppu::palette_blocked`]; Pan Docs: LCD Color Palettes).
        0x69 | 0x6B if cgb && !sys.ppu.palette_blocked(hot.clock(), hot.mcycle == 2) => {
            palette_byte(mem, reg)
        }
        // FF74 (GB_IO_PSW): R/W in CGB mode only (see `read_io`).
        0x74 if cgb => mem.io[reg],
        _ => 0xFF,
    }
}

/// A CPU read of VRAM: FF while the PPU blocks it ([`Ppu::vram_blocked`]).
/// Out of line: the check needs the PPU's position.
#[inline(never)]
fn read_vram(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    if sys.ppu.vram_blocked(hot.clock(), hot.mcycle == 2) {
        return 0xFF;
    }
    mem.vram[(sys.vram_off | (usize::from(addr) & 0x1FFF)) & 0x3FFF]
}

/// A CPU read of FE00-FEFF: FF while the PPU blocks OAM
/// ([`Ppu::oam_blocked`]), FEA0-FEFF included.
#[inline(never)]
fn read_oam(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    if sys.ppu.oam_blocked(hot.clock(), hot.mcycle == 2, false) {
        return 0xFF;
    }
    raw_oam(mem, addr)
}

/// FE00-FEFF as stored.
#[inline]
fn raw_oam(mem: &Memory, addr: u16) -> u8 {
    let a = usize::from(addr);
    if addr < 0xFEA0 {
        mem.oam[a & 0xFF]
    } else {
        mem.extra_oam[extra_oam_index(a)]
    }
}

/// E000-FFFF, and every address during an OAM DMA transfer (CPU only:
/// `peek` reads FE00-FEFF itself).
fn read_high(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    if hot.dma != 0 {
        return read_during_dma(mem, sys, hot, addr);
    }
    let a = usize::from(addr);
    match addr {
        // E000-FDFF: echo of C000-DDFF (Pan Docs: Memory Map).
        0..=0xFDFF => wram(mem, sys, a),
        0xFE00..=0xFEFF => read_oam(mem, sys, hot, addr),
        0xFF00..=0xFF7F => read_io(mem, sys, hot, a & 0x7F),
        0xFF80..=0xFFFE => mem.hram[a - 0xFF80],
        _ => sys.ie,
    }
}

/// FF00-FF7F by the low 7 address bits. Unused bits read 1; registers
/// that don't exist (or are CGB-only, in DMG compatibility mode) read FF.
fn read_io(mem: &Memory, sys: &Sys, hot: &Hot, reg: usize) -> u8 {
    let cgb = sys.cgb_mode;
    match reg {
        // P1/JOYP (Pan Docs: Joypad Input).
        0x00 => 0xC0 | sys.joyp_select | p1_lines(sys.buttons, sys.joyp_select),
        0x01 => sys.serial.sb,
        0x02 => sys.serial.read_sc(cgb),
        0x04..=0x07 => sys.timer.read(hot.clock(), reg),
        0x0F => 0xE0 | sys.iflags,
        // NR52, wave RAM and PCM12/PCM34 depend on the APU state.
        0x26 | 0x30..=0x3F | 0x76 | 0x77 => apu::read_apu(mem, hot.clock(), reg),
        // Sound: write-only and unused bits read 1 (`SOUND_READ_MASK`;
        // power off zeroes the stored bytes).
        0x10..=0x2F => mem.io[reg] | SOUND_READ_MASK[(reg - 0x10) & 0x1F],
        0x40 => sys.ppu.lcdc,
        0x41 => sys.ppu.stat(hot.clock(), hot.mcycle == 2),
        0x44 => sys.ppu.ly(hot.clock(), hot.mcycle == 2),
        0x45 => sys.ppu.lyc,
        // FF55 (HDMA5): its read-back, kept in `io` by the VRAM DMA
        // (`Ctx::vdma_sync_reg`; FF in DMG compatibility mode), so
        // `read_high` stays small. FF51-FF54 are write-only (`_`: FF).
        0x42 | 0x43 | 0x46..=0x4B | 0x55 => mem.io[reg],
        // FF51-FF54 are write-only (the `_` arm: FF).
        0x4D if cgb => 0x7E | sys.key1 | if hot.mcycle == 2 { 0x80 } else { 0 },
        // VBK reads FE | bank on a CGB in either mode (SameBoy
        // `read_high_memory`: only `GB_is_cgb`; Mooneye `misc/boot_hwio-C`
        // reads FE in DMG compatibility mode). Writes need CGB mode.
        0x4F => 0xFE | sys.vbk,
        // BCPS/OCPS: bit 6 reads 1, in either mode (SameBoy
        // `read_high_memory`; Mooneye `misc/bits/unused_hwio-C`).
        0x68 | 0x6A => 0x40 | mem.io[reg],
        // BCPD/OCPD: `read_io_other`.
        0x70 if cgb => 0xF8 | sys.svbk,
        // Undocumented CGB registers, as in SameBoy (Core/memory.c,
        // `read_high_memory`, the I/O part of `GB_read_memory`): OPRI
        // reads `| 0xFE` on CGB in either mode; FF72 and FF73
        // (GB_IO_PSWX/PSWY) are fully R/W on CGB; FF74 (GB_IO_PSW) is R/W
        // in CGB mode only and reads FF otherwise; FF75 (GB_IO_PGB) has
        // only bits 4-6 R/W and reads `| 0x8F`. The model is always a CGB
        // (ADR 0003), so "on CGB" needs no check.
        0x6C => 0xFE | mem.io[reg],
        0x72 | 0x73 => mem.io[reg],
        // FF74: `read_io_other`.
        0x75 => 0x8F | mem.io[reg],
        // FF76/FF77 PCM12/PCM34 (read only, on CGB in either mode; Mooneye
        // `misc/bits/unused_hwio-C` reads 00 with the APU silent): above.
        _ => read_io_other(mem, sys, hot, reg),
    }
}

impl Emulator<'_> {
    /// Reads a byte as the CPU would see it at `addr` at the current clock,
    /// without side effects and ignoring PPU access blocking. Debug access
    /// for tools and tests; not used by emulation. Reads past the end of a
    /// short ROM return 0xFF, as if it were padded to a full bank
    /// ([`MIN_ROM_SIZE`](crate::MIN_ROM_SIZE)).
    pub fn peek(&self, addr: u16) -> u8 {
        let sys = &self.hw.sys;
        // Conflict-free: the memory as it is, OAM included, during a DMA,
        // and VRAM/OAM whatever the PPU mode.
        if (0xFE00..0xFF00).contains(&addr) {
            return raw_oam(self.mem, addr);
        }
        // Palette RAM whatever the PPU mode, too.
        if matches!(addr, 0xFF69 | 0xFF6B) && sys.cgb_mode {
            return palette_byte(self.mem, usize::from(addr & 0x7F));
        }
        read::<false>(self.mem, sys, &sys.hot.without_dma(), addr)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::{Config, FRAME_CLOCKS, Frame, cart_ram_size};
    use std::boxed::Box;
    use std::vec;

    /// An emulator with `rom` loaded and a RAM buffer of the header's size.
    fn load(rom: &'static [u8]) -> &'static mut Emulator<'static> {
        let ram = vec![0u8; cart_ram_size(rom).unwrap()].leak();
        let emu = crate::tests::fresh();
        emu.load(rom, ram, Config::default()).unwrap();
        emu
    }

    fn rom(len: usize, cart_type: u8, ram_code: u8, cgb_flag: u8) -> &'static mut [u8] {
        let rom = vec![0u8; len].leak();
        rom[0x143] = cgb_flag;
        rom[0x147] = cart_type;
        rom[0x149] = ram_code;
        rom
    }

    /// A CPU write through the bus (no M-cycle).
    fn write(emu: &mut Emulator, addr: u16, value: u8) {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write_mapped(addr, value);
    }

    /// The per-byte VRAM DMA source read before the block fast path: the
    /// reference for `vdma_block`.
    fn vdma_block_ref(mem: &Memory, sys: &Sys, src: u16) -> [u8; 16] {
        let hot = Hot::new();
        core::array::from_fn(|i| {
            let a = src.wrapping_add(i as u16);
            if a < 0x8000 || matches!(a & 0xE000, 0xA000 | 0xC000) {
                read::<false>(mem, sys, &hot, a)
            } else {
                0xFF
            }
        })
    }

    /// The VRAM DMA block fast path (`vdma_block`) reads what the per-byte
    /// path (`vdma_block_ref`) reads, for every 16-aligned source, in
    /// each source region and mapper state: ROM banks (one window ending
    /// mid-block), cartridge RAM disabled / banked / an RTC register,
    /// WRAM banks via SVBK (0 maps bank 1), VRAM and E000-FFFF.
    #[test]
    fn vdma_block_fast_path_matches_per_byte_reads() {
        // MBC3+TIMER+RAM+BATTERY, 32 KiB RAM; 0x8008 bytes, so ROM bank 2
        // is an 8-byte window.
        let r = rom(0x8008, 0x10, 0x03, 0x80);
        for (i, b) in r.iter_mut().enumerate().skip(0x150) {
            *b = (i * 7 + (i >> 8)) as u8;
        }
        let emu = load(r);
        for (i, b) in emu.mem.wram.iter_mut().enumerate() {
            *b = (i * 5 + (i >> 12) * 33) as u8;
        }
        for (i, b) in emu.mem.vram.iter_mut().enumerate() {
            *b = i as u8;
        }
        write(emu, 0x0000, 0x0A); // RAM and RTC enabled
        for i in 0..0x8000u32 {
            write(emu, 0x4000, (i >> 13) as u8);
            write(
                emu,
                0xA000 | (i & 0x1FFF) as u16,
                (i * 3 + (i >> 13) * 71) as u8,
            );
        }
        let check = |emu: &Emulator, what: &str| {
            for src in (0..=0xFFF0u16).step_by(16) {
                assert_eq!(
                    vdma_block(emu.mem, &emu.hw.sys, src),
                    vdma_block_ref(emu.mem, &emu.hw.sys, src),
                    "{what}: {src:04X}"
                );
            }
        };
        for (what, writes) in [
            (
                "ROM bank 1, RAM bank 0, SVBK 0",
                &[(0x2000, 1), (0x4000, 0), (0xFF70, 0)][..],
            ),
            (
                "ROM bank 2 (partial), RAM bank 2, SVBK 3",
                &[(0x2000, 2), (0x4000, 2), (0xFF70, 3)],
            ),
            (
                "RAM bank 3, SVBK 7",
                &[(0x2000, 1), (0x4000, 3), (0xFF70, 7)],
            ),
            ("RTC seconds", &[(0x4000, 0x08)]),
            ("RAM disabled", &[(0x4000, 1), (0x0000, 0x00)]),
        ] {
            for &(a, v) in writes {
                write(emu, a, v);
            }
            check(emu, what);
        }
        // The block path sees the WRAM bank: bank 7 at D000.
        assert_eq!(
            vdma_block(emu.mem, &emu.hw.sys, 0xD010)[0],
            emu.mem.wram[0x7010]
        );
    }

    #[test]
    fn clock_countdown_is_capped_and_exact() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF40, 0x11); // LCD off: no event scheduled
        write(emu, 0xFF26, 0x00); // APU off: no frame-sequencer events
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let start = emu.clock();
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // Nothing due: the countdown is capped to fit an i32.
        assert_eq!((ctx.hot.budget, ctx.clock()), (i32::MAX, start));
        // Reaching a (capped) deadline mid-M-cycle just reschedules, and
        // the clock stays exact across it.
        ctx.hot.budget = 6;
        ctx.hot.deadline = start + 6;
        ctx.idle();
        assert_eq!((ctx.hot.budget, ctx.clock()), (2, start + 4));
        ctx.idle();
        assert_eq!((ctx.hot.budget, ctx.clock()), (i32::MAX, start + 8));
        assert!(!ctx.done);
        drop(ctx);
        assert_eq!(emu.clock(), start + 8);

        // A target ends the call at the first M-cycle boundary at or after
        // it.
        let now = emu.clock();
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, now + 13);
        for _ in 0..3 {
            ctx.idle();
        }
        assert!(!ctx.done);
        ctx.idle();
        assert!(ctx.done);
        assert_eq!(ctx.clock(), now + 16);
        assert_eq!(ctx.hot.budget, i32::MAX);
    }

    #[test]
    fn idle_halted_consumes_the_whole_countdown() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF40, 0x11); // LCD off: no event scheduled
        write(emu, 0xFF26, 0x00); // APU off: no frame-sequencer events
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // (M-cycle, budget before) -> clocks idled: up to the first
        // M-cycle boundary at or after the deadline, at least one M-cycle,
        // then the deadline is handled (the capped countdown is renewed).
        for (mcycle, budget, idled) in [
            (4, 6, 8),
            (4, 8, 8),
            (4, 1, 4),
            (4, 0, 4),
            (4, -3, 4),
            (2, 7, 8),
            (4, i32::MAX, 1 << 31),
            (2, i32::MAX, 1 << 31),
        ] {
            let start = ctx.clock();
            ctx.hot.mcycle = mcycle;
            ctx.hot.budget = budget;
            ctx.hot.deadline = start.wrapping_add_signed(budget.into());
            ctx.idle_halted();
            assert_eq!(ctx.clock() - start, idled, "{mcycle} {budget}");
            assert_eq!(ctx.hot.budget, i32::MAX, "{mcycle} {budget}");
        }
    }

    #[test]
    fn echo_ram_and_wram_banks() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xC123, 0x11);
        assert_eq!(emu.peek(0xE123), 0x11);
        write(emu, 0xFDFF, 0x22);
        assert_eq!(emu.peek(0xDDFF), 0x22);
        // CGB mode: SVBK selects the bank at D000; 0 means 1.
        write(emu, 0xD000, 0x01);
        write(emu, 0xFF70, 2);
        assert_eq!(emu.peek(0xFF70), 0xFA);
        assert_eq!(emu.peek(0xD000), 0x00);
        write(emu, 0xD000, 0x02);
        assert_eq!(emu.peek(0xF000), 0x02);
        write(emu, 0xFF70, 0);
        assert_eq!(emu.peek(0xD000), 0x01);
        // VBK.
        write(emu, 0x8000, 0xA0);
        write(emu, 0xFF4F, 1);
        assert_eq!(emu.peek(0xFF4F), 0xFF);
        assert_eq!(emu.peek(0x8000), 0x00);
        write(emu, 0xFF4F, 0);
        assert_eq!(emu.peek(0x8000), 0xA0);
    }

    #[test]
    fn cgb_registers_are_locked_in_dmg_compatibility_mode() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        write(emu, 0xD000, 0x01);
        write(emu, 0xFF70, 2);
        write(emu, 0xFF4F, 1);
        write(emu, 0xFF4D, 1);
        for reg in [0xFF4D, 0xFF70] {
            assert_eq!(emu.peek(reg), 0xFF, "{reg:#06x}");
        }
        // VBK is readable (bank 0) but not writable.
        assert_eq!(emu.peek(0xFF4F), 0xFE);
        assert_eq!(emu.peek(0xD000), 0x01);
        // BCPS/OCPS are readable and writable, BCPD/OCPD are not.
        write(emu, 0xFF68, 0x05);
        write(emu, 0xFF69, 0x12);
        assert_eq!((emu.peek(0xFF68), emu.peek(0xFF69)), (0x45, 0xFF));
    }

    /// A DMG-mode emulator whose BG is tile 0 (color 3) in even map
    /// columns and tile 1 (color 0) in odd ones.
    fn striped_bg() -> &'static mut Emulator<'static> {
        striped_bg_with(0x00)
    }

    /// [`striped_bg`] with header byte 0x143 = `cgb_flag`.
    fn striped_bg_with(cgb_flag: u8) -> &'static mut Emulator<'static> {
        let emu = load(rom(0x8000, 0, 0, cgb_flag));
        emu.mem.vram[..16].fill(0xFF);
        for col in 0..32 {
            emu.mem.vram[0x1800 + col] = (col & 1) as u8;
        }
        emu
    }

    /// Runs `emu` to line position `t` of line `ly` in the first full
    /// frame (an M-cycle boundary: `t` is 3 mod 4), writes `value` to
    /// `reg` there, and runs past the line's end. Returns the line's
    /// pixels.
    fn write_in_mode3(emu: &mut Emulator, ly: usize, t: u64, reg: u16, value: u8) -> [u16; 160] {
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * ly as u64;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + t, &mut fb, &mut []);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert_eq!(ctx.clock(), line + t);
        ctx.write(reg, value);
        while ctx.clock() < line + 400 {
            ctx.idle();
        }
        drop(ctx);
        fb.as_chunks::<160>().0[ly]
    }

    /// A BGP write in mode 3 changes the pixels from the one being output
    /// one dot before the access on (SameBoy `GB_CONFLICT_PALETTE_CGB`,
    /// CGB C); the ones before keep the old colors.
    #[test]
    fn mid_mode3_bgp_write_splits_the_line() {
        let emu = striped_bg();
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        // No SCX fine scroll, no objects: pixel x is output at t = 96 + x.
        // The write at t = 131 lands at 130: pixels 0-33 are out (34 at
        // the access itself; both in a color-3 tile).
        let row = write_in_mode3(emu, 5, 131, 0xFF47, 0x3C);
        for (x, &p) in row.iter().enumerate() {
            let color3 = (x / 8) % 2 == 0;
            // BGP FC: color 3 -> shade 3; 3C: color 3 -> shade 0, color 0
            // -> shade 0 either way.
            let want = match (color3, x < 34) {
                (true, true) => gray[3],
                _ => gray[0],
            };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// BCPD/OCPD writes store at the BCPS/OCPS index and, with bit 7 set,
    /// increment it with bits 0-5 wrapping (bit 7 kept, bit 6 reads 1);
    /// reads return the byte at the index without incrementing (Pan Docs:
    /// LCD Color Palettes; SameBoy `write_high_memory`/`read_high_memory`).
    #[test]
    fn cgb_palette_data_auto_increments_and_reads_back() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF68, 0xBE); // auto-increment, index 3E
        for v in [0x11, 0x22, 0x33] {
            write(emu, 0xFF69, v);
        }
        assert_eq!(emu.peek(0xFF68), 0xC1);
        assert_eq!((emu.mem.bg_pal[0x3E], emu.mem.bg_pal[0x3F]), (0x11, 0x22));
        assert_eq!(emu.mem.bg_pal[0], 0x33);
        // No auto-increment: writes and reads stay at the index.
        write(emu, 0xFF68, 0x3F);
        assert_eq!((emu.peek(0xFF69), emu.peek(0xFF69)), (0x22, 0x22));
        write(emu, 0xFF69, 0x44);
        assert_eq!((emu.peek(0xFF68), emu.peek(0xFF69)), (0x7F, 0x44));
        // OCPD: the same, on OBJ palette RAM.
        write(emu, 0xFF6A, 0xBF);
        write(emu, 0xFF6B, 0x55);
        write(emu, 0xFF6B, 0x66);
        assert_eq!(emu.peek(0xFF6A), 0xC1);
        write(emu, 0xFF6A, 0x3F);
        assert_eq!(emu.peek(0xFF6B), 0x55);
        write(emu, 0xFF6A, 0x00);
        assert_eq!(emu.peek(0xFF6B), 0x66);
        assert_eq!(emu.mem.bg_pal[0x3F], 0x44);
    }

    /// Mode 3 (CGB mode): a BCPD/OCPD read gives FF and a write is dropped
    /// but still steps the index; after mode 3 both work again (Pan Docs:
    /// LCD Color Palettes; SameBoy `cgb_palettes_blocked`).
    #[test]
    fn cgb_palette_data_is_blocked_in_mode3() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * 5;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + 150, &mut fb, &mut []);
        emu.mem.bg_pal[0] = 0x5A;
        emu.mem.obj_pal[1] = 0xA5;
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write(0xFF68, 0x80);
        ctx.write(0xFF6A, 0x01);
        let reads = [ctx.read(0xFF69), ctx.read(0xFF6B)];
        ctx.write(0xFF69, 0x12);
        ctx.write(0xFF6B, 0x34);
        while ctx.clock() < line + 400 {
            ctx.idle();
        }
        let after = [ctx.read(0xFF69), ctx.read(0xFF6B)];
        drop(ctx);
        assert_eq!(reads, [0xFF, 0xFF]);
        // Not stored; BCPS stepped (auto-increment), OCPS not.
        assert_eq!((emu.mem.bg_pal[0], emu.mem.obj_pal[1]), (0x5A, 0xA5));
        assert_eq!((emu.mem.io[0x68], emu.mem.io[0x6A]), (0x81, 0x01));
        // HBlank: readable again (BCPS now at byte 1).
        assert_eq!(after, [emu.mem.bg_pal[1], 0xA5]);
    }

    /// An OCPD write resolves its OBJ color right away (bit 15 is not a
    /// color bit; Pan Docs: LCD Color Palettes).
    #[test]
    fn ocpd_write_sets_the_obj_color() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF6A, 0x80 | 0x0E); // OBJ palette 1, color 3
        write(emu, 0xFF6B, 0x34);
        write(emu, 0xFF6B, 0x92);
        assert_eq!(emu.hw.sys.ppu.colors.obj[1][3], 0x1234);
    }

    /// Line 5 of the first full frame of `emu` (striped BG).
    fn line5(emu: &mut Emulator) -> [u16; 160] {
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * 5;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + 400, &mut fb, &mut []);
        fb.as_chunks::<160>().0[5]
    }

    /// CGB mode: BG colors come from palette RAM (BG palette 0 here, no
    /// attributes yet); BGP has no effect; the other colors are the boot
    /// ROM's white.
    #[test]
    fn cgb_mode_bg_colors_come_from_palette_ram() {
        let emu = striped_bg_with(0x80);
        // Off clock 0 (the BGP write path looks one dot back).
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(100, &mut fb, &mut []);
        write(emu, 0xFF68, 0x86); // BG palette 0, color 3, auto-increment
        write(emu, 0xFF69, 0x20);
        write(emu, 0xFF69, 0x03);
        write(emu, 0xFF47, 0x00);
        let row = line5(emu);
        for (x, &p) in row.iter().enumerate() {
            let want = if (x / 8) % 2 == 0 { 0x0320 } else { 0x7FFF };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// DMG compatibility mode: BCPD writes are ignored, BGP picks the
    /// compatibility colors as before.
    #[test]
    fn dmg_compat_bg_colors_ignore_palette_ram() {
        let emu = striped_bg();
        write(emu, 0xFF68, 0x80);
        for _ in 0..8 {
            write(emu, 0xFF69, 0x1F);
        }
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        let row = line5(emu);
        for (x, &p) in row.iter().enumerate() {
            // Post-boot BGP = FC: color 3 -> shade 3, color 0 -> shade 0.
            let want = if (x / 8) % 2 == 0 { gray[3] } else { gray[0] };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// A WX write in mode 3 that brings the window on screen starts it if
    /// it lands (one dot after the access, `GB_CONFLICT_WRITE_CPU`) by the
    /// window stall's dot (`ppu::WX_ON_LEAD`), as one that moves it off
    /// screen removes it (gambatte `window/late_wx_ff_0f_{1,2}`, CGB04C).
    #[test]
    fn wx_write_starts_the_window_up_to_its_stall() {
        // WY = 0, LCDC.5 set, WX FF -> 0F on line 5: the window (map 9800
        // column 0, dark) at x = 8 over the BG's column 1 (light); its
        // stall would begin at dot 104.
        let run = |t: u64| {
            let emu = striped_bg();
            let Emulator { mem, hw } = &mut *emu;
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.write(0xFF4B, 0xFF);
            ctx.write(0xFF4A, 0);
            ctx.write(0xFF40, 0xB1);
            drop(ctx);
            write_in_mode3(emu, 5, t, 0xFF4B, 0x0F)
        };
        let row = run(103);
        assert_eq!(row[8], row[0], "lands at 104: the window at x = 8");
        let row = run(107);
        assert_ne!(row[8], row[0], "lands at 108: too late");
    }

    /// A SCX write in mode 3 (landing at the access, `GB_CONFLICT_READ_OLD`)
    /// moves the BG from the first tile the fetcher has not read the map
    /// entry of yet (`Ppu::hold_fetch`).
    #[test]
    fn mid_mode3_scx_write_splits_the_line() {
        let emu = striped_bg();
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        // The write at t = 151: pixels 0-54 are out with SCX = 0, tile 6
        // (pixels 48-55) is in the FIFO and tile 7 (56-63) was fetched with
        // SCX = 0 too.
        let row = write_in_mode3(emu, 7, 151, 0xFF43, 8);
        for (x, &p) in row.iter().enumerate() {
            let scx = if x < 64 { 0 } else { 8 };
            let want = if ((x + scx) / 8) % 2 == 0 {
                gray[3]
            } else {
                gray[0]
            };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// Double speed: an SCX write reaches the fetcher one dot before the
    /// access (SameBoy `GB_CONFLICT_SCX_DMG_AND_CGB_DOUBLE`; gambatte
    /// `scx_during_m3_ds_*`). The access at t = 145, one dot after tile 6's
    /// first pixel (x = 48) is output, lands at 144, before tile 7's map
    /// entry is read (`FETCH_SAMPLED`): tile 7 (56-63) has the new SCX.
    #[test]
    fn double_speed_scx_write_lands_a_dot_before_the_access() {
        let emu = striped_bg();
        let ly = 7;
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * ly as u64;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + 143, &mut fb, &mut []);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // 2-clock M-cycles (`write_ppu` keys double speed on them; the
        // switch's own timing is not under test).
        ctx.hot.mcycle = 2;
        ctx.idle();
        assert_eq!(ctx.clock(), line + 145);
        ctx.write(0xFF43, 8);
        while ctx.clock() < line + 400 {
            ctx.idle();
        }
        drop(ctx);
        let row = fb.as_chunks::<160>().0[ly];
        assert_ne!(row[47], row[48], "SCX = 0 up to tile 6");
        assert_eq!(row[56], row[48], "tile 7 with SCX = 8");
        assert_ne!(row[64], row[48], "tile 8 with SCX = 8");
    }

    /// Interrupt dispatch, last M-cycle: the STAT bit is cleared half-way
    /// into it (`Bus::acknowledge`), so a STAT edge up to there is
    /// absorbed and one after it requests the interrupt again (gambatte
    /// `m2int_m2irq/m2int_m2irq_late_retrigger_{1,ds_1}`), in both speeds.
    #[test]
    fn dispatch_acknowledges_ppu_interrupts_half_way_into_its_last_mcycle() {
        // The mode-2 interrupt (STAT bit 5) of line 5 (t = 3) and of the
        // next frame's line 0 (t = 4, double speed 5: `edges::ly0_pulse`): on
        // the CPU's grid (M-cycles from clock 0 mod 4), the first is at an
        // M-cycle boundary and the second one clock past it.
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        let line0 = line5 + 149 * 456;
        // (M-cycle clocks, edge, last M-cycle start before it, absorbed)
        for (m, edge, before, absorbed) in [
            (4, line5 + 3, 4, false),
            (4, line0 + 4, 1, true),
            (2, line5 + 3, 2, false),
            // `lyc153int_m2irq/lyc153int_m2irq_late_retrigger_ds_1`: cleared
            // at 0:4, before the edge; `_ds_2`: at 0:6, after it.
            (2, line0 + 5, 2, false),
            (2, line0 + 5, 0, true),
        ] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let start = edge - before;
            let _ = emu.run_until(start - 40, &mut fb, &mut []);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.write(0xFF41, 0x20);
            ctx.hot.mcycle = m;
            ctx.sys.ppu.speed_changed(m == 2);
            while ctx.clock() < start {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), start);
            ctx.sys.iflags = crate::ppu::INT_STAT;
            let _ = crate::cpu::Bus::push_acknowledge(&mut ctx, 0xC000, 0, crate::ppu::INT_STAT);
            let stat = ctx.sys.iflags & crate::ppu::INT_STAT != 0;
            assert_eq!(
                stat, !absorbed,
                "M-cycle {m}, edge {edge}, last from - {before}"
            );
        }
    }

    /// A LYC write that makes LY = LYC with the LYC select set: in single
    /// speed the interrupt reaches IF one M-cycle after the write's
    /// (gambatte `cc + 5`, CGB04C `lycEnable/lyc_ff45_trigger_delay_2`),
    /// also when a walk point syncs the PPU in between; in double speed at
    /// once. An IF write in between doesn't cancel it.
    #[test]
    fn lyc_write_interrupt_reaches_if_one_mcycle_later_in_single_speed() {
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        // (M-cycle clocks, SCX, write at line 5 dot, IF write after it,
        // M-cycles to IF). Around the mode-3 end (a PPU deadline, 256 +
        // SCX % 8 here), with SCX 1-3 it falls inside the next M-cycle.
        let end = (240..270).flat_map(|t| (0..8).map(move |scx| (4, scx, t, false, 1)));
        let fixed = [
            (4, 0, 100, false, 1),
            (4, 0, 100, true, 1),
            (2, 0, 100, false, 0),
        ];
        for (m, scx, t, if_write, later) in fixed.into_iter().chain(end) {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line5 - 40, &mut fb, &mut []);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.hot.mcycle = m;
            ctx.sys.ppu.speed_changed(m == 2);
            ctx.write(0xFF41, 0x40);
            ctx.write(0xFF43, scx);
            while ctx.clock() < line5 + t {
                ctx.idle();
            }
            ctx.sys.iflags = 0;
            crate::cpu::Bus::write(&mut ctx, 0xFF45, 5);
            if if_write {
                ctx.write_mapped(0xFF0F, 0);
            }
            for i in 0..3 {
                let stat = ctx.sys.iflags & crate::ppu::INT_STAT != 0;
                assert_eq!(stat, i >= later, "{m}, {scx}, {t}, {if_write}, {i}");
                ctx.idle();
            }
        }
    }

    /// An IF write lands one dot after its access (SameBoy
    /// `GB_CONFLICT_WRITE_CPU`): it clears a mode-0 interrupt requested
    /// up to that dot, not one after it (CGB04C
    /// `m2int_m0irq/m2int_m0irq_scx3_ifw_*`). Single speed, line 153's
    /// LYC=0 point and line 0's mode-2 pulse (one dot past an M-cycle
    /// boundary here) count as a dot later: an IF write accessed the dot
    /// before them doesn't clear them (`ly0/lycint152_lyc0irq_ifw_1`,
    /// `lyc153int_m2irq/lyc153int_m2irq_ifw_1`).
    #[test]
    fn if_write_lands_one_dot_after_its_access() {
        let line1 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456;
        let fresh = |scx: u8, stat: u8, lyc: u8| {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line1 - 40, &mut fb, &mut []);
            write(emu, 0xFF43, scx);
            write(emu, 0xFF45, lyc);
            write(emu, 0xFF41, stat);
            emu
        };
        // The mode-0 interrupt's dot against the M-cycle grid, per SCX:
        // cleared exactly when it is one dot past the write's access.
        let mut seen = [false; 2];
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        // A fresh emulator's context at M-cycle boundary `at`.
        macro_rules! ctx_at {
            ($ctx:ident, $emu:expr, $at:expr) => {
                let emu = $emu;
                let Emulator { mem, hw } = &mut *emu;
                let mut $ctx =
                    Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
                while $ctx.clock() < $at {
                    $ctx.idle();
                }
            };
        }
        for scx in 0..8 {
            let b = {
                ctx_at!(ctx, fresh(scx, 0x08, 0xFF), line1 + 200);
                ctx.sys.iflags = 0;
                let mut b = ctx.clock();
                while ctx.sys.iflags & crate::ppu::INT_STAT == 0 {
                    b = ctx.clock();
                    ctx.idle();
                }
                b
            };
            // The interrupt's dot: in (b, b + 4].
            let e = {
                ctx_at!(ctx, fresh(scx, 0x08, 0xFF), b);
                ctx.sys.iflags = 0;
                let mut e = b + 1;
                ctx.run_ppu_to_call(e);
                while ctx.sys.iflags & crate::ppu::INT_STAT == 0 {
                    e += 1;
                    ctx.run_ppu_to_call(e);
                }
                e
            };
            // An IF write accessed at `b`.
            ctx_at!(ctx, fresh(scx, 0x08, 0xFF), b);
            crate::cpu::Bus::write(&mut ctx, 0xFF0F, 0);
            let cleared = ctx.sys.iflags & crate::ppu::INT_STAT == 0;
            assert_eq!(cleared, e == b + 1, "SCX {scx}: access {b}, interrupt {e}");
            seen[usize::from(cleared)] = true;
        }
        assert_eq!(seen, [true, true]);
        // (STAT, LYC, interrupt dot): line 153's LYC=0 at 12, line 0's
        // mode-2 pulse at 4; the IF write accessed a dot before it.
        let line0 = line1 + 153 * 456;
        for (stat, lyc, edge) in [(0x40, 0, line0 - 456 + 12), (0x20, 0xFF, line0 + 4)] {
            ctx_at!(ctx, fresh(0, stat, lyc), edge - 1);
            assert_eq!(ctx.clock(), edge - 1);
            ctx.sys.iflags = 0;
            crate::cpu::Bus::write(&mut ctx, 0xFF0F, 0);
            assert_ne!(ctx.sys.iflags & crate::ppu::INT_STAT, 0, "STAT {stat:02X}");
        }
    }

    /// Single speed, a STAT write whose phase 2 lands on line 0's mode-2
    /// pulse (access at t = 3) sees the mode-1 source still holding the
    /// line: `10 -> 50` with LYC = 0 requests nothing (CGB04C
    /// `miscmstatirq/lycstatwirq_trigger_ly00_10_50_1`); accessed at
    /// t = 7 (`_2`) it does. `10 -> 20` at t = 3: the pulse is past for
    /// the new mode-2 select (`m2enable/late_enable_m1disable_ly0_2`).
    #[test]
    fn stat_write_on_the_line_0_pulse_sees_mode_1() {
        let line0 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 154 * 456;
        for (value, t, irq) in [(0x50, 3, false), (0x50, 7, true), (0x20, 3, false)] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line0 - 200, &mut fb, &mut []);
            write(emu, 0xFF45, 0);
            write(emu, 0xFF41, 0x10);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            while ctx.clock() < line0 + t {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), line0 + t);
            ctx.sys.iflags = 0;
            crate::cpu::Bus::write(&mut ctx, 0xFF41, value);
            ctx.idle();
            let stat = ctx.sys.iflags & crate::ppu::INT_STAT != 0;
            assert_eq!(stat, irq, "{value:02X} at 0:{t}");
        }
    }

    /// A dispatch's low-byte push onto IF (SP = FF10) chooses the vector
    /// from IF as it was before that write (SameBoy `cycle_write_if`;
    /// gambatte cgb04c `irq_precedence/if_and_ie_0_vector_4`). A push
    /// elsewhere sees IF as it is after the write.
    #[test]
    fn dispatch_push_onto_if_chooses_from_the_old_if() {
        use crate::timer::INT_TIMER;
        for (addr, pending) in [(0xFF0F, INT_TIMER), (0xC000, 0)] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.sys.iflags = INT_TIMER;
            if addr != 0xFF0F {
                // The same IF change, by another access.
                ctx.write(0xFF0F, 0x80);
            }
            let got = crate::cpu::Bus::push_acknowledge(&mut ctx, addr, 0x80, 0x1F);
            assert_eq!(got, pending, "push to {addr:04X}");
            assert_eq!(ctx.sys.iflags, 0, "push to {addr:04X}");
        }
    }

    /// CPU accesses to VRAM in mode 3 and to OAM in modes 2-3 are blocked;
    /// `peek` and the OAM DMA's reads are not.
    #[test]
    fn ppu_blocks_cpu_vram_and_oam_access() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        emu.mem.vram[0] = 0x12;
        emu.mem.oam[0] = 0x34;
        // Line 5 of the first full frame, 120 dots in: mode 3.
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        let _ = emu.run_until(line5 + 120, &mut fb, &mut []);
        assert_eq!(emu.peek(0xFF41) & 3, 3);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0x8000), 0xFF);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE00), 0xFF);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFEA0), 0xFF);
        ctx.write_mapped(0x8000, 0x56);
        ctx.write_mapped(0xFE00, 0x78);
        assert_eq!((ctx.mem.vram[0], ctx.mem.oam[0]), (0x12, 0x34));
        // An OAM DMA from VRAM reads it anyway.
        assert_eq!(read::<false>(ctx.mem, ctx.sys, &ctx.hot, 0x8000), 0x12);
        drop(ctx);
        assert_eq!((emu.peek(0x8000), emu.peek(0xFE00)), (0x12, 0x34));
        // HBlank of the same line: free.
        let _ = emu.run_until(line5 + 300, &mut fb, &mut []);
        assert_eq!(emu.peek(0xFF41) & 3, 0);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0x8000), 0x12);
        ctx.write_mapped(0x8000, 0x56);
        ctx.write_mapped(0xFE00, 0x78);
        assert_eq!((ctx.mem.vram[0], ctx.mem.oam[0]), (0x56, 0x78));
    }

    /// The post-boot values Mooneye `misc/boot_hwio-C` and
    /// `misc/boot_div-cgbABCDE` check that depend on the mode or phase,
    /// and the CGB-mode DIV phase.
    #[test]
    fn post_boot_state_of_a_dmg_mode_cartridge() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        assert_eq!(emu.peek(0xFF00), 0xFF);
        assert_eq!((emu.peek(0xFF68), emu.peek(0xFF6A)), (0xC8, 0xD0));
        // DIV ticks to 27 at clock 136 (34 M-cycles).
        let t = Timer::post_boot(false);
        assert_eq!(
            (t.div(132), t.div(136), t.div(136 + 255)),
            (0x26, 0x27, 0x27)
        );
        assert_eq!(emu.peek(0xFF04), 0x26);
        // CGB mode leaves P1 alone.
        let emu = load(rom(0x8000, 0, 0, 0x80));
        assert_eq!(emu.peek(0xFF00), 0xCF);
        // A CGB-mode cartridge's boot is shorter: DIV ticks to 1F at clock
        // 96 (24 M-cycles; gambatte `div/start_inc_*_cgb04c`, ADR 0022).
        assert_eq!(emu.peek(0xFF04), 0x1E);
        assert_eq!(emu.hw.sys.timer.sc(0), Timer::post_boot(true).sc(0));
        let t = Timer::post_boot(true);
        assert_eq!((t.div(92), t.div(96), t.div(96 + 255)), (0x1E, 0x1F, 0x1F));
    }

    // --- APU framework (ADR 0021; M6 task 1(a)) ---

    fn run_audio(emu: &mut Emulator, clocks: u64, audio: &mut [StereoSample]) -> crate::RunResult {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        emu.run_cycles(clocks, &mut fb, audio)
    }

    /// The clock of the first DIV-APU edge after reset: counter bit 12
    /// falls when the counter reaches a multiple of 0x2000.
    fn first_div_apu_edge() -> u64 {
        let c0 = Timer::post_boot(true).sc(0);
        (((c0 >> 13) + 1) << 13) - c0
    }

    #[test]
    fn nr52_power_off_clears_the_registers_and_ignores_writes() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        assert_eq!(emu.peek(0xFF26), 0xF1, "post-boot");
        write(emu, 0xFF26, 0x8E); // only bit 7 is writable
        assert_eq!(emu.peek(0xFF26), 0xF1);
        write(emu, 0xFF30, 0x5A);
        write(emu, 0xFF26, 0x00);
        assert_eq!(emu.peek(0xFF26), 0x70);
        assert_eq!(
            emu.hw.sys.sched.deadline(EventKind::DivApu),
            NEVER,
            "no frame sequencer while off"
        );
        for reg in 0xFF10..=0xFF25u16 {
            let mask = SOUND_READ_MASK[usize::from(reg - 0xFF10)];
            assert_eq!(emu.peek(reg), mask, "{reg:#06x} cleared");
            write(emu, reg, 0xFF);
            assert_eq!(emu.peek(reg), mask, "{reg:#06x} ignores writes while off");
        }
        // Wave RAM is kept and stays writable.
        assert_eq!(emu.peek(0xFF30), 0x5A);
        write(emu, 0xFF31, 0xA5);
        assert_eq!(emu.peek(0xFF31), 0xA5);
        write(emu, 0xFF26, 0x80);
        assert_eq!(emu.peek(0xFF26), 0xF0);
        assert_eq!(emu.peek(0xFF12), 0x00, "still cleared after power on");
        write(emu, 0xFF12, 0xF3);
        assert_eq!(emu.peek(0xFF12), 0xF3);
    }

    #[test]
    fn div_apu_steps_on_each_falling_edge_of_counter_bit_12() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), first);
        let step_at = |emu: &mut Emulator, t: u64| {
            let now = emu.clock();
            let _ = run_audio(emu, t - now, &mut []);
            emu.mem.apu.fs.step
        };
        assert_eq!(step_at(emu, first - 8), 0);
        assert_eq!(step_at(emu, first + 4), 1);
        assert_eq!(step_at(emu, first + 8192 - 8), 1);
        assert_eq!(step_at(emu, first + 8192 + 4), 2);
        assert_eq!(
            emu.hw.sys.sched.deadline(EventKind::DivApu),
            first + 2 * 8192
        );
    }

    /// The sweep step comes `SWEEP_LAG` (4) clocks after the edge into
    /// step 3: an access at the edge's clock still sees CH1 on (gambatte
    /// cgb04c `ch1_init_reset_sweep_counter_timing_nr52_3`), the `DivApu`
    /// event runs the step 4 clocks on and its overflow check 4 more on
    /// (shift 1), then waits for the next edge.
    #[test]
    fn the_sweep_steps_4_clocks_after_the_edge_into_step_3() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        let e3 = first + 2 * 8192;
        let _ = run_audio(emu, first + 8192 + 0x100, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 2);
        // Period 1, shift 1, f = 1280: the first step stores 1920 and its
        // overflow check (2880) turns CH1 off.
        for (reg, v) in [
            (0xFF12, 0xF0),
            (0xFF10, 0x11),
            (0xFF13, 0x00),
            (0xFF14, 0x85),
        ] {
            write(emu, reg, v);
        }
        assert_eq!(emu.peek(0xFF26) & 1, 1);
        let now = emu.clock();
        let _ = run_audio(emu, e3 - now, &mut []);
        assert_eq!(emu.clock(), e3);
        assert_eq!(emu.mem.apu.fs.step, 3);
        assert_eq!(emu.peek(0xFF26) & 1, 1, "on at the edge");
        assert_eq!(emu.mem.apu.sq[0].freq, 1280);
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), e3 + 4);
        let _ = run_audio(emu, 4, &mut []);
        assert_eq!(emu.mem.apu.sq[0].freq, 1920);
        // The overflow check is 1 MHz tick (shift 1) after the step.
        assert_eq!(emu.peek(0xFF26) & 1, 1, "on until the check");
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), e3 + 8);
        let _ = run_audio(emu, 4, &mut []);
        assert_eq!(emu.peek(0xFF26) & 1, 0, "off 8 clocks on");
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), e3 + 8192);
    }

    #[test]
    fn a_div_write_with_bit_12_set_steps_the_frame_sequencer_early() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        let _ = run_audio(emu, first + 0x800, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 1);
        // Bit 12 is 0: no edge; the next one is 0x2000 after the reset.
        write(emu, 0xFF04, 0);
        assert_eq!(emu.mem.apu.fs.step, 1);
        let reset = emu.clock();
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), reset + 0x2000);
        // 0x1100 later bit 12 is 1: the reset is its falling edge.
        let _ = run_audio(emu, 0x1100, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 1);
        write(emu, 0xFF04, 0);
        assert_eq!(emu.mem.apu.fs.step, 2);
        let reset = emu.clock();
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), reset + 0x2000);
        let _ = run_audio(emu, 0x2000 + 4, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 3);
    }

    /// In double speed the edge is counter bit 13's fall, and each switch
    /// to double speed with the APU on toggles a 1 M-cycle (2 clock) delay:
    /// the 1st and 3rd switches delay the frame sequencer, the 2nd doesn't,
    /// single speed never does, and NR52 power on/off clears it (AGE
    /// `spsw-ch2-lc-delay`: `TEST_DS`, `TEST_DS_NS_DS`, `TEST_DS_OFF_ON`).
    #[test]
    fn div_apu_follows_counter_bit_13_after_a_switch_to_double_speed() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // The deadline less the bare edge of the current DIV-APU bit.
        let late = |ctx: &Ctx| {
            let t = &ctx.sys.timer;
            let k = t.div_apu_bit() + 1;
            let edge = t.clock_at(((t.sc(ctx.clock()) >> k) + 1) << k);
            ctx.sys.sched.deadline(EventKind::DivApu) - edge
        };
        let mut lates = [0; 6];
        for l in &mut lates {
            assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
            *l = late(&ctx);
        }
        assert_eq!(ctx.sys.timer.div_apu_bit(), 12);
        assert_eq!(lates, [2, 0, 0, 0, 2, 0], "DS, NS, DS, NS, DS, NS");
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        assert_eq!(late(&ctx), 0, "4th switch to double speed");
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        assert_eq!(late(&ctx), 2, "5th switch to double speed");
        let now = ctx.clock();
        ctx.write_nr52(0x00, now);
        ctx.write_nr52(0x80, now);
        assert_eq!(late(&ctx), 0, "power cycle in double speed");
    }

    #[test]
    fn power_on_with_bit_12_set_skips_the_first_edge() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        write(emu, 0xFF26, 0x00);
        let _ = run_audio(emu, first + 0x1100, &mut []);
        write(emu, 0xFF26, 0x80);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, 2));
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), first + 0x2000);
        let _ = run_audio(emu, 0x1000, &mut []);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, 1));
        let _ = run_audio(emu, 0x2000, &mut []);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, 0));
        let _ = run_audio(emu, 0x2000, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 2);
    }

    /// The power-on skip samples the counter at the end of the NR52 write's
    /// M-cycle: a write in the M-cycle at whose end bit 12 rises (counter
    /// 0x2FFC-0x3000) skips; one M-cycle earlier doesn't. Gambatte
    /// `ch2_late_reset_nr52_1*` / `_2*` (CGB) differ by exactly that.
    #[test]
    fn power_on_in_the_m_cycle_where_bit_12_rises_skips() {
        for (sc, skip) in [(0x2FF8, 0), (0x2FFC, 2)] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            write(emu, 0xFF26, 0x00);
            let _ = run_audio(emu, sc - Timer::post_boot(true).sc(0), &mut []);
            assert_eq!(emu.hw.sys.timer.sc(emu.clock()), sc);
            write(emu, 0xFF26, 0x80);
            assert_eq!(emu.mem.apu.fs.skip, skip, "{sc:#x}");
        }
    }

    /// An edge at the end of the NR52 power-on write's M-cycle (counter
    /// 0x3FFC -> 0x4000, bit 12 or 13 falling) happens while the APU is
    /// still off: no skip, and the first step comes one DIV-APU period
    /// later, not at once. Normal and double speed.
    #[test]
    fn power_on_in_the_m_cycle_where_the_div_apu_bit_falls_waits_a_period() {
        for double in [false, true] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            if double {
                let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
                let Emulator { mem, hw } = &mut *emu;
                let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
                assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
            }
            write(emu, 0xFF26, 0x00);
            let k = emu.hw.sys.timer.div_apu_bit() + 1;
            let period = 1u64 << k;
            let sc0 = emu.hw.sys.timer.sc(emu.clock());
            let sc = (((sc0 >> k) + 2) << k) - 4;
            let _ = run_audio(emu, emu.hw.sys.timer.clock_at(sc) - emu.clock(), &mut []);
            assert_eq!(emu.hw.sys.timer.sc(emu.clock()), sc, "double {double}");
            write(emu, 0xFF26, 0x80);
            assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (0, 0));
            let first = emu.hw.sys.timer.clock_at(sc + 4 + period);
            assert_eq!(
                emu.hw.sys.sched.deadline(EventKind::DivApu),
                first,
                "double {double}"
            );
            let _ = run_audio(emu, first - 32 - emu.clock(), &mut []);
            assert!(emu.clock() < first);
            assert_eq!(emu.mem.apu.fs.step, 0, "double {double}");
            let _ = run_audio(emu, 64, &mut []);
            assert_eq!(emu.mem.apu.fs.step, 1, "double {double}");
        }
    }

    #[test]
    fn one_sample_per_128_clocks_across_run_calls() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut audio = vec![StereoSample { left: 1, right: 1 }; 1000];
        let mut total = 0;
        for clocks in [100, 200, 28, 4, 1000, 40_000, 12] {
            let start = emu.clock();
            let r = run_audio(emu, clocks, &mut audio);
            let end = emu.clock();
            assert_eq!(
                r.audio_samples as u64,
                end / 128 - start / 128,
                "{start}..{end}"
            );
            // Post-boot only CH1 is on: DAC on (NR12 = F3) at volume 0,
            // panned to both sides (NR51 = F3), NR50 = 77: a constant
            // +15 x 8 x 64.
            let dc = StereoSample {
                left: 7680,
                right: 7680,
            };
            assert!(audio[..r.audio_samples].iter().all(|s| *s == dc));
            assert_eq!(emu.mem.apu.out.synced, end, "synced at the end of the call");
            total += r.audio_samples as u64;
        }
        assert_eq!(total, emu.clock() / 128);
        assert_eq!(crate::AUDIO_SAMPLE_RATE, 32_768);
        assert_eq!(crate::CLOCK_HZ / crate::AUDIO_SAMPLE_CLOCKS, 32_768);
    }

    #[test]
    fn a_full_audio_buffer_ends_the_call() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut audio = vec![StereoSample::default(); 10];
        let r = run_audio(emu, u64::from(FRAME_CLOCKS), &mut audio);
        assert!(r.events.contains(Events::AUDIO_BUFFER_FULL));
        assert_eq!(r.audio_samples, 10);
        assert!((1280..1280 + 128).contains(&r.clocks), "{}", r.clocks);
        // The next call continues the window grid.
        let mut audio = vec![StereoSample::default(); 100];
        let r = run_audio(emu, 1000, &mut audio);
        assert!(!r.events.contains(Events::AUDIO_BUFFER_FULL));
        assert_eq!(r.audio_samples as u64, emu.clock() / 128 - 10);
    }

    #[test]
    fn an_empty_audio_buffer_only_advances_and_restarts_the_grid() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let r = run_audio(emu, 1000, &mut []);
        assert_eq!(r.audio_samples, 0);
        assert!(!r.events.contains(Events::AUDIO_BUFFER_FULL));
        assert!(!emu.mem.apu.out.live);
        assert_eq!(emu.mem.apu.out.synced, emu.clock());
        // 100 clocks into a window, then an empty-buffer stretch: the next
        // call with room restarts the grid at its start.
        let mut audio = vec![StereoSample::default(); 10];
        assert_eq!(run_audio(emu, 100, &mut audio).audio_samples, 0);
        let _ = run_audio(emu, 400, &mut []);
        assert_eq!(run_audio(emu, 124, &mut audio).audio_samples, 0);
        assert_eq!(run_audio(emu, 4, &mut audio).audio_samples, 1);
    }

    // --- CH2 (ADR 0021 section 5; M6 task 1(b)) ---

    #[test]
    fn ch2_length_expiry_clears_nr52_bit_1() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF17, 0xF0); // DAC on
        write(emu, 0xFF16, 0x3E); // length 2
        write(emu, 0xFF19, 0xC0); // trigger, length on (fs 0: no extra clock)
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x02);
        // The first DIV-APU edge is fs step 1: one length clock.
        let _ = run_audio(emu, first_div_apu_edge() + 100, &mut []);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x02, "2 -> 1");
        // Step 3 is the next length clock.
        let _ = run_audio(emu, 2 * 8192, &mut []);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x00, "1 -> 0: off");
    }

    #[test]
    fn ch2_envelope_steps_at_fs_step_0() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF17, 0xF1); // volume 15, down, period 1
        write(emu, 0xFF19, 0x80);
        // Steps 1-7 don't clock the envelope; step 0 (the 8th) does.
        let _ = run_audio(emu, first_div_apu_edge() + 6 * 8192 + 100, &mut []);
        assert_eq!(emu.mem.apu.sq[1].env.volume, 15);
        let _ = run_audio(emu, 8192, &mut []);
        assert_eq!(emu.mem.apu.sq[1].env.volume, 14);
    }

    #[test]
    fn ch2_dac_off_clears_nr52_bit_1() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF17, 0xF0);
        write(emu, 0xFF19, 0x80);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x02);
        write(emu, 0xFF17, 0x07);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x00);
    }

    #[test]
    fn pcm12_high_nibble_is_ch2s_digital_output() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF16, 0x40); // duty 25%: positions 0 and 7 high
        write(emu, 0xFF17, 0xF0); // volume 15
        write(emu, 0xFF18, 0x00);
        write(emu, 0xFF19, 0x84); // f = 0x400: 4096 clocks per step
        let _ = run_audio(emu, 2000, &mut []);
        assert_eq!(emu.peek(0xFF76), 0x00, "position 0, before the first step");
        let _ = run_audio(emu, 4106 + 6 * 4096, &mut []);
        assert_eq!(emu.peek(0xFF76), 0xF0, "position 7");
        // A pure read: the same at the same clock.
        assert_eq!(emu.peek(0xFF76), 0xF0);
        let _ = run_audio(emu, 4096, &mut []);
        assert_eq!(emu.peek(0xFF76), 0xF0, "position 0, started");
        let _ = run_audio(emu, 4096, &mut []);
        assert_eq!(emu.peek(0xFF76), 0x00, "position 1");
    }

    /// CH2 at volume 15, duty 12.5%, frequency `f`, on both sides at
    /// volume 8, after `warm` windows; then `n` samples.
    fn ch2_tone(f: u16, warm: usize, n: usize) -> std::vec::Vec<StereoSample> {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF24, 0x77);
        write(emu, 0xFF25, 0x22);
        write(emu, 0xFF16, 0x00);
        write(emu, 0xFF17, 0xF0);
        write(emu, 0xFF18, f as u8);
        write(emu, 0xFF19, 0x80 | (f >> 8) as u8);
        let mut audio = vec![StereoSample::default(); warm + n];
        let r = run_audio(emu, 128 * (warm + n) as u64, &mut audio);
        assert_eq!(r.audio_samples, warm + n);
        audio.split_off(warm)
    }

    #[test]
    fn a_known_ch2_tone_averages_through_the_mixer() {
        // One duty high (digital 15 -> analog -15) and seven low (+15) per
        // period: average 11.25, times volume 8, times 64 = 5760.
        let want = StereoSample {
            left: 5760,
            right: 5760,
        };
        // f = 2044: one duty period per 128-clock window, every window.
        assert!(ch2_tone(2044, 2, 20).iter().all(|&s| s == want));
        // f = 2047: four whole periods per window (the closed-form path).
        assert!(ch2_tone(2047, 2, 20).iter().all(|&s| s == want));
        // f = 2016: one step per window; eight windows are one period.
        let tone = ch2_tone(2016, 10, 32);
        assert!(tone.iter().any(|&s| s != want));
        for period in tone.chunks(8) {
            let sum: i32 = period.iter().map(|s| i32::from(s.left)).sum();
            assert_eq!(sum, 8 * 5760);
        }
    }

    /// What a [`split_run`] ends with.
    #[derive(Debug, PartialEq, Eq)]
    struct SplitEnd {
        samples: std::vec::Vec<StereoSample>,
        clock: u64,
        regs: crate::Registers,
        out: crate::apu::Output,
        vram_sum: u64,
    }

    /// Runs `program` (at 0100, CGB mode, CH2 sounding on both sides) to
    /// clock `end`, in run calls of at most `chunk` clocks with `k`-slot
    /// audio buffers. Also returns whether a call ended inside a VRAM DMA
    /// stall.
    fn split_run(program: &[u8], end: u64, chunk: u64, k: usize) -> (SplitEnd, bool) {
        let rom = rom(0x8000, 0, 0, 0x80);
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        let emu = load(rom);
        for (reg, v) in [
            (0xFF24, 0x77),
            (0xFF25, 0x22),
            (0xFF16, 0x80), // duty 50%
            (0xFF17, 0xF0),
            (0xFF18, 0x00),
            (0xFF19, 0x87),
        ] {
            write(emu, reg, v);
        }
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let mut buf = vec![StereoSample::default(); k];
        let mut samples = std::vec::Vec::new();
        let mut mid_stall = false;
        while emu.clock() < end {
            let t = emu.clock().saturating_add(chunk).min(end);
            let r = emu.run_until(t, &mut fb, &mut buf);
            samples.extend_from_slice(&buf[..r.audio_samples]);
            mid_stall |= emu.hw.sys.vdma.stall != 0;
        }
        let vram_sum = emu.mem.vram.iter().enumerate();
        let vram_sum = vram_sum.map(|(i, &b)| (i as u64 + 1) * u64::from(b)).sum();
        let end = SplitEnd {
            samples,
            clock: emu.clock(),
            regs: emu.registers(),
            out: emu.mem.apu.out,
            vram_sum,
        };
        (end, mid_stall)
    }

    /// A loop of 128-block GDMAs (each a 4,100-clock CPU stall) that
    /// changes the CH2 frequency between them.
    const GDMA_LOOP: [u8; 0x18] = [
        0x3E, 0xC0, 0xE0, 0x51, // LD A,C0; LDH (51),A
        0xAF, 0xE0, 0x52, // XOR A; LDH (52),A
        0x3E, 0x80, 0xE0, 0x53, // LD A,80; LDH (53),A
        0xAF, 0xE0, 0x54, // XOR A; LDH (54),A
        0x3E, 0x7F, 0xE0, 0x55, // LD A,7F; LDH (55),A: GDMA, 128 blocks
        0x04, 0x78, 0xE0, 0x18, // INC B; LD A,B; LDH (18),A
        0x18, 0xE8, // JR 0100
    ];

    #[test]
    fn a_gdma_stall_across_the_buffer_deadline_drops_no_sample() {
        let end = 3 * u64::from(FRAME_CLOCKS) + 77;
        let (want, _) = split_run(&GDMA_LOOP, end, u64::MAX, 10_000);
        assert!(want.samples.len() as u64 >= end / 128 - 1);
        for k in [1, 7, 33, 549] {
            let (got, mid_stall) = split_run(&GDMA_LOOP, end, u64::MAX, k);
            assert_eq!(got, want, "k = {k}");
            assert!(mid_stall || k == 549, "k = {k}: no FULL inside a stall");
        }
        // Targets inside stalls too.
        let (got, mid_stall) = split_run(&GDMA_LOOP, end, 1_001, 7);
        assert_eq!(got, want);
        assert!(mid_stall);
    }

    #[test]
    fn samples_and_state_are_independent_of_call_and_buffer_splits() {
        // INC B; LD A,B; LDH (18),A; LD C,20; DEC C; JR NZ,-3; JR 0100.
        let program = [
            0x04, 0x78, 0xE0, 0x18, 0x0E, 0x20, 0x0D, 0x20, 0xFD, 0x18, 0xF5,
        ];
        let end = 2 * u64::from(FRAME_CLOCKS) + 333;
        let (want, _) = split_run(&program, end, u64::MAX, 10_000);
        for k in [1, 7, 33, 549] {
            for chunk in [u64::MAX, 997, 128 * 549] {
                let (got, _) = split_run(&program, end, chunk, k);
                assert_eq!(got, want, "k = {k}, chunk = {chunk}");
            }
        }
        // A run of n clocks never completes more than n / 128 + 2.
        let (got, _) = split_run(&program, end, 255, 4);
        assert_eq!(got, want, "4 slots suffice for 255-clock calls");
    }

    #[test]
    fn sound_registers_read_their_unused_bits_as_1() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        for (reg, ones) in [
            (0xFF10, 0x80),
            (0xFF15, 0xFF),
            (0xFF1C, 0x9F),
            (0xFF26, 0x70),
        ] {
            write(emu, reg, 0x00);
            assert_eq!(emu.peek(reg), ones, "{reg:#06x}");
        }
        write(emu, 0xFF30, 0x00);
        assert_eq!(emu.peek(0xFF30), 0x00);
        assert_eq!((emu.peek(0xFF76), emu.peek(0xFF77)), (0x00, 0x00));
    }

    #[test]
    fn unusable_area_is_cgb_c_ram_without_address_bits_3_and_4() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFEA0, 0x11);
        write(emu, 0xFEC5, 0x22);
        write(emu, 0xFEE7, 0x33);
        for (addr, v) in [
            (0xFEA8, 0x11),
            (0xFEB0, 0x11),
            (0xFEB8, 0x11),
            (0xFEDD, 0x22),
            (0xFEFF, 0x33),
            (0xFEA1, 0x00),
        ] {
            assert_eq!(emu.peek(addr), v, "{addr:#06x}");
        }
    }

    #[test]
    fn joypad_reads_the_selected_group() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        emu.set_buttons(Buttons::A | Buttons::DOWN);
        assert_eq!(emu.peek(0xFF00), 0xC0 | 0x06); // both groups selected
        write(emu, 0xFF00, 0x10); // buttons
        assert_eq!(emu.peek(0xFF00), 0xD0 | 0x0E);
        write(emu, 0xFF00, 0x20); // d-pad
        assert_eq!(emu.peek(0xFF00), 0xE0 | 0x07);
        write(emu, 0xFF00, 0x30); // neither
        assert_eq!(emu.peek(0xFF00), 0xFF);
    }

    #[test]
    fn mbc1_rom_banking() {
        // 1 MiB: 64 banks, each starting with its own number.
        let r = rom(0x10_0000, 0x01, 0, 0);
        for bank in 1..64 {
            r[bank * 0x4000] = bank as u8;
        }
        r[0x104] = 1; // not a multicart: no logo copy in bank 0x10
        let emu = load(r);
        assert_eq!(emu.peek(0x4000), 1);
        write(emu, 0x2000, 0x00); // 0 selects 1
        assert_eq!(emu.peek(0x4000), 1);
        write(emu, 0x2000, 0x05);
        assert_eq!(emu.peek(0x4000), 5);
        write(emu, 0x2000, 0xE7); // 5 bits: 0x07
        assert_eq!(emu.peek(0x4000), 7);
        write(emu, 0x4000, 0x01); // BANK2 = 1: bank 0x27
        assert_eq!(emu.peek(0x4000), 0x27);
        assert_eq!(emu.peek(0x0000), 0x00); // mode 0: bank 0 at 0000
        write(emu, 0x6000, 0x01); // mode 1: bank 0x20 at 0000
        assert_eq!(emu.peek(0x0000), 0x20);
        // 0x20 selects 0x21 only through the 5-bit zero check: 0x20 itself
        // can't be mapped at 4000.
        write(emu, 0x2000, 0x00);
        assert_eq!(emu.peek(0x4000), 0x21);
    }

    #[test]
    fn mbc1_bank_numbers_wrap_to_the_rom_size() {
        // 256 KiB: 16 banks; bank 0x15 mirrors bank 5.
        let r = rom(0x4_0000, 0x01, 0, 0);
        for bank in 1..16 {
            r[bank * 0x4000] = bank as u8;
        }
        let emu = load(r);
        write(emu, 0x2000, 0x15);
        assert_eq!(emu.peek(0x4000), 5);
        write(emu, 0x4000, 0x03);
        assert_eq!(emu.peek(0x4000), 5);
    }

    #[test]
    fn mbc1_ram_enable_and_banks() {
        let emu = load(rom(0x8000, 0x03, 0x03, 0)); // 32 KiB RAM
        write(emu, 0xA000, 0x55);
        assert_eq!(emu.peek(0xA000), 0xFF, "disabled");
        write(emu, 0x0000, 0x0A);
        write(emu, 0xA000, 0x55);
        assert_eq!(emu.peek(0xA000), 0x55);
        write(emu, 0x4000, 0x02); // BANK2 = 2, but mode 0: still bank 0
        assert_eq!(emu.peek(0xA000), 0x55);
        write(emu, 0x6000, 0x01); // mode 1: RAM bank 2
        assert_eq!(emu.peek(0xA000), 0x00);
        write(emu, 0xA001, 0x66);
        assert_eq!(emu.cart_ram()[2 * 0x2000 + 1], 0x66);
        write(emu, 0x0000, 0x00);
        assert_eq!(emu.peek(0xA001), 0xFF);
    }

    #[test]
    fn io_registers_after_boot() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        for (reg, v) in [
            (0xFF00, 0xCF),
            (0xFF07, 0xF8),
            (0xFF0F, 0xE1),
            (0xFF26, 0xF1),
            (0xFF40, 0x91),
            (0xFF47, 0xFC),
            (0xFF4D, 0x7E),
            (0xFF4F, 0xFE),
            (0xFF70, 0xF8),
            (0xFFFF, 0x00),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
    }

    #[test]
    fn joypad_interrupt_on_a_line_going_low() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let joyp_if = |emu: &mut Emulator| {
            let v = emu.peek(0xFF0F) & 0x10 != 0;
            write(emu, 0xFF0F, 0);
            v
        };
        write(emu, 0xFF0F, 0);
        write(emu, 0xFF00, 0x10); // action buttons selected
        emu.set_buttons(Buttons::A);
        assert!(joyp_if(emu), "A pressed");
        emu.set_buttons(Buttons::A | Buttons::B);
        assert!(joyp_if(emu), "B pressed while A held");
        emu.set_buttons(Buttons::A);
        assert!(!joyp_if(emu), "release: line goes high");
        emu.set_buttons(Buttons::A | Buttons::UP);
        assert!(!joyp_if(emu), "d-pad not selected");
        // Selecting the d-pad exposes the held Up: P12 goes low.
        write(emu, 0xFF00, 0x20);
        assert!(joyp_if(emu), "select exposes Up");
        // Deselecting raises the lines; selecting both groups lowers P10
        // (A) and P12 (Up) again.
        write(emu, 0xFF00, 0x30);
        assert!(!joyp_if(emu));
        write(emu, 0xFF00, 0x00);
        assert!(joyp_if(emu));
        // Same lines low as before: no edge.
        write(emu, 0xFF00, 0x00);
        assert!(!joyp_if(emu));
        // Pressing Right while A already holds P10 low: no edge.
        emu.set_buttons(Buttons::A | Buttons::UP | Buttons::RIGHT);
        assert!(!joyp_if(emu));
    }

    #[test]
    fn serial_control_after_boot() {
        // Pan Docs: Power Up Sequence (CGB 7F); Mooneye boot_hwio-C (DMG
        // compatibility mode on CGB: 7E).
        assert_eq!(load(rom(0x8000, 0, 0, 0x80)).peek(0xFF02), 0x7F);
        assert_eq!(load(rom(0x8000, 0, 0, 0x00)).peek(0xFF02), 0x7E);
    }

    #[test]
    fn undocumented_cgb_registers() {
        // CGB mode (SameBoy Core/memory.c read_high_memory).
        let emu = load(rom(0x8000, 0, 0, 0x80));
        for (reg, v) in [
            (0xFF6C, 0xFE),
            (0xFF72, 0),
            (0xFF73, 0),
            (0xFF74, 0),
            (0xFF75, 0x8F),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x} after boot");
        }
        for reg in [0xFF6C, 0xFF72, 0xFF73, 0xFF74, 0xFF75] {
            write(emu, reg, 0xFF);
        }
        for (reg, v) in [
            (0xFF6C, 0xFF),
            (0xFF72, 0xFF),
            (0xFF73, 0xFF),
            (0xFF74, 0xFF),
            (0xFF75, 0xFF),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
        for reg in [0xFF6C, 0xFF72, 0xFF73, 0xFF74, 0xFF75] {
            write(emu, reg, 0x00);
        }
        for (reg, v) in [
            (0xFF6C, 0xFE),
            (0xFF72, 0),
            (0xFF73, 0),
            (0xFF74, 0),
            (0xFF75, 0x8F),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
        write(emu, 0xFF72, 0x5A);
        write(emu, 0xFF73, 0xA5);
        write(emu, 0xFF75, 0x50);
        assert_eq!(
            (emu.peek(0xFF72), emu.peek(0xFF73), emu.peek(0xFF75)),
            (0x5A, 0xA5, 0xDF)
        );

        // DMG compatibility mode: the values Mooneye boot_hwio-C expects;
        // OPRI and FF74 are locked, FF72/FF73/FF75 still work.
        let emu = load(rom(0x8000, 0, 0, 0x00));
        for (reg, v) in [
            (0xFF6C, 0xFF),
            (0xFF72, 0),
            (0xFF73, 0),
            (0xFF74, 0xFF),
            (0xFF75, 0x8F),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x} after boot");
        }
        write(emu, 0xFF6C, 0x00);
        write(emu, 0xFF72, 0x12);
        write(emu, 0xFF74, 0x34);
        write(emu, 0xFF75, 0x70);
        for (reg, v) in [
            (0xFF6C, 0xFF),
            (0xFF72, 0x12),
            (0xFF74, 0xFF),
            (0xFF75, 0xFF),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
    }
}

#[cfg(test)]
mod dma_tests {
    //! OAM DMA timing, blocking and CGB C conflicts (`docs/reference/oam-dma.md`).
    extern crate std;
    use super::*;
    use crate::{Config, Frame};
    use std::boxed::Box;
    use std::vec;

    /// A CGB-mode emulator with the LCD off (no PPU deadline), OAM filled
    /// with 55, C100+i = C200+i = i ^ A5 and ROM 4000+i = i ^ 3C.
    fn setup() -> &'static mut Emulator<'static> {
        setup_with(&[])
    }

    /// [`setup`] with `program` at 0100 (the entry point).
    fn setup_with(program: &[u8]) -> &'static mut Emulator<'static> {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x143] = 0x80;
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        for i in 0..0xA0 {
            rom[0x4000 + i] = i as u8 ^ 0x3C;
        }
        let emu = crate::tests::fresh();
        emu.load(rom, &mut [], Config::default()).unwrap();
        emu.mem.oam.fill(0x55);
        for i in 0..0x200 {
            emu.mem.wram[0x100 + i] = i as u8 ^ 0xA5;
        }
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write_mapped(0xFF40, 0x11);
        drop(ctx);
        emu
    }

    /// Runs `f` with a run-call context on `emu`.
    fn with_ctx(emu: &mut Emulator, f: impl FnOnce(&mut Ctx)) {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        f(&mut ctx);
    }

    /// Idles until the next access is at `t0 + n` M-cycles.
    fn idle_to(ctx: &mut Ctx, t0: u64, n: u64) {
        while ctx.clock() < t0 + 4 * n {
            ctx.idle();
        }
        assert_eq!(ctx.clock(), t0 + 4 * n);
    }

    #[test]
    fn start_delay_blocking_and_end() {
        let emu = setup();
        let mut t0 = 0;
        with_ctx(emu, |ctx| {
            t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1); // M-cycle 0
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), t0 + 162 * 4);
            // M-cycle 1, the start delay: OAM is readable and untouched.
            assert_eq!(ctx.read(0xFE00), 0x55);
            // M-cycle 2: byte 0 transferred, OAM blocked; HRAM is not.
            assert_eq!(ctx.read(0xFE00), 0xFF);
            assert_eq!(ctx.read(0xFEA5), 0xFF);
            ctx.write(0xFF80, 0x12);
            assert_eq!(ctx.read(0xFF80), 0x12);
            // OAM writes are dropped.
            ctx.write(0xFE30, 0x99);
            ctx.write(0xFEA0, 0x99);
            idle_to(ctx, t0, 10);
        });
        // Between run calls OAM is synced to the clock: bytes 0-8 are in
        // (byte 8 counts from the access at M-cycle 10), the rest are not.
        // `peek` sees OAM as it is, without FF or conflicts.
        assert_eq!(emu.peek(0xFE00), 0xA5);
        assert_eq!(emu.peek(0xFE08), 8 ^ 0xA5);
        assert_eq!(emu.peek(0xFE09), 0x55);
        assert_eq!(emu.peek(0xC000), emu.mem.wram[0]);
        assert_eq!(emu.peek(0xC105), 5 ^ 0xA5);
        with_ctx(emu, |ctx| {
            idle_to(ctx, t0, 161);
            // The last transfer M-cycle: still blocked.
            assert_eq!(ctx.read(0xFE9F), 0xFF);
            // M-cycle 162: done.
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.read(0xFE9F), 0x9F ^ 0xA5);
            assert_eq!(ctx.read(0xFEA0), 0);
            ctx.write(0xFE30, 0x99);
            assert_eq!(ctx.read(0xFE30), 0x99);
        });
        for (i, &b) in emu.mem.oam.iter().enumerate() {
            if i != 0x30 {
                assert_eq!(b, i as u8 ^ 0xA5, "{i:#x}");
            }
        }
    }

    #[test]
    fn restart_keeps_oam_blocked_through_the_new_delay() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 20);
            ctx.write(0xFF46, 0xC2); // M-cycle 20
            // The new start delay: the old transfer still blocks OAM.
            assert_eq!(ctx.read(0xFE00), 0xFF);
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), t0 + (20 + 162) * 4);
            // The new transfer from C200.
            idle_to(ctx, t0, 20 + 162);
            assert_eq!(ctx.read(0xFE00), 0xA5);
            assert_eq!(ctx.mem.oam[0x9F], 0x9F ^ 0xA5);
        });
    }

    /// The M-cycle after a restarting FF46 write: the old transfer moves
    /// its next byte from the new source, so a CPU access on the new
    /// source's bus conflicts with that byte (gambatte cgb04c
    /// `oamdma_src8000_srcchange0000_busyinc`).
    #[test]
    fn restart_moves_the_old_byte_from_the_new_source() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 20);
            ctx.write(0xFF46, 0x40); // M-cycle 20: restart from ROM 4000
            // M-cycle 21: the old transfer's byte 19, from 4013.
            assert_eq!(ctx.read(0x0123), 19 ^ 0x3C);
            // M-cycle 22: the new transfer's byte 0.
            assert_eq!(ctx.read(0x0123), 0x3C);
            // The new transfer copies all 160 bytes from 4000 as usual.
            idle_to(ctx, t0, 190);
            assert_eq!(ctx.hot.dma, 0);
            for (i, &b) in ctx.mem.oam.iter().enumerate() {
                assert_eq!(b, i as u8 ^ 0x3C, "{i:#x}");
            }
            // Restarted in its own start delay: byte 0 in the next M-cycle.
            idle_to(ctx, t0, 200);
            ctx.write(0xFF46, 0xC1); // M-cycle 200
            ctx.write(0xFF46, 0x40); // 201
            assert_eq!(ctx.read(0x0123), 0x3C); // 202: byte 0 from 4000
            assert_eq!(ctx.read(0x0123), 0x3C); // 203: the new byte 0
        });
        // A fresh start conflicts with nothing in its start delay.
        let emu = setup();
        with_ctx(emu, |ctx| {
            ctx.write(0xFF46, 0x40);
            assert_eq!(ctx.read(0x0123), 0);
            assert_eq!(ctx.read(0x0123), 0x3C);
        });
    }

    #[test]
    fn restart_in_the_last_mcycle_is_a_fresh_start() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            ctx.write(0xFF46, 0xC1); // the old transfer's last M-cycle
            assert_eq!(ctx.read(0xFE10), 0x10 ^ 0xA5);
            assert_eq!(ctx.read(0xFE10), 0xFF);
        });
    }

    /// OAM DMA steps inside a VRAM DMA write the VRAM DMA's byte of that
    /// moment to OAM at its source address & FF, not their own (gambatte
    /// cgb04c `dma/hdma_transition_oamdma_1`).
    #[test]
    fn oam_dma_steps_during_a_vram_dma_take_its_bytes() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1); // bytes i ^ A5
            for (reg, v) in [(0xFF51, 0x40), (0xFF52, 0), (0xFF53, 0), (0xFF54, 0)] {
                ctx.write(reg, v);
            }
            ctx.write(0xFF55, 0x00); // a 1-block GDMA from ROM 4000
            idle_to(ctx, t0, 10);
            // At M-cycle 10 (bytes 0-8 due): the block's 8 M-cycles hold
            // the steps of bytes 9-16, which take block bytes 1, 3, .. 15.
            ctx.vram_dma(true);
            idle_to(ctx, t0, 170);
        });
        for (p, &b) in emu.mem.oam.iter().enumerate() {
            let want = match p {
                1..=15 if p % 2 == 1 => p as u8 ^ 0x3C,
                10 | 12 | 14 | 16 => 0x55,
                _ => p as u8 ^ 0xA5,
            };
            assert_eq!(b, want, "{p:#x}");
        }
    }

    #[test]
    fn wram_source_conflicts() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            // The start delay: no conflict.
            assert_eq!(ctx.read(0xC000), ctx.mem.wram[0]);
            idle_to(ctx, t0, 7);
            // WRAM reads see the DMA's byte (M-cycle 7: byte 5, C105); ROM
            // and VRAM are free.
            assert_eq!(ctx.read(0xD000), 5 ^ 0xA5);
            assert_eq!(ctx.read(0x4001), 1 ^ 0x3C);
            // The address the DMA reads next is not a conflict.
            assert_eq!(ctx.read(0xC108), 8 ^ 0xA5); // M-cycle 9: byte 7
            // WRAM writes are dropped.
            let before = ctx.mem.wram[0x10];
            ctx.write(0xC010, 0x77);
            assert_eq!(ctx.mem.wram[0x10], before);
            // A VRAM write is free.
            ctx.write(0x8000, 0x66);
            assert_eq!(ctx.mem.vram[0], 0x66);
        });
    }

    #[test]
    fn rom_source_conflicts() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0x40);
            idle_to(ctx, t0, 4);
            // External bus: the DMA's byte (M-cycle 4: byte 2, 4002).
            assert_eq!(ctx.read(0x0150), 2 ^ 0x3C);
            // WRAM: redirected to the DMA's bank half (bit 12 clear).
            ctx.mem.wram[0x123] = 0x42;
            assert_eq!(ctx.read(0xD123), 0x42);
            ctx.write(0xD124, 0x43); // M-cycle 6, goes to C124
            assert_eq!(ctx.mem.wram[0x124], 0x43);
            // An external-bus write is dropped, and its data replaces the
            // OAM byte just transferred (byte 5; CGB C, ADR 0028).
            ctx.write(0x2000, 0x05); // M-cycle 7
            assert_eq!(ctx.mem.oam[5], 0x05);
            assert_eq!(ctx.mem.oam[4], 4 ^ 0x3C);
            assert_eq!(ctx.sys.cart.read_rom(0x4000), 0x3C); // bank unchanged
            // VRAM is free.
            ctx.mem.vram[5] = 0x31;
            assert_eq!(ctx.read(0x8005), 0x31);
        });
    }

    #[test]
    fn high_source_reads_ff_and_opens_the_external_bus() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xFE);
            idle_to(ctx, t0, 3);
            assert_eq!(ctx.read(0x0150), 0xFF);
            ctx.mem.wram[0x1456] = 0x24;
            assert_eq!(ctx.read(0xC456), 0x24); // FE00: bit 12 set, D456
            idle_to(ctx, t0, 162);
        });
        assert!(emu.mem.oam.iter().all(|&b| b == 0xFF));
    }

    /// Without HALT (STOP, lockup: not paused, `docs/reference/oam-dma.md`)
    /// the idle skip stops at the `Dma` deadline like any other.
    #[test]
    fn unhalted_skip_stops_at_the_dma_deadline() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            assert_eq!(ctx.hot.dma, 0xFE00);
            // Other deadlines may come first (in CGB mode the first
            // DIV-APU edge is 352 clocks after power on, ADR 0022): skip
            // until the transfer ends; no skip may pass its deadline.
            // Each skip advances the clock, so a few dozen iterations reach
            // the deadline; the cap turns a stuck skip into a failure.
            let mut skips = 0;
            while ctx.hot.dma != 0 {
                skips += 1;
                assert!(
                    skips <= 1000,
                    "idle skip made no progress: dma still {:#06X} after {skips} skips, clock {} (deadline {})",
                    ctx.hot.dma,
                    ctx.clock(),
                    t0 + 162 * 4
                );
                ctx.idle_halted();
                assert!(ctx.clock() <= t0 + 162 * 4);
            }
            assert_eq!(ctx.clock(), t0 + 162 * 4);
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.mem.oam[0x9F], 0x9F ^ 0xA5);
        });
    }

    /// HALT in M-cycle 10 of a transfer, 50 halted M-cycles, wake-up. The
    /// transfer makes no progress while halted, OAM stays blocked, and the
    /// end moves out by the paused time (SameBoy `GB_dma_run`).
    #[test]
    fn halt_pauses_the_transfer() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 10);
            ctx.read(0x0100); // HALT's fetch: its step counts
            ctx.halt_begin();
            let f = ctx.clock();
            assert_eq!(ctx.sys.ppu.dma.copied, 10);
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), NEVER);
            for _ in 0..50 {
                ctx.idle();
                ctx.dma_sync(ctx.clock());
                assert_eq!(ctx.sys.ppu.dma.copied, 10);
                assert_eq!(ctx.hot.dma, 0xFE00);
                assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE00), 0xFF);
                assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE9F), 0xFF);
            }
            assert_eq!(ctx.mem.oam[10], 0x55);
            // The wake-up M-cycle: pushed out by 50 M-cycles.
            ctx.halt_end();
            assert_eq!(ctx.clock(), f + 50 * 4);
            let end = t0 + (162 + 50) * 4;
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), end);
            ctx.idle();
            ctx.dma_sync(ctx.clock());
            assert_eq!(ctx.sys.ppu.dma.copied, 11);
            while ctx.clock() < end - 4 {
                ctx.idle();
            }
            assert_eq!(ctx.hot.dma, 0xFE00);
            ctx.idle();
            assert_eq!((ctx.clock(), ctx.hot.dma), (end, 0));
        });
        assert!((0..0xA0).all(|i| emu.mem.oam[i] == i as u8 ^ 0xA5));
    }

    /// A CPU write on the bus the transfer reads, in the M-cycle of byte
    /// 5: it never reaches its target, and OAM byte 5 gets the CPU's data
    /// (ROM/SRAM and E000-FFFF sources), 00 (VRAM source) or keeps the
    /// DMA's byte (WRAM source). gambatte cgb04c `oamdma_src*_busypush*`,
    /// ADR 0028.
    #[test]
    fn conflicting_write_lands_in_oam_by_source() {
        // (FF46 value, CPU write address, OAM byte 5 afterwards).
        let cases = [
            (0x40, 0xA123, 0x9A),
            (0x40, 0x7FFF, 0x9A),
            (0x80, 0x9123, 0x00),
            (0xC1, 0xD123, 5 ^ 0xA5),
            (0xFE, 0x2000, 0x9A),
            (0xFE, 0xB000, 0x9A),
        ];
        for (src, addr, want) in cases {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let t0 = ctx.clock();
                ctx.write(0xFF46, src);
                idle_to(ctx, t0, 2 + 5);
                ctx.write(addr, 0x9A);
                ctx.dma_sync(ctx.clock());
            });
            assert_eq!(emu.mem.oam[5], want, "source {src:02X}, write {addr:04X}");
            match addr {
                0x9123 => assert_eq!(emu.mem.vram[0x1123], 0),
                0xD123 => assert_eq!(emu.mem.wram[0x1123], 0),
                _ => {}
            }
        }
    }

    /// A CPU read on the VRAM bus during a transfer from VRAM returns the
    /// byte being transferred and clears that OAM byte (gambatte cgb04c
    /// `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`, ADR 0028 amendment):
    /// byte 5 (not yet copied when read), byte 6 (copied by a sync at the
    /// same clock, before the read) and byte 40 (a second word of the
    /// record), all applied by the next sync. A WRAM source's conflicting
    /// read leaves OAM alone.
    #[test]
    fn conflicting_vram_read_clears_the_oam_byte() {
        for (src, addr) in [(0x80u8, 0x9123), (0xC1, 0xD123)] {
            let emu = setup();
            for (i, b) in emu.mem.vram[..0xA0].iter_mut().enumerate() {
                *b = i as u8 ^ 0x69;
            }
            let byte = |i: usize| {
                if src == 0x80 {
                    i as u8 ^ 0x69
                } else {
                    i as u8 ^ 0xA5
                }
            };
            with_ctx(emu, |ctx| {
                let t0 = ctx.clock();
                ctx.write(0xFF46, src);
                idle_to(ctx, t0, 2 + 5);
                assert_eq!(ctx.read(addr), byte(5));
                ctx.dma_sync(ctx.clock());
                assert_eq!(ctx.read(addr), byte(6));
                idle_to(ctx, t0, 2 + 40);
                assert_eq!(ctx.read(addr), byte(40));
                ctx.idle();
                ctx.dma_sync(ctx.clock());
            });
            let cleared = |i: usize| src == 0x80 && matches!(i, 5 | 6 | 40);
            for (i, &b) in emu.mem.oam[..=41].iter().enumerate() {
                let want = if cleared(i) { 0 } else { byte(i) };
                assert_eq!(b, want, "source {src:02X}, byte {i}");
            }
            assert!(emu.mem.dma_read_clears.is_empty());
        }
    }

    /// The halted skip passes the (cleared) `Dma` deadline: a paused
    /// transfer is no deadline (ADR 0011).
    #[test]
    fn halted_skip_passes_the_dma_deadline() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // APU off: no frame-sequencer events.
            ctx.write(0xFF26, 0x00);
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 3);
            ctx.halt_begin();
            ctx.idle_halted();
            assert!(ctx.clock() > t0 + 1_000_000);
            assert_eq!(ctx.hot.dma, 0xFE00);
            ctx.dma_sync(ctx.clock());
            assert_eq!(ctx.sys.ppu.dma.copied, 2);
        });
    }

    /// The CPU's hooks: `LD A,C1; LDH (46),A; HALT`, 40 halted M-cycles, an
    /// IRQ with IME off, then `LD A,(D000)`, which conflicts with the WRAM
    /// source and reads the byte being transferred. SameBoy: the HALT's
    /// fetch M-cycle steps (byte 0), the halted ones including the wake-up
    /// M-cycle don't, the wake-up takes one step (byte 1), and the three
    /// M-cycles before the read step (bytes 2-4).
    #[test]
    fn wake_up_takes_one_dma_step() {
        let emu = setup_with(&[0x3E, 0xC1, 0xE0, 0x46, 0x76, 0xFA, 0x00, 0xD0, 0x18, 0xFE]);
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        let cpu = &mut hw.cpu;
        cpu.set_ime(false);
        ctx.sys.ie = 0x01;
        ctx.sys.iflags = 0;
        cpu.step(&mut ctx);
        cpu.step(&mut ctx);
        let w = ctx.clock() - 4;
        cpu.step(&mut ctx);
        assert!(cpu.halted());
        assert_eq!(ctx.clock(), w + 8);
        for _ in 0..40 {
            ctx.idle();
        }
        ctx.sys.iflags = 0x01;
        cpu.step(&mut ctx); // wake-up M-cycle, LD A,(D000)
        assert!(!cpu.halted());
        assert_eq!(cpu.registers().a, 4 ^ 0xA5);
        assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), w + (162 + 40) * 4);
    }

    /// HALT with bytes left: the PPU sees the transfer halted until the
    /// end of the wake-up M-cycle (SameBoy clears `halted` after it), then
    /// running again, moved out by the halted time.
    #[test]
    fn halt_holds_the_ppu_window_while_bytes_are_left() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 10);
            let p = ctx.clock();
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(p), (Running, 9));
            assert_eq!(d.ppu_at(p + 1), (Halted, 9));
            assert_eq!(d.ppu_at(p + 5000), (Halted, 9));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(r + 4), (Halted, 9));
            assert_eq!(d.ppu_at(r + 5), (Running, 10));
            assert_eq!(d.ppu_to, d.end - 2);
            assert_eq!(d.ppu_at(d.end - 2), (Running, 160));
            assert_eq!(d.ppu_at(d.end - 1), (Off, 160));
        });
    }

    /// The speed-switch halt after the last byte, before the end: unlike
    /// HALT, the transfer's last step is held for the PPU through the
    /// halt (gambatte cgb04c `oamdma/oamdma_late_speedchange_stat_1`).
    #[test]
    fn speed_switch_after_the_last_byte_holds_the_ppu() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            assert!(ctx.speed_switch(true, false));
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.copied, 160);
            assert_eq!(d.ppu_at(t0 + 647), (Halted, 160));
            assert_eq!(d.ppu_at(t0 + 5000), (Halted, 160));
        });
    }

    /// A switch to double speed with the APU on delays the APU by one
    /// 2 MHz tick (`Timer::apu_ds_delay`): a CH1 trigger in double speed
    /// then takes the other of its two delays than the power-on phase
    /// gives (gambatte cgb04c
    /// `speedchange_ch1_nr4init_duty0_pos6_to_pos7_timing_1`/`_2`). A
    /// power cycle in double speed clears the delay.
    #[test]
    fn a_switch_to_double_speed_with_the_apu_on_flips_the_trigger_phase() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // CH1 DAC on, f = 0x700 (1024 clocks per step), triggered off
            // at two consecutive M-cycles: the clocks to the first step
            // against gambatte's linear rule with the power-on 1 MHz phase
            // `lf_div` (`Apu::trigger_delay`), flipped by `flip`.
            let check = |ctx: &mut Ctx, flip: u64| {
                ctx.write(0xFF12, 0xF0);
                for _ in 0..2 {
                    let t = ctx.clock();
                    ctx.write(0xFF14, 0x87);
                    let apu = &ctx.mem.apu;
                    let first = apu.out.synced + u64::from(apu.sq[0].countdown);
                    let lf_div = ((t >> 1) ^ u64::from(apu.fs.phase) ^ 1) & 1;
                    assert_eq!(first - t, 1024 + 6 + 2 * (lf_div ^ flip), "at {t}");
                    // Off again (DAC off, on) for the next trigger.
                    ctx.write(0xFF12, 0x00);
                    ctx.write(0xFF12, 0xF0);
                }
            };
            ctx.write(0xFF26, 0x00);
            ctx.write(0xFF26, 0x80);
            assert!(ctx.speed_switch(true, false));
            while !ctx.speed_switch_over() {
                ctx.idle_speed_switch();
            }
            ctx.speed_switch_end();
            assert!(ctx.sys.timer.apu_ds_delay);
            check(ctx, 1);
            ctx.write(0xFF26, 0x00);
            ctx.write(0xFF26, 0x80);
            assert!(!ctx.sys.timer.apu_ds_delay);
            check(ctx, 0);
        });
    }

    /// HALT after the last byte: the transfer's last step isn't held for
    /// the PPU, which sees it end on time (gambatte
    /// `oamdma/oamdma_late_halt_stat_2`, cgb04c).
    #[test]
    fn halt_after_the_last_byte_does_not_hold_the_ppu() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(t0 + 646), (Halted, 160));
            assert_eq!(d.ppu_at(t0 + 647), (Off, 160));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            assert_eq!(ctx.sys.ppu.dma.ppu_at(r + 1), (Off, 160));
        });
    }

    /// HALT in the start delay: the transfer hasn't started for the PPU,
    /// and starts only after the wake-up.
    #[test]
    fn halt_in_the_start_delay_delays_the_ppu_window() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(t0 + 7), (Off, 0));
            assert_eq!(d.ppu_at(t0 + 5000), (Off, 0));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(r + 2), (Off, 0));
            assert_eq!(d.ppu_at(r + 5), (Running, 1));
        });
    }

    /// STAT reads mode 0 instead of 2 while a transfer runs (SameBoy
    /// `GB_STAT_update`); modes 3 and 0 read as usual, and mode 2 again
    /// once the transfer is over.
    #[test]
    fn stat_reads_mode_2_as_0_during_oam_dma() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // LCD on at E: frame position = clock - E + 7, so the access at
            // E + 4k is at position 4k + 7.
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, 100);
            ctx.write(0xFF46, 0xC1); // Line 0 (the short first line), HBlank.
            idle_to(ctx, e, 118); // Line 1, t = 23: mode 2.
            assert_eq!(ctx.read(0xFF41) & 3, 0);
            idle_to(ctx, e, 137); // Line 1, t = 99: mode 3.
            assert_eq!(ctx.read(0xFF41) & 3, 3);
            idle_to(ctx, e, 346); // Line 3, t = 23: mode 2, transfer over.
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.read(0xFF41) & 3, 2);
        });
    }

    /// An HBlank trigger while a block's CPU stall runs is lost; one at the
    /// stall's end starts the next block (gambatte acknowledges the
    /// request at a transfer's end; ADR 0017, trigger amendment).
    #[test]
    fn an_hblank_trigger_during_a_block_stall_is_lost() {
        for (at, kept) in [(8, false), (12, true)] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                ctx.write(0xFF40, 0x91);
                ctx.sys.vdma.hblank = true;
                ctx.sys.vdma.stall = 3;
                let now = ctx.clock();
                ctx.sys.sched.set(EventKind::VramDma, now + at);
                ctx.reschedule();
                ctx.vdma_stall(u64::MAX);
                assert_eq!(ctx.clock(), now + 12);
                assert_eq!(ctx.sys.vdma.stall, 0);
                assert_eq!(ctx.sys.vdma.pending, kept, "trigger at +{at}");
            });
        }
    }

    /// CGB B/C read STAT mode 0 at line 0 dot 3 in single speed, but an
    /// HBlank-HDMA start in that M-cycle must not see HBlank: no block
    /// starts now (review finding; SameBoy keeps mode 1 there).
    #[test]
    fn hblank_hdma_write_at_line_0_dot_3_does_not_start_a_block() {
        // Frame position = clock - E + 7; line 0 begins at 154 * 456.
        let m = (154 * 456 + 3 - 7) / 4;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, m);
            assert_eq!(ctx.read(0xFF41) & 3, 0, "STAT read keeps the quirk");
        });
        let emu = setup();
        with_ctx(emu, |ctx| {
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, m);
            ctx.write(0xFF55, 0x80);
            assert!(!ctx.sys.vdma.pending, "no immediate block");
            assert!(ctx.sys.vdma.hblank);
        });
    }

    /// CGB, single speed: a STAT write that switches the selected source
    /// to LY=LYC while both are active keeps the line high, so it requests
    /// no interrupt, although the LYC select lands one dot after the
    /// others (gambatte `miscmstatirq/lycstatwirq_trigger_m0_*_08_40`,
    /// CGB `E0`). The same write while mode 3 has the line low does.
    #[test]
    fn stat_write_switching_to_lyc_select_keeps_the_line_high() {
        // Frame position = clock - E + 7: line 2, t = 301 (mode 0) and
        // t = 101 (mode 3).
        for (m, want) in [
            ((2 * 456 + 301 - 7) / 4, 0),
            ((2 * 456 + 101 - 7) / 4, crate::ppu::INT_STAT),
        ] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let e = ctx.clock();
                ctx.write(0xFF45, 2);
                ctx.write(0xFF40, 0x91);
                idle_to(ctx, e, m);
                ctx.write(0xFF41, 0x08);
                ctx.sys.iflags = 0;
                ctx.write(0xFF41, 0x40);
                ctx.idle();
                assert_eq!(ctx.sys.iflags & crate::ppu::INT_STAT, want, "access at {m}");
            });
        }
    }

    /// CGB, single speed: [`stat_irq_after_write_with`] with LYC = FF.
    fn stat_irq_after_write(sel: u8, pos: u64, value: u8, n: u64) -> u8 {
        stat_irq_after_write_with(0xFF, sel, pos, value, n, false)
    }

    /// CGB: with LYC = `lyc` and the LCD on (frame position = clock - E +
    /// 3), selects `sel`, then (in double speed: 2-clock M-cycles from
    /// here) a STAT write of `value` with its access at frame position
    /// `pos`; returns IF's STAT bit from the write to `n` M-cycles after
    /// it.
    fn stat_irq_after_write_with(
        lyc: u8,
        sel: u8,
        pos: u64,
        value: u8,
        n: u64,
        double: bool,
    ) -> u8 {
        irq_after_write(0, lyc, sel, pos, 0xFF41, value, n, double)
    }

    /// [`stat_irq_after_write_with`] with SCX = `scx` and the write to
    /// `reg` (STAT or LYC).
    #[allow(clippy::too_many_arguments)]
    fn irq_after_write(
        scx: u8,
        lyc: u8,
        sel: u8,
        pos: u64,
        reg: u16,
        value: u8,
        n: u64,
        double: bool,
    ) -> u8 {
        let emu = setup();
        let mut stat = 0;
        with_ctx(emu, |ctx| {
            if scx != 0 {
                ctx.write(0xFF43, scx);
            }
            let e = ctx.clock();
            ctx.write(0xFF45, lyc);
            ctx.write(0xFF40, 0x91);
            ctx.write(0xFF41, sel);
            // The switch's own timing is not under test.
            ctx.hot.mcycle = if double { 2 } else { 4 };
            while ctx.clock() < e + pos - 3 {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), e + pos - 3);
            ctx.sys.iflags = 0;
            ctx.write(reg, value);
            while ctx.clock() < e + pos - 3 + n * ctx.hot.mcycle as u64 {
                ctx.idle();
            }
            stat = ctx.sys.iflags & crate::ppu::INT_STAT;
        });
        stat
    }

    /// Line 143's mode 0 ends for STAT writes at line 144 dot 3, and a
    /// write from there to VBlank (dot 5) no longer changes the selects
    /// that block the mode-1 interrupt (gambatte `m1/ly143_late_m0enable`,
    /// `m1irq_m0disable` `_1`/`_2`, CGB04C; `edges::MODE_HOLD_SS`).
    #[test]
    fn line_144_stat_writes_from_dot_3_keep_the_mode_part() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let (l143_455, l144_3) = (143 * 456 + 455, 144 * 456 + 3);
        // Enabling mode 0: on time, then too late.
        assert_eq!(stat_irq_after_write(0, l143_455, 0x08, 1), STAT);
        assert_eq!(stat_irq_after_write(0, l144_3, 0x08, 3), 0);
        // Clearing mode 0 with mode 1 selected: the mode-1 interrupt
        // follows, then stays blocked by the old select.
        assert_eq!(stat_irq_after_write(0x18, l143_455, 0x10, 3), STAT);
        assert_eq!(stat_irq_after_write(0x18, l144_3, 0x10, 3), 0);
        // Line 0 dot 3: a mode-1 enable is too late (`m1irq_late_enable`).
        let (l153_455, l0_3) = (153 * 456 + 455, 154 * 456 + 3);
        assert_eq!(stat_irq_after_write(0x40, l153_455, 0x10, 1), STAT);
        assert_eq!(stat_irq_after_write(0x40, l0_3, 0x10, 1), 0);
    }

    /// Line 144's mode-2 quirk interrupt is at dot 3, once, and with mode
    /// 2 selected the mode-1 interrupt at VBlank is blocked (gambatte
    /// `m1/m2m1irq_ifw_2`, `ly143_late_m2enable_2`, CGB04C;
    /// `edges::QUIRK2`).
    #[test]
    fn line_144_mode_2_quirk_once_at_dot_3() {
        const STAT: u8 = crate::ppu::INT_STAT;
        // Writes of the same selects: the line doesn't change. From line
        // 143 dot 455: the quirk at dot 3.
        assert_eq!(stat_irq_after_write(0x30, 143 * 456 + 455, 0x30, 2), STAT);
        // From dot 3, with the quirk already in: nothing at VBlank.
        assert_eq!(stat_irq_after_write(0x30, 144 * 456 + 3, 0x30, 3), 0);
        // Mode 2 enabled at dot 3 is too late for the quirk, and VBlank
        // doesn't repeat it.
        assert_eq!(stat_irq_after_write(0, 144 * 456 + 3, 0x20, 3), 0);
    }

    /// Single speed: a STAT write's first phase at the comparison-off dot
    /// (t = 3) sees LY=LYC off, so switching from LYC to mode 1 there
    /// requests the interrupt; a dot before it the LYC source still holds
    /// the line (gambatte `m1/m1irq_enable_after_lyc144_{1,2}`, CGB04C).
    #[test]
    fn stat_write_at_the_comparison_off_dot_sees_ly_lyc_off() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |pos| stat_irq_after_write_with(144, 0x40, pos, 0x10, 2, false);
        assert_eq!(w(144 * 456 + 455), 0);
        assert_eq!(w(145 * 456 + 3), STAT);
    }

    /// Double speed: a STAT write's second phase, half a dot after the
    /// first, sees the comparison of that half dot: at line 144 t = 3.5
    /// LY=LYC (143) still holds the line, so switching LYC for mode 0
    /// keeps it high (gambatte `m1/lyc143_late_m0enable_lycdisable_ds_1`,
    /// CGB04C: no STAT interrupt).
    #[test]
    fn double_speed_stat_phase_2_sees_the_comparison_half_a_dot_in() {
        assert_eq!(
            stat_irq_after_write_with(143, 0x40, 144 * 456 + 3, 0x08, 2, true),
            0
        );
    }

    /// Single speed: before `t = 3` (the mode-2 interrupt) a STAT write
    /// sees the previous line's mode 0, not mode 2: dropping the LYC
    /// select at line 2 dot 0 with only mode 2 left lowers the line, and
    /// line 2's mode-2 interrupt follows (gambatte
    /// `m2enable/lyc1_m2irq_late_lycdisable_1`, CGB04C).
    #[test]
    fn stat_write_before_dot_3_sees_mode_0() {
        const STAT: u8 = crate::ppu::INT_STAT;
        assert_eq!(
            stat_irq_after_write_with(1, 0x60, 456 + 455, 0x20, 2, false),
            STAT
        );
    }

    /// Double speed: a STAT write's first phase enabling mode 2 at t = 5
    /// still requests the mode-2 interrupt, at t = 7 not (gambatte
    /// `m2enable/late_enable_ds_{1,2}`, CGB04C); on line 0 the same
    /// (`late_enable_ly0_ds_{1,2}`).
    #[test]
    fn double_speed_mode_2_enable_window() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |pos| stat_irq_after_write_with(0xFF, 0, pos, 0x20, 1, true);
        assert_eq!(w(136 * 456 + 5), STAT);
        assert_eq!(w(136 * 456 + 7), 0);
        assert_eq!(w(154 * 456 + 5), STAT);
        assert_eq!(w(154 * 456 + 7), 0);
    }

    /// STAT writes just before the mode-0 interrupt (line 2, SCX 0: at
    /// t = 257; SCX 1: 258) no longer change the selects it sees: within
    /// 2 dots of the access in single speed, 1 in double speed
    /// (`edges::M0_STAT_LATCH`; gambatte `m0enable/disable_*`,
    /// `lycdisable_ff41_scx1_ds_*`, CGB04C).
    #[test]
    fn stat_writes_just_before_the_mode_0_interrupt_are_too_late() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let at = 2 * 456 + 255;
        // Single speed, clearing mode 0: 2 dots before, the interrupt
        // still comes; 3 dots before, not.
        let w = |scx| irq_after_write(scx, 0xFF, 0x08, at, 0xFF41, 0, 3, false);
        assert_eq!(w(0), STAT);
        assert_eq!(w(1), 0);
        // Clearing the LYC select while LY=LYC (2): 2 dots before, the
        // interrupt stays blocked; 3 dots before, unblocked.
        let w = |scx, at, double| irq_after_write(scx, 2, 0x48, at, 0xFF41, 0x08, 3, double);
        assert_eq!(w(0, at, false), 0);
        assert_eq!(w(1, at, false), STAT);
        // Double speed: 1 dot before, blocked; 2 dots before, not.
        assert_eq!(w(1, at + 2, true), 0);
        assert_eq!(w(0, at, true), STAT);
    }

    /// A LYC write ending LY=LYC just before the mode-0 interrupt (line 2)
    /// doesn't unblock it: within 6 dots of the access in single speed, 2
    /// in double speed (`edges::M0_LYC_LATCH`; gambatte
    /// `m0enable/lycdisable_ff45_*`, CGB04C).
    #[test]
    fn lyc_writes_just_before_the_mode_0_interrupt_still_block_it() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |scx, at, double| irq_after_write(scx, 2, 0x48, at, 0xFF45, 0xFF, 4, double);
        // Single speed: 6 dots before (SCX 0), 7 (SCX 1).
        assert_eq!(w(0, 2 * 456 + 251, false), 0);
        assert_eq!(w(1, 2 * 456 + 251, false), STAT);
        // Double speed: 2 dots before (SCX 0), 3 (SCX 1).
        assert_eq!(w(0, 2 * 456 + 255, true), 0);
        assert_eq!(w(1, 2 * 456 + 255, true), STAT);
    }

    /// Single speed: clearing the mode-1 select 1-2 dots before line
    /// 153's LY=LYC comparison doesn't unblock the LYC=153 interrupt
    /// (gambatte `lycEnable/lyc153_late_m1disable_{1,2}`, CGB04C).
    #[test]
    fn mode_1_clear_just_before_the_line_153_comparison_still_blocks() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |pos| stat_irq_after_write_with(153, 0x50, pos, 0x40, 3, false);
        assert_eq!(w(152 * 456 + 455), STAT);
        assert_eq!(w(153 * 456 + 3), 0);
    }

    /// Single speed, line 153: a LYC write is compared with 153 up to dot
    /// 7, the held LY=LYC (152) dropped from dot 4 (view; access at 3), and
    /// not from dot 12 (gambatte `lycEnable/lyc153_late_ff45_enable_{3,5}`,
    /// CGB04C).
    #[test]
    fn line_153_lyc_write_windows() {
        const STAT: u8 = crate::ppu::INT_STAT;
        for (lyc, pos, want) in [(152, 153 * 456 + 3, STAT), (0xFF, 153 * 456 + 11, 0)] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let e = ctx.clock();
                ctx.write(0xFF45, lyc);
                ctx.write(0xFF40, 0x91);
                ctx.write(0xFF41, 0x40);
                idle_to(ctx, e, (pos - 3) / 4);
                ctx.sys.iflags = 0;
                ctx.write(0xFF45, 153);
                ctx.idle();
                assert_eq!(ctx.sys.iflags & STAT, want, "access at {pos}");
            });
        }
    }

    /// LCD off with the coincidence bit kept set: a STAT write newly
    /// selecting LY=LYC requests the interrupt, one keeping it selected
    /// doesn't (gambatte `memory.cpp` FF41; `lycEnable/lcdoff_lycirqen_1`).
    #[test]
    fn lcd_off_lyc_select_with_the_kept_coincidence_bit() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let emu = setup();
        with_ctx(emu, |ctx| {
            ctx.write(0xFF45, 0);
            ctx.write(0xFF40, 0x91);
            ctx.idle();
            ctx.write(0xFF40, 0x11);
            ctx.sys.iflags = 0;
            ctx.write(0xFF41, 0x40);
            assert_eq!(ctx.sys.iflags & STAT, STAT);
            ctx.sys.iflags = 0;
            ctx.write(0xFF41, 0x40);
            assert_eq!(ctx.sys.iflags & STAT, 0);
        });
    }
}
