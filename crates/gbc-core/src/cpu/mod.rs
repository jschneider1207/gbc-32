//! SM83 CPU: registers, interrupt dispatch, HALT/STOP and the instruction
//! set (in `exec`). Design: ADR 0008.
//!
//! The CPU is generic over a [`Bus`], which it drives one M-cycle at a time:
//! every [`Bus::read`], [`Bus::write`] and [`Bus::idle`] call is exactly one
//! M-cycle, in the order the hardware performs them (Gekkio's GBCTR,
//! "Instruction set"). The emulator's bus advances the clock on each call
//! (the timer, serial port and PPU follow from it, ADR 0011), so the CPU
//! is the master clock (ADR 0002). The
//! bus is a type parameter, not `dyn`, so the emulator's instance is
//! monomorphized and inlined; the SM83 single-step tests (`tools/sm83-tests`)
//! use a flat 64 KiB test bus through the same trait.

mod exec;

use crate::Events;
use crate::rom::Rom;

/// A snapshot of the CPU registers ([`Emulator::registers`](crate::Emulator::registers)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
}

impl Registers {
    const fn zero() -> Self {
        Self {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            sp: 0,
            pc: 0,
        }
    }

    /// Register state after the CGB boot ROM hands over (Pan Docs: Power Up
    /// Sequence, "CPU registers", CGB and "CGB (DMG mode)" columns).
    ///
    /// `rom` holds at least the cartridge header. CGB mode (header 0143 bit
    /// 7 set): A=11 F=80 B=00 C=00 D=FF E=56 H=00 L=0D. DMG compatibility
    /// mode: A=11 F=80 C=00 D=00 E=08; B is the title checksum (sum of
    /// 0134-0143) if the licensee is Nintendo (old licensee 01, or 33 with
    /// new licensee "01"), else 00; HL is 991A if B is 43 or 58, else 007C
    /// (the boot ROM's palette lookup leaves these behind).
    pub(crate) fn post_boot(rom: Rom<'_>, cgb_mode: bool) -> Self {
        let mut r = Self {
            a: 0x11,
            f: 0x80,
            b: 0x00,
            c: 0x00,
            d: 0xFF,
            e: 0x56,
            h: 0x00,
            l: 0x0D,
            sp: 0xFFFE,
            pc: 0x0100,
        };
        if !cgb_mode {
            // Byte reads through `Rom::get` (ADR 0027); the caller checked
            // that the header is there.
            let at = |i| rom.get(i).unwrap_or(0);
            let nintendo =
                at(0x14B) == 0x01 || (at(0x14B) == 0x33 && at(0x144) == b'0' && at(0x145) == b'1');
            r.b = if nintendo {
                (0x134..0x144).fold(0u8, |s, i| s.wrapping_add(at(i)))
            } else {
                0
            };
            r.d = 0x00;
            r.e = 0x08;
            (r.h, r.l) = if matches!(r.b, 0x43 | 0x58) {
                (0x99, 0x1A)
            } else {
                (0x00, 0x7C)
            };
        }
        r
    }
}

/// Flag bits in F.
pub(crate) const FZ: u8 = 0x80;
pub(crate) const FN: u8 = 0x40;
pub(crate) const FH: u8 = 0x20;
pub(crate) const FC: u8 = 0x10;

/// The interrupt sources in IE/IF (bit 0 = VBlank ... bit 4 = joypad).
pub(crate) const INT_MASK: u8 = 0x1F;

