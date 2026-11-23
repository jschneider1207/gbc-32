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
use crate::ppu::{Frame, Ppu};
use crate::schedule::{EventKind, NEVER, Schedule};
use crate::serial::{INT_SERIAL, Serial};
use crate::timer::Timer;
use crate::{Buttons, Emulator, Events};

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
    /// rebased or the APU powers on. The caller reschedules.
    pub(crate) fn set_div_apu_deadline(&mut self, powered: bool, now: u64) {
        let at = if powered {
            let k = self.timer.div_apu_bit() + 1;
            self.timer.clock_at(((self.timer.sc(now) >> k) + 1) << k)
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
        self.cgb_mode = self.cart.rom.get(0x143).is_some_and(|&f| f & 0x80 != 0);
        self.timer = Timer::post_boot();
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

/// A BCPD/OCPD write in CGB mode: stores `value` at the palette RAM byte the
/// index register (BCPS/OCPS) selects in bits 0-5, then, if its bit 7
/// (auto-increment) is set, increments bits 0-5, wrapping 3F to 00 and
/// keeping bit 7 (Pan Docs: LCD Color Palettes, FF68 BCPS; SameBoy
/// `write_high_memory`, GB_IO_BGPD). Returns the byte written.
fn palette_data_write(index: &mut u8, ram: &mut [u8; 64], value: u8) -> usize {
    let i = usize::from(*index & 0x3F);
    ram[i] = value;
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
    stop_on: Events,
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
        if old >> self.sys.timer.div_apu_bit() & 1 != 0 {
            let now = self.clock();
            self.apu_sync(now);
            self.mem.apu.step();
        }
        self.sys.set_div_apu_deadline(true, self.clock());
    }

    /// Brings the APU up to `now`, writing completed samples into the
    /// caller's buffer (ADR 0021 section 3).
    #[inline(never)]
    pub(crate) fn apu_sync(&mut self, now: u64) {
        let Memory { apu, io, .. } = &mut *self.mem;
        apu.sync(now, io, self.audio, &mut self.audio_len);
    }

    /// The start of a run call: the APU is brought to the clock (advance
    /// only, if a caller left it behind), and with buffer room the
    /// `AudioBuffer` deadline is where the last slot's window ends. The
    /// caller reschedules.
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

    /// A DIV-APU falling edge (`EventKind::DivApu`, due at `now`).
    #[inline(never)]
    fn div_apu_event(&mut self, now: u64) {
        self.apu_sync(now);
        self.mem.apu.step();
        self.sys.set_div_apu_deadline(true, now);
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
            // Channel registers and wave RAM: stored; the channels decode
            // them from task 1(b) on.
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
            let high = self.sys.timer.sc(now) >> self.sys.timer.div_apu_bit() & 1 != 0;
            self.mem.apu.power_on(now, high);
        } else {
            self.mem.apu.power_off();
            self.mem.io[0x10..=0x25].fill(0);
        }
        self.sys.set_div_apu_deadline(on, now);
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
                EventKind::Serial => {
                    let done = self.sys.serial.shift();
                    self.serial_done(done);
                    // `boundary` reschedules after the loop.
                    let at = self.serial_deadline(now);
                    self.sys.sched.set_inline(EventKind::Serial, at);
                }
                EventKind::Ppu => self.sync_ppu(),
                EventKind::Input => self.apply_inputs(now),
                EventKind::Dma => {
                    self.dma_sync(now);
                    self.hot.dma = 0;
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

    /// Applies the scheduled button changes due at the M-cycle boundary
    /// `now`: those at a clock inside the M-cycle that starts at `now` or
    /// earlier (rounded down, ADR 0012), so accesses from `now` on see
    /// them. Then schedules the next one.
    fn apply_inputs(&mut self, now: u64) {
        let m = self.hot.mcycle as u64;
        let s = &mut *self.sys;
        if let Some(buttons) = s.inputs.take_due(now + m - 1) {
            s.set_joypad(buttons, s.joyp_select);
        }
        s.schedule_inputs(m);
    }

    /// Catches the PPU up to the current clock and reschedules.
    #[inline(never)]
    fn sync_ppu(&mut self) {
        let now = self.clock();
        self.sync_ppu_to(now);
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
                let s = &mut *self.sys;
                s.ppu.flush(now, self.mem, self.frame);
                // Turning the LCD off outside mode 0 starts an HBlank
                // transfer block (SameBoy `GB_lcd_off`).
                if s.vdma.hblank && value & 0x80 == 0 && s.ppu.stat(now, double) & 3 != 0 {
                    s.vdma.pending = true;
                    self.done = true;
                }
                let s = &mut *self.sys;
                s.ppu.write_lcdc(now, value, &mut s.iflags, double);
                s.ppu.check_wy_at(now, self.mem);
            }
            0x41 => {
                // CGB: in two phases one dot apart; for the first, the LYC
                // select (double speed: the mode-0 select) keeps its old
                // value (SameBoy `GB_CONFLICT_STAT_CGB`, `_DOUBLE`).
                let keep = if double { 0x08 } else { 0x40 };
                let first = (value & !keep) | (self.sys.ppu.stat_select() & keep);
                self.run_ppu_to_call(now);
                let s = &mut *self.sys;
                s.ppu.write_stat(now, first, &mut s.iflags, double);
                self.run_ppu_to_call(now + 1);
                let s = &mut *self.sys;
                s.ppu.write_stat(now + 1, value, &mut s.iflags, double);
            }
            0x44 => {} // LY is read-only.
            0x45 => {
                // Single speed: one dot into the M-cycle (SameBoy
                // `GB_CONFLICT_WRITE_CPU`); double speed: at the access
                // (`GB_CONFLICT_READ_OLD`).
                let at = now + u64::from(!double);
                self.run_ppu_to_call(at);
                let s = &mut *self.sys;
                s.ppu.write_lyc(at, value, &mut s.iflags, double);
            }
            // SCY SCX BGP OBP0 OBP1 WY WX: the pixels already output in
            // mode 3 are drawn with the old value first. Where in the
            // M-cycle the write lands (SameBoy `cgb_conflict_map`,
            // single speed): SCY, SCX and WY at the access
            // (`GB_CONFLICT_READ_OLD`); the palettes one dot earlier on
            // CGB C (`GB_CONFLICT_PALETTE_CGB`, `pending_cycles - 1`;
            // Mealybug `m3_bgp_change`, `m3_obp0_change` CGB C
            // screenshots); WX one dot later (`GB_CONFLICT_WRITE_CPU`).
            // Double speed: at the access (a CPU cycle is half a dot
            // there; approximation).
            _ => {
                let at = match reg {
                    _ if double => now,
                    0x47..=0x49 => now - 1,
                    0x4B => now + 1,
                    _ => now,
                };
                self.run_ppu_to_call(now.max(at));
                let s = &mut *self.sys;
                s.ppu.flush(at, self.mem, self.frame);
                self.mem.io[reg] = value;
                match reg {
                    0x47..=0x49 => s.ppu.write_palette(
                        reg,
                        value,
                        &s.config.dmg_palettes,
                        s.cgb_mode,
                        &mut self.mem.bg_pairs,
                    ),
                    0x4A => s.ppu.check_wy_at(now, self.mem),
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

    /// A BCPD (`obj` false) or OCPD (`obj` true) write in CGB mode: draws
    /// the pixels output so far with the old colors, stores the byte at
    /// BCPS/OCPS and re-resolves the color. Out of line: rare next to the
    /// other I/O writes (hi-color raster effects do it a few times a line).
    #[inline(never)]
    fn write_palette_data(&mut self, obj: bool, value: u8) {
        let now = self.clock();
        self.run_ppu_to(now);
        let s = &mut *self.sys;
        // Synced to `now`: outside mode 3 there is nothing to draw.
        if s.ppu.drawing() {
            s.ppu.flush(now, self.mem, self.frame);
        }
        if obj {
            let i = palette_data_write(&mut self.mem.io[0x6A], &mut self.mem.obj_pal, value);
            s.ppu.write_ocpd(i, self.mem);
        } else {
            let i = palette_data_write(&mut self.mem.io[0x68], &mut self.mem.bg_pal, value);
            s.ppu.write_bcpd(i, self.mem);
        }
    }

    /// FF00-FF7F, by the low 7 address bits.
    fn write_io(&mut self, reg: usize, value: u8) {
        let s = &mut *self.sys;
        let cgb = s.cgb_mode;
        match reg {
            0x00 => s.set_joypad(s.buttons, value),
            0x01 => s.serial.sb = value,
            0x02 | 0x04..=0x07 => self.write_timer_io(reg, value),
            0x0F => s.iflags = value & INT_MASK,
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
            // `write_high_memory`). TODO(M5): access blocked in mode 3.
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
        self.dma_ppu_event(now);
        self.mem.io[0x46] = value;
        let m = self.hot.mcycle as u64;
        let restart = self.hot.dma != 0 && {
            self.dma_sync(now);
            now + m < self.sys.ppu.dma.end
        };
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
        self.sys.ppu.dma = d;
        self.mem.oam_dma_old = self.mem.oam;
        self.hot.dma = 0xFE00;
        self.sys.sched.set(EventKind::Dma, end);
        self.reschedule();
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
    }

    /// A CPU write while an OAM DMA transfer runs: OAM writes are dropped
    /// for the whole transfer, start delay included (SameBoy
    /// `write_high_memory`), and the rest follows [`dma::conflict`] for
    /// CGB C (SameBoy `GB_write_memory`). The transfer is synced first,
    /// because the write may change a source byte or its mapping.
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
            (Conflict::Open, _) => {}
            (Conflict::Wram(a), _) => self.write_to(a, value),
            (Conflict::Dma, i) => {
                // CGB C: the OAM byte just written is cleared when the
                // DMA reads below A000 (SameBoy: `oam[dest - 1] = 0`).
                if self.sys.ppu.dma.base < 0xA000 {
                    self.mem.oam[usize::from(i)] = 0;
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
    ///   `div_cycles = -4`, the DIV reset held).
    fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        self.sys.key1 = 0;
        let m = 6 - self.hot.mcycle as u64;
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        let dma_paused = self.sys.ppu.dma.paused != NEVER;
        if dma_paused {
            self.dma_retime(m);
        }
        self.hot.mcycle = m as i32;
        let now = self.clock();
        let mut freeze = if ime { 0 } else { 4 };
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
        // The DIV-APU bit changes (12 <-> 13) with the counter speed.
        let powered = self.mem.apu.powered();
        self.sys.set_div_apu_deadline(powered, now);
        if !pending && self.sys.vdma.hblank {
            self.vdma_halt(true);
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

    /// As [`Bus::halt_end`], except that the end of the countdown (not an
    /// interrupt) starts no HDMA block: SameBoy clears `halted` in
    /// `GB_advance_cycles` without the wake-up's `hdma_on`.
    #[inline]
    pub(crate) fn speed_switch_end(&mut self) {
        if self.sys.vdma.halted && self.speed_switch_over() {
            self.sys.vdma.halted = false;
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
    /// `n` idle M-cycles (the CPU stalled), skipped a deadline at a time
    /// (ADR 0011), each deadline handled on the M-cycle it was before.
    fn idle_mcycles(&mut self, mut n: u32) {
        while n > 0 {
            let k = self.mcycles_to_deadline().min(n);
            self.hot.budget -= (k * self.hot.mcycle as u32) as i32;
            n -= k;
            if self.hot.budget <= 0 {
                self.boundary();
            }
        }
    }

    /// Stores the FF55 read-back where `read_io` reads it.
    fn vdma_sync_reg(&mut self) {
        self.mem.io[0x55] = self.sys.vdma.read_hdma5();
    }

    /// STAT mode 0 now (also with the LCD off).
    fn stat_mode0(&self) -> bool {
        self.sys.ppu.stat(self.clock(), self.hot.mcycle == 2) & 3 == 0
    }

    /// A transfer step is due: the CPU stops after this instruction so
    /// the run loop can run it ([`Emulator::run`]).
    fn vdma_request(&mut self) {
        self.sys.vdma.pending = true;
        self.done = true;
    }

    /// FF55 write. An HBlank transfer that starts outside mode 0 waits
    /// for the next HBlank; one that starts in mode 0 does its first
    /// block now, and the current line's HBlank (if still ahead) doesn't
    /// start another.
    #[inline(never)]
    fn write_hdma5(&mut self, value: u8) {
        let now = self.clock();
        if value & 0x80 != 0 {
            // Latch the current line, if due, before the transfer is
            // active.
            self.run_ppu_to_call(now);
            self.schedule_ppu();
        }
        let mode0 = self.stat_mode0();
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
    /// is halted (SameBoy: `!gb->halted`) or the transfer was stopped.
    fn vdma_hblank(&mut self) {
        let v = &self.sys.vdma;
        if v.hblank && !v.halted && self.sys.ppu.lcd_enabled() {
            self.vdma_request();
        }
    }

    /// HALT begins (`halt`) or ends during an HBlank transfer. Waking up
    /// in mode 0 from a HALT that began outside it starts a block
    /// (SameBoy `allow_hdma_on_wake`).
    #[cold]
    fn vdma_halt(&mut self, halt: bool) {
        let mode0 = self.stat_mode0();
        let v = &mut self.sys.vdma;
        v.halted = halt;
        if halt {
            v.wake_ok = !mode0;
        } else if v.hblank && v.wake_ok && mode0 {
            self.vdma_request();
        }
    }

    /// Runs the pending transfer step at an instruction fetch: all blocks
    /// of a GDMA, or one HDMA block, then stalls the CPU for it
    /// ([`hdma::stall_mcycles`]). The bytes are copied at once at the
    /// start, after the PPU has drawn everything before it.
    ///
    /// Sources: ROM, cartridge RAM and WRAM are read; VRAM and E000-FFFF
    /// read FF (SameBoy reads the CPU data bus there, which this core does
    /// not model).
    pub(crate) fn vram_dma(&mut self) {
        let now = self.clock();
        self.run_ppu_to_call(now);
        if self.sys.ppu.drawing() {
            self.sys.ppu.flush(now, self.mem, self.frame);
        }
        let bank = self.sys.vram_off;
        let mut v = self.sys.vdma;
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
        self.sys.vdma = v;
        self.vdma_sync_reg();
        self.idle_mcycles(hdma::stall_mcycles(blocks, self.hot.mcycle as u32));
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

    /// SameBoy `stop()`: a button held on a selected P1 line ends STOP at
    /// once, with no DIV reset and no speed switch (Pan Docs: "Using the
    /// STOP instruction"). Otherwise STOP resets DIV (Pan Docs: FF04).
    fn stop_begin(&mut self) -> StopKind {
        if p1_lines(self.sys.buttons, self.sys.joyp_select) != 0x0F {
            return StopKind::Joypad;
        }
        self.div_reset();
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

    fn halt_begin(&mut self) {
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        if self.sys.vdma.hblank {
            self.vdma_halt(true);
        }
    }

    fn halt_end(&mut self) {
        if self.sys.ppu.dma.paused != NEVER {
            self.dma_resume();
        }
        if self.sys.vdma.halted {
            self.vdma_halt(false);
        }
    }
}

/// A read as the CPU sees it at the current clock, without side effects
/// (no read on this hardware has any). Shared by the CPU bus and
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
/// conflict in the start delay.
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
/// free, like [`read`].
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
        (Conflict::Dma, i) => sys.ppu.dma.base + u16::from(i),
    };
    read::<true>(mem, sys, &plain, addr)
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
        // BCPD/OCPD read the palette RAM byte at the index, without
        // incrementing it (SameBoy `read_high_memory`; Pan Docs: FF69).
        // TODO(M5): FF while the PPU reads palettes (mode 3).
        0x69 if cgb => mem.bg_pal[usize::from(mem.io[0x68] & 0x3F)],
        0x6B if cgb => mem.obj_pal[usize::from(mem.io[0x6A] & 0x3F)],
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
        0x74 if cgb => mem.io[reg],
        0x75 => 0x8F | mem.io[reg],
        // FF76/FF77 PCM12/PCM34 (read only, on CGB in either mode; Mooneye
        // `misc/bits/unused_hwio-C` reads 00 with the APU silent): above.
        _ => 0xFF,
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

    /// A SCX write in mode 3 moves the BG from the pixel being output at
    /// the access on (`GB_CONFLICT_READ_OLD`).
    #[test]
    fn mid_mode3_scx_write_splits_the_line() {
        let emu = striped_bg();
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        // The write at t = 151: pixels 0-54 are out with SCX = 0.
        let row = write_in_mode3(emu, 7, 151, 0xFF43, 8);
        for (x, &p) in row.iter().enumerate() {
            let scx = if x < 55 { 0 } else { 8 };
            let want = if ((x + scx) / 8) % 2 == 0 {
                gray[3]
            } else {
                gray[0]
            };
            assert_eq!(p, want, "x = {x}");
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
    /// `misc/boot_div-cgbABCDE` check that depend on the mode or phase.
    #[test]
    fn post_boot_state_of_a_dmg_mode_cartridge() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        assert_eq!(emu.peek(0xFF00), 0xFF);
        assert_eq!((emu.peek(0xFF68), emu.peek(0xFF6A)), (0xC8, 0xD0));
        // DIV ticks to 27 at clock 136 (34 M-cycles).
        let t = Timer::post_boot();
        assert_eq!(
            (t.div(132), t.div(136), t.div(136 + 255)),
            (0x26, 0x27, 0x27)
        );
        assert_eq!(emu.peek(0xFF04), 0x26);
        // CGB mode leaves P1 alone.
        let emu = load(rom(0x8000, 0, 0, 0x80));
        assert_eq!(emu.peek(0xFF00), 0xCF);
    }

    // --- APU framework (ADR 0021; M6 task 1(a)) ---

    fn run_audio(emu: &mut Emulator, clocks: u64, audio: &mut [StereoSample]) -> crate::RunResult {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        emu.run_cycles(clocks, &mut fb, audio)
    }

    /// The clock of the first DIV-APU edge after reset: counter bit 12
    /// falls when the counter reaches a multiple of 0x2000.
    fn first_div_apu_edge() -> u64 {
        let c0 = Timer::post_boot().sc(0);
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

    #[test]
    fn div_apu_follows_counter_bit_13_after_a_switch_to_double_speed() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        let t = &ctx.sys.timer;
        assert_eq!(t.div_apu_bit(), 13);
        let want = t.clock_at(((t.sc(ctx.clock()) >> 14) + 1) << 14);
        assert_eq!(ctx.sys.sched.deadline(EventKind::DivApu), want);
    }

    #[test]
    fn power_on_with_bit_12_set_skips_the_first_edge() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        write(emu, 0xFF26, 0x00);
        let _ = run_audio(emu, first + 0x1100, &mut []);
        write(emu, 0xFF26, 0x80);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, true));
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), first + 0x2000);
        let _ = run_audio(emu, 0x1000, &mut []);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, false));
        let _ = run_audio(emu, 0x2000, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 2);
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
            // No channels yet: silence.
            assert!(
                audio[..r.audio_samples]
                    .iter()
                    .all(|s| *s == StereoSample::default())
            );
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
            // An external-bus write is dropped and clears the OAM byte
            // just transferred (byte 5; CGB C).
            ctx.write(0x2000, 0x05); // M-cycle 7
            assert_eq!(ctx.mem.oam[5], 0);
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
            ctx.idle_halted();
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
}