/// What the CPU is connected to. Each of `read`, `write` and `idle` is one
/// M-cycle; the emulator's implementation advances the rest of the machine
/// by one M-cycle per call.
///
/// Public only so that instruction-level tests outside the core
/// (`tools/sm83-tests`) can drive the CPU with their own bus; front ends
/// don't need it.
pub trait Bus {
    /// One M-cycle that reads `addr`.
    fn read(&mut self, addr: u16) -> u8;
    /// One M-cycle that writes `value` to `addr`.
    fn write(&mut self, addr: u16, value: u8);
    /// One M-cycle without a memory access (an internal CPU cycle).
    fn idle(&mut self);
    /// The opcode fetch M-cycle: reads `addr`, or with `latched` (an
    /// opcode fetched earlier, [`Cpu::latch_fetch`]) only idles and
    /// returns it. One call site with the M-cycle shared by both cases,
    /// so the opcode dispatch has one entry (perf: a second entry for the
    /// latched opcode cost `synth:worst-case` 1%: LLVM duplicated the
    /// dispatch jump and lost addressing modes all through `Cpu::run`).
    #[inline(always)]
    fn fetch(&mut self, addr: u16, latched: Option<u8>) -> u8 {
        match latched {
            Some(op) => {
                self.idle();
                op
            }
            None => self.read(addr),
        }
    }
    /// Idles a halted, stopped, locked-up or powered-off CPU: at least one
    /// M-cycle, and at most up to the first M-cycle boundary at which
    /// [`Bus::interrupt_flags`], [`Bus::interrupt_enable`], the result of
    /// [`Bus::stop_halt`] or [`Bus::run_done`] can change. The default
    /// is one M-cycle ([`Bus::idle`]); the emulator skips straight to its
    /// next deadline (ADR 0011).
    fn idle_halted(&mut self) {
        self.idle();
    }
    /// The CPU has just halted: HALT, with nothing pending, after its
    /// fetch M-cycle. Free. The emulator pauses OAM DMA here (SameBoy
    /// `GB_dma_run` returns while halted).
    fn halt_begin(&mut self) {}
    /// A halted CPU wakes up: called at the start of the wake-up M-cycle
    /// (the CGB +1 idle before the next fetch or the dispatch). Free.
    fn halt_end(&mut self) {}
    /// At the end of a wake-up M-cycle (after [`Bus::halt_end`] and the
    /// idle): whether something runs before the next opcode fetch or
    /// dispatch, so the step ends there. The emulator: a VRAM DMA block
    /// the wake-up starts (ADR 0017, amendment). Free.
    fn wake_stalls(&mut self) -> bool {
        false
    }
    /// The IE register (FFFF). Free: no M-cycle.
    fn interrupt_enable(&self) -> u8;
    /// The IF register (FF0F), low 5 bits significant. Free: no M-cycle.
    fn interrupt_flags(&self) -> u8;
    /// Writes IF (used to acknowledge a dispatched interrupt). Free.
    fn set_interrupt_flags(&mut self, value: u8);
    /// The last M-cycle of an interrupt dispatch: writes `value` (PC's low
    /// byte) to `addr` at its start, then samples IF (`ie & IF`, the
    /// interrupts still pending) and clears the lowest of them in IF,
    /// which SameBoy does 2 T-cycles in. Returns the pending bits.
    /// Default: the clear after the M-cycle, so a source firing again in
    /// it is absorbed (at M-cycle granularity, ADR 0008).
    fn push_acknowledge(&mut self, addr: u16, value: u8, ie: u8) -> u8 {
        self.write(addr, value);
        let pending = ie & self.interrupt_flags() & INT_MASK;
        self.set_interrupt_flags(self.interrupt_flags() & !(pending.isolate_lowest_one()));
        pending
    }
    /// The CPU raised an event (`Events::LD_B_B`, `Events::CPU_LOCKED`).
    fn event(&mut self, event: Events) {
        let _ = event;
    }
    /// STOP was executed, at the end of its fetch M-cycle. Free: the
    /// emulator resets DIV here unless the result is [`StopKind::Joypad`].
    fn stop_begin(&mut self) -> StopKind {
        StopKind::Stop
    }
    /// The CGB speed switch of a [`StopKind::Switch`], after STOP's second
    /// byte (if read). `ime`: IME at the STOP; `pending`: IE & IF was
    /// nonzero at it. Returns true if the CPU waits: without `pending`, it
    /// halts until [`Bus::speed_switch_over`] or an interrupt; with it,
    /// it idles one M-cycle. Free.
    fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        let _ = (ime, pending);
        false
    }
    /// One step of a CPU in the speed-switch halt: if an interrupt is
    /// pending (IE & IF) or the halt has run its length, the halt ends (in
    /// place of [`Bus::halt_end`]) and this returns true; otherwise it
    /// idles like [`Bus::idle_halted`], never past the halt's end, and
    /// returns false. The default halt has no length: it ends at once.
    /// One cold call, so the HALT path of `step_not_running` stays as it was.
    fn speed_switch_halt(&mut self) -> bool {
        self.halt_end();
        true
    }
    /// After HALT halts ([`Bus::halt_begin`]) or a speed switch that halts
    /// ([`Bus::speed_switch`]): whether the byte after HALT, or STOP's
    /// second byte, latched then, runs as the next opcode after the
    /// wake-up ([`Mode::Prefetched`]). The emulator: a VRAM DMA block
    /// requested in HALT's M-cycle or STOP's first (gambatte
    /// `Memory::halt`/`stop`, `prefetched`). Free.
    fn hdma_refetch(&mut self) -> bool {
        false
    }
    /// The byte an opcode fetch at `addr` would read now, without the
    /// M-cycle (the emulator latches an opcode with it before a VRAM DMA
    /// block, [`Cpu::latch_fetch`]). Free. Default: FF.
    fn fetch_peek(&mut self, addr: u16) -> u8 {
        let _ = addr;
        0xFF
    }
    /// STOP mode was entered ([`StopKind::Stop`]), after STOP's second
    /// byte (if read). Free. The emulator pauses OAM DMA and HDMA here;
    /// it stopped the timer in [`Bus::stop_begin`].
    fn stop_enter(&mut self) {}
    /// One step of a CPU in STOP mode. If a selected P1 line is low, STOP
    /// mode ends: this returns true without an M-cycle (the emulator
    /// starts the timer, OAM DMA and HDMA again), and the CPU runs again
    /// after two wake-up M-cycles. Otherwise it idles like
    /// [`Bus::idle_halted`] and returns false. Interrupts don't end STOP
    /// mode. The default never wakes: one idle M-cycle. One cold call, as
    /// [`Bus::speed_switch_halt`].
    fn stop_halt(&mut self) -> bool {
        self.idle();
        false
    }
    /// Whether [`Cpu::run`] returns, checked after every step. Free. The
    /// default (true) makes `run` a single step.
    fn run_done(&self) -> bool {
        true
    }
}

/// What STOP does, decided at its first M-cycle ([`Bus::stop_begin`];
/// SameBoy `stop()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopKind {
    /// STOP mode: DIV reset, the CPU and the timer stop until a selected
    /// P1 line goes low ([`Bus::stop_halt`]).
    Stop,
    /// A CGB speed switch (KEY1 armed, no button held): DIV reset.
    Switch,
    /// A button held on a selected P1 line: no DIV reset, no STOP mode
    /// and no speed switch; with nothing pending the CPU halts.
    Joypad,
}

/// `step_not_running`: run [`Cpu::latched`] without a fetch.
const LATCHED: u16 = 0x100;

/// What the CPU does on its next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Executes instructions.
    Running,
    /// HALT: idles until an interrupt is pending (IE & IF != 0).
    Halted,
    /// The halt after a CGB speed switch: as `Halted`, but it also ends
    /// when its countdown runs out ([`Bus::speed_switch_halt`]; SameBoy
    /// `speed_switch_halt_countdown`).
    SpeedSwitch,
    /// The HALT bug: the next opcode fetch doesn't increment PC, so the
    /// byte after HALT is read twice (Pan Docs: halt, "Halt bug").
    HaltBug,
    /// STOP mode: idles until a selected P1 line goes low
    /// ([`Bus::stop_halt`]); interrupts don't end it.
    Stopped,
    /// The opcode was fetched before a VRAM DMA block ([`Cpu::latch_fetch`],
    /// PC already past it): the step runs it after one idle M-cycle (the
    /// fetch's), unless an interrupt dispatch is due, which undoes the
    /// fetch.
    Latched,
    /// HALT or a speed-switch STOP met a VRAM DMA block request: the byte
    /// after HALT, or STOP's second byte, latched then, runs after the
    /// wake-up (and the block) as the next opcode, before any dispatch,
    /// with PC unchanged (gambatte `prefetched_`).
    Prefetched,
    /// Locked up after an invalid opcode; only a reset recovers.
    Locked,
    /// No cartridge loaded: the console is off and time just passes.
    Off,
}

/// The SM83 CPU state.
pub struct Cpu {
    pub(crate) regs: Registers,
    /// Interrupt master enable.
    ime: bool,
    /// EI was just executed: IME turns on after the next instruction
    /// (Pan Docs: EI; SameBoy `ime_toggle`).
    ei_pending: bool,
    mode: Mode,
    /// The opcode of [`Mode::Latched`] and [`Mode::Prefetched`].
    latched: u8,
    /// A HALT or speed-switch halt wakes into [`Mode::Prefetched`].
    prefetched: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A running CPU with all registers zero, IME off.
    pub const fn new() -> Self {
        Self {
            regs: Registers::zero(),
            ime: false,
            ei_pending: false,
            latched: 0,
            prefetched: false,
            mode: Mode::Running,
        }
    }

    /// No cartridge: the CPU doesn't execute anything until `reset`.
    pub(crate) const fn powered_off() -> Self {
        Self {
            mode: Mode::Off,
            ..Self::new()
        }
    }

    /// Power-on state after the boot ROM (see [`Registers::post_boot`]).
    pub(crate) fn reset(&mut self, rom: Rom<'_>, cgb_mode: bool) {
        *self = Self::new();
        self.regs = Registers::post_boot(rom, cgb_mode);
    }

    pub fn registers(&self) -> Registers {
        self.regs
    }

    pub fn set_registers(&mut self, regs: Registers) {
        self.regs = regs;
    }

    /// Interrupt master enable.
    pub fn ime(&self) -> bool {
        self.ime
    }

    pub fn set_ime(&mut self, ime: bool) {
        self.ime = ime;
    }

    /// Whether IME turns on after the next instruction (EI was just
    /// executed).
    pub fn ei_pending(&self) -> bool {
        self.ei_pending
    }

    pub fn set_ei_pending(&mut self, pending: bool) {
        self.ei_pending = pending;
    }

    /// Whether the CPU is in HALT.
    pub fn halted(&self) -> bool {
        self.mode == Mode::Halted
    }

    /// Whether the CPU is in STOP mode.
    #[cfg(test)]
    pub(crate) fn stopped(&self) -> bool {
        self.mode == Mode::Stopped
    }

    /// Whether the CPU runs (not halted, stopped, locked up or off): a
    /// pending VRAM DMA step runs before its next step, also before an
    /// interrupt dispatch due there (ADR 0017, trigger amendment).
    pub(crate) fn running(&self) -> bool {
        matches!(
            self.mode,
            Mode::Running | Mode::HaltBug | Mode::Latched | Mode::Prefetched
        )
    }

    /// Whether the CPU locked up on an invalid opcode.
    pub fn locked(&self) -> bool {
        self.mode == Mode::Locked
    }

    /// Executes steps (see [`Cpu::step`]) until [`Bus::run_done`] is true
    /// after one; always at least one step.
    ///
    /// The emulator runs whole run calls through this, so the instruction
    /// loop and the opcode dispatch are one function: registers are saved
    /// and restored once per call, not once per instruction (ADR 0008,
    /// amendments). Not `#[inline]`: one instance per bus type.
    pub fn run<B: Bus>(&mut self, bus: &mut B) {
        loop {
            self.step_inline(bus);
            if bus.run_done() {
                break;
            }
        }
    }

    /// Executes one instruction, or dispatches one interrupt, or idles
    /// ([`Bus::idle_halted`]) while halted, stopped, locked up or powered
    /// off.
    #[inline]
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        self.step_inline(bus);
    }

    #[inline(always)]
    fn step_inline<B: Bus>(&mut self, bus: &mut B) {
        let mut pc_step = 1;
        let mut latched = None;
        if self.mode != Mode::Running {
            match self.step_not_running(bus) {
                // A latched opcode: its M-cycle without a read, PC already
                // past it; `step_not_running` did the interrupt check.
                Some(LATCHED) => {
                    latched = Some(self.latched);
                    pc_step = 0;
                }
                Some(step) => pc_step = step,
                None => return,
            }
        }
        if latched.is_none() {
            if self.ime && bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK != 0 {
                self.dispatch(bus);
                return;
            }
            // EI takes effect after the instruction that follows it. The
            // check above used the old IME, so that instruction always
            // runs.
            if self.ei_pending {
                self.ei_pending = false;
                self.ime = true;
            }
        }
        let pc = self.regs.pc;
        let op = bus.fetch(pc, latched);
        self.regs.pc = pc.wrapping_add(pc_step);
        // The only call site, so the 256-arm dispatch is inlined once.
        self.execute(op, bus);
    }

    /// A VRAM DMA block is about to run where the CPU would fetch its next
    /// opcode: SameBoy (`GB_cpu_run`) and gambatte (`Interrupter::prefetch`
    /// in the DMA event) fetch it before the block, so a block that
    /// overwrites it, or a PPU mode that starts blocking VRAM during the
    /// stall, doesn't change it (gambatte CGB C `dma/hdma_pc_7ffe`,
    /// `late_gdma_pc_7ffe_{1,2}`). Only from a plain running state; the
    /// fetch's M-cycle stays after the stall (ADR 0017).
    #[cold]
    pub(crate) fn latch_fetch<B: Bus>(&mut self, bus: &mut B) {
        if self.mode == Mode::Running {
            let pc = self.regs.pc;
            self.latched = bus.fetch_peek(pc);
            self.regs.pc = pc.wrapping_add(1);
            self.mode = Mode::Latched;
        }
    }

    /// The step for every mode except `Running`. Returns `None` if the
    /// step is done (an idle, [`Bus::idle_halted`]), or `Some(pc_step)` if the CPU is
    /// running again and the caller should go on with a normal step whose
    /// opcode fetch advances PC by `pc_step`.
    #[inline(never)]
    fn step_not_running<B: Bus>(&mut self, bus: &mut B) -> Option<u16> {
        match self.mode {
            Mode::Running => Some(1),
            Mode::Halted => {
                // Any pending interrupt ends HALT, even with IME off (then
                // execution just continues after HALT). On CGB the wake-up
                // costs one more idle M-cycle before the dispatch (IME on)
                // or the next fetch (IME off): SameBoy `GB_cpu_run` samples
                // IE & IF, then advances 4 clocks while halted, then takes
                // either branch. See docs/reference/halt.md.
                if bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK != 0 {
                    bus.halt_end();
                    bus.idle();
                    let prefetched = self.wake_prefetched();
                    // A VRAM DMA block the wake-up started runs before the
                    // next instruction: SameBoy fetches its opcode, runs
                    // `GB_hdma_run`, then executes it (ADR 0017, amendment).
                    if bus.wake_stalls() || prefetched {
                        return None;
                    }
                    Some(1)
                } else {
                    bus.idle_halted();
                    None
                }
            }
            // The speed-switch halt is a HALT (SameBoy `halted`) that its
            // countdown also ends, with the same wake-up M-cycle, then the
            // fetch, and the same check for a VRAM DMA block before it.
            Mode::SpeedSwitch => {
                if bus.speed_switch_halt() {
                    bus.idle();
                    let prefetched = self.wake_prefetched();
                    if bus.wake_stalls() || prefetched {
                        return None;
                    }
                    Some(1)
                } else {
                    None
                }
            }
            Mode::HaltBug => {
                // HALT with IME off and an interrupt already pending: the CPU
                // doesn't halt, and the next fetch fails to increment PC
                // (Pan Docs: halt, "Halt bug"). The normal step's interrupt
                // and EI checks do nothing here: IME was off at the HALT,
                // and EI's delay had already been applied before the HALT
                // was fetched, so only a further EI can change IME.
                debug_assert!(!self.ime && !self.ei_pending);
                self.mode = Mode::Running;
                Some(0)
            }
            Mode::Stopped => {
                // Only a selected P1 line going low ends STOP mode (Pan
                // Docs: "Using the STOP instruction"); a pending interrupt
                // does not. SameBoy `GB_cpu_run`, the `stopped` branch:
                // the line is sampled every M-cycle; once low,
                // `leave_stop_mode`, 8 more T-cycles (two M-cycles at
                // either speed), then the CPU runs: the instruction after
                // STOP, or a dispatch (the press requested the joypad
                // interrupt). ADR 0020, STOP mode amendment.
                if !bus.stop_halt() {
                    return None;
                }
                bus.idle();
                bus.idle();
                self.mode = Mode::Running;
                // A VRAM DMA block the wake-up started runs first, as
                // after HALT.
                if bus.wake_stalls() {
                    return None;
                }
                Some(1)
            }
            Mode::Latched => {
                self.mode = Mode::Running;
                if self.ime && bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK != 0 {
                    // gambatte `Interrupter::interrupt`: "undo prefetch".
                    self.regs.pc = self.regs.pc.wrapping_sub(1);
                    return Some(1);
                }
                if self.ei_pending {
                    self.ei_pending = false;
                    self.ime = true;
                }
                // The M-cycle: `Bus::fetch` with the latched opcode.
                Some(LATCHED)
            }
            Mode::Prefetched => {
                self.mode = Mode::Running;
                Some(LATCHED)
            }
            Mode::Locked | Mode::Off => {
                bus.idle_halted();
                None
            }
        }
    }

    /// Interrupt dispatch: 5 M-cycles (three internal, push PC high, push
    /// PC low; the jump overlaps the next fetch). SameBoy `GB_cpu_run`
    /// (read, OAM-bug cycle, no access, write, write) and gambatte
    /// `Interrupter::interrupt` (writes at +12 and +16 clocks) agree; an
    /// OAM DMA pins the write M-cycles on CGB C (gambatte cgb04c
    /// `oamdma/oamdma_src0000_busyint0002`). IE is sampled after the high
    /// byte is pushed and IF after the low byte (a low byte pushed onto IF
    /// itself: IF before it, [`Bus::push_acknowledge`]), so a push that
    /// overwrites IE (SP = 0) or a change of IF in between can cancel the
    /// dispatch;
    /// PC is then 0000 (Mooneye `ie_push`; SameBoy `GB_cpu_run`). The
    /// chosen IF bit is cleared only in the last M-cycle (SameBoy: 2
    /// T-cycles into it), so the same source firing again by then is
    /// absorbed (gambatte `tima/tc00_irq_late_retrigger_2`;
    /// [`Bus::push_acknowledge`]).
    fn dispatch<B: Bus>(&mut self, bus: &mut B) {
        self.ime = false;
        self.ei_pending = false;
        bus.idle();
        bus.idle();
        bus.idle();
        let pc = self.regs.pc;
        let sp = self.regs.sp.wrapping_sub(1);
        bus.write(sp, (pc >> 8) as u8);
        let ie = bus.interrupt_enable();
        let sp = sp.wrapping_sub(1);
        self.regs.sp = sp;
        let pending = bus.push_acknowledge(sp, pc as u8, ie);
        self.regs.pc = if pending == 0 {
            0x0000
        } else {
            0x0040 + 8 * pending.trailing_zeros() as u16
        };
    }

    /// HALT (opcode 76), after its fetch.
    fn halt<B: Bus>(&mut self, bus: &mut B) {
        if bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK == 0 {
            self.mode = Mode::Halted;
            bus.halt_begin();
            // A VRAM DMA block requested in HALT's M-cycle: the byte after
            // HALT, read now (gambatte `halt`: `opcode_ = read(pc)`,
            // `prefetched_ = mem_.halt()`), runs after the wake-up with PC
            // unchanged, so it runs twice, as in the HALT bug (CGB C
            // `dma/hdma_late_m3halt_m2unhalt_inc_*_2`).
            if bus.hdma_refetch() {
                self.latched = bus.fetch_peek(self.regs.pc);
                self.prefetched = true;
            }
        } else if !self.ime {
            self.mode = Mode::HaltBug;
        } else {
            // IME on and an interrupt pending (this includes `EI; HALT`:
            // EI's delay set IME before HALT's fetch): no halt, the
            // interrupt is dispatched next, and PC is rewound so the pushed
            // return address is the HALT itself, which runs again after
            // RETI (SameBoy `halt()`: `gb->pc--`; Pan Docs: halt bug;
            // SameSuite `ei_delay_halt`).
            self.regs.pc = self.regs.pc.wrapping_sub(1);
        }
    }

    /// The wake-up from HALT or the speed-switch halt, after its M-cycle:
    /// into [`Mode::Prefetched`] (the step ends; the next one runs the
    /// latched opcode, after the VRAM DMA block) or [`Mode::Running`].
    #[inline(always)]
    fn wake_prefetched(&mut self) -> bool {
        self.mode = Mode::Running;
        if self.prefetched {
            self.prefetched = false;
            self.mode = Mode::Prefetched;
            return true;
        }
        false
    }

    /// STOP (opcode 10), after SameBoy `stop()` (Pan Docs: "Using the
    /// STOP instruction"). With nothing pending (IE & IF = 0) STOP is two
    /// bytes: its second byte is read (one M-cycle) before the speed switch
    /// or the halt. With an interrupt pending it is one byte, and the
    /// second byte runs as the next opcode.
    ///
    /// - A button held on a selected P1 line: no DIV reset; with nothing
    ///   pending the CPU halts (a plain HALT), else it just continues.
    /// - KEY1 armed (CGB): the speed switch, then, with nothing pending,
    ///   the speed-switch halt ([`Mode::SpeedSwitch`]).
    /// - Otherwise STOP mode ([`Mode::Stopped`]), entered after the second
    ///   byte (if read), also with an interrupt pending. SingleStepTests
    ///   expects STOP to be one byte even with nothing pending; its
    ///   harness exempts PC and the bus cycles of those cases (ADR 0020).
    ///
    /// Cold and out of line: it keeps `Cpu::run` small.
    #[cold]
    #[inline(never)]
    fn stop<B: Bus>(&mut self, bus: &mut B) {
        let pending = bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK != 0;
        let kind = bus.stop_begin();
        if !pending {
            let pc = self.regs.pc;
            self.latched = bus.read(pc);
            self.regs.pc = pc.wrapping_add(1);
        }
        match kind {
            StopKind::Stop => {
                self.mode = Mode::Stopped;
                bus.stop_enter();
            }
            StopKind::Switch => {
                if bus.speed_switch(self.ime, pending) {
                    if pending {
                        bus.idle();
                    } else {
                        self.mode = Mode::SpeedSwitch;
                        // STOP's second byte, read above, runs after the
                        // halt (gambatte `stop`: `prefetched_`).
                        self.prefetched = bus.hdma_refetch();
                    }
                }
            }
            StopKind::Joypad => {
                if !pending {
                    self.mode = Mode::Halted;
                    bus.halt_begin();
                }
            }
        }
    }

    /// An invalid opcode (D3 DB DD E3 E4 EB EC ED F4 FC FD) hangs the CPU
    /// for good (Pan Docs: CPU Instruction Set; GBCTR).
    fn lock<B: Bus>(&mut self, bus: &mut B) {
        self.mode = Mode::Locked;
        bus.event(Events::CPU_LOCKED);
    }
}
