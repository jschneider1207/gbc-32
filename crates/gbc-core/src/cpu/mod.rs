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
    pub(crate) fn post_boot(rom: &[u8], cgb_mode: bool) -> Self {
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
            let nintendo =
                rom[0x14B] == 0x01 || (rom[0x14B] == 0x33 && rom[0x144..0x146] == *b"01");
            r.b = if nintendo {
                rom[0x134..0x144]
                    .iter()
                    .fold(0u8, |s, &x| s.wrapping_add(x))
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
    /// The IE register (FFFF). Free: no M-cycle.
    fn interrupt_enable(&self) -> u8;
    /// The IF register (FF0F), low 5 bits significant. Free: no M-cycle.
    fn interrupt_flags(&self) -> u8;
    /// Writes IF (used to acknowledge a dispatched interrupt). Free.
    fn set_interrupt_flags(&mut self, value: u8);
    /// The CPU raised an event (`Events::LD_B_B`, `Events::CPU_LOCKED`).
    fn event(&mut self, event: Events) {
        let _ = event;
    }
    /// STOP was executed. Returns true if it was a CGB speed switch, after
    /// which the CPU keeps running; otherwise the CPU stops.
    fn stop(&mut self) -> bool {
        false
    }
    /// Whether a stopped CPU wakes up (a button is pressed).
    fn stop_wakeup(&self) -> bool {
        false
    }
    /// Whether [`Cpu::run`] returns, checked after every step. Free. The
    /// default (true) makes `run` a single step.
    fn run_done(&self) -> bool {
        true
    }
}

/// What the CPU does on its next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Executes instructions.
    Running,
    /// HALT: idles until an interrupt is pending (IE & IF != 0).
    Halted,
    /// The HALT bug: the next opcode fetch doesn't increment PC, so the
    /// byte after HALT is read twice (Pan Docs: halt, "Halt bug").
    HaltBug,
    /// STOP (stub until M5): idles until a button is pressed.
    Stopped,
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
    pub(crate) fn reset(&mut self, rom: &[u8], cgb_mode: bool) {
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

    /// Executes one instruction, or dispatches one interrupt, or idles for
    /// one M-cycle while halted, stopped, locked up or powered off.
    #[inline]
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        self.step_inline(bus);
    }

    #[inline(always)]
    fn step_inline<B: Bus>(&mut self, bus: &mut B) {
        let mut pc_step = 1;
        if self.mode != Mode::Running {
            match self.step_not_running(bus) {
                Some(step) => pc_step = step,
                None => return,
            }
        }
        if self.ime && bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK != 0 {
            self.dispatch(bus);
            return;
        }
        // EI takes effect after the instruction that follows it. The check
        // above used the old IME, so that instruction always runs.
        if self.ei_pending {
            self.ei_pending = false;
            self.ime = true;
        }
        let pc = self.regs.pc;
        let op = bus.read(pc);
        self.regs.pc = pc.wrapping_add(pc_step);
        // The only call site, so the 256-arm dispatch is inlined once.
        self.execute(op, bus);
    }

    /// The step for every mode except `Running`. Returns `None` if the
    /// step is done (one idle M-cycle), or `Some(pc_step)` if the CPU is
    /// running again and the caller should go on with a normal step whose
    /// opcode fetch advances PC by `pc_step`.
    #[inline(never)]
    fn step_not_running<B: Bus>(&mut self, bus: &mut B) -> Option<u16> {
        match self.mode {
            Mode::Running => Some(1),
            Mode::Halted => {
                // Any pending interrupt ends HALT, even with IME off (then
                // execution just continues after HALT). TODO(M2): the
                // exact wake-up latency (GBCTR: HALT; SameBoy).
                if bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK != 0 {
                    self.mode = Mode::Running;
                    Some(1)
                } else {
                    bus.idle();
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
                bus.idle();
                if bus.stop_wakeup() {
                    self.mode = Mode::Running;
                }
                None
            }
            Mode::Locked | Mode::Off => {
                bus.idle();
                None
            }
        }
    }

    /// Interrupt dispatch: 5 M-cycles (two internal, push PC high, push PC
    /// low, jump). IE is sampled after the high byte is pushed and IF after
    /// the low byte, so a push that overwrites IE (SP = 0) or a change of
    /// IF in between can cancel the dispatch; PC is then 0000 (Mooneye
    /// `ie_push`; SameBoy `GB_cpu_run`).
    fn dispatch<B: Bus>(&mut self, bus: &mut B) {
        self.ime = false;
        self.ei_pending = false;
        bus.idle();
        bus.idle();
        let pc = self.regs.pc;
        let sp = self.regs.sp.wrapping_sub(1);
        bus.write(sp, (pc >> 8) as u8);
        let ie = bus.interrupt_enable();
        let sp = sp.wrapping_sub(1);
        bus.write(sp, pc as u8);
        self.regs.sp = sp;
        let pending = ie & bus.interrupt_flags() & INT_MASK;
        if pending == 0 {
            self.regs.pc = 0x0000;
        } else {
            let bit = pending.trailing_zeros();
            bus.set_interrupt_flags(bus.interrupt_flags() & !(1 << bit));
            self.regs.pc = 0x0040 + 8 * bit as u16;
        }
        bus.idle();
    }

    /// HALT (opcode 76), after its fetch.
    fn halt<B: Bus>(&mut self, bus: &mut B) {
        if bus.interrupt_enable() & bus.interrupt_flags() & INT_MASK == 0 {
            self.mode = Mode::Halted;
        } else if !self.ime {
            self.mode = Mode::HaltBug;
        }
        // IME on and an interrupt pending: no halt; it is dispatched next.
        // TODO(M2): `EI; HALT` with an interrupt pending returns to the HALT
        // itself (Pan Docs: halt bug).
    }

    /// STOP (opcode 10). With a CGB speed switch armed (KEY1 bit 0), the
    /// bus switches speed and execution continues. Otherwise a stub until
    /// M5: the CPU idles until a button is pressed.
    fn stop<B: Bus>(&mut self, bus: &mut B) {
        if !bus.stop() {
            self.mode = Mode::Stopped;
        }
    }

    /// An invalid opcode (D3 DB DD E3 E4 EB EC ED F4 FC FD) hangs the CPU
    /// for good (Pan Docs: CPU Instruction Set; GBCTR).
    fn lock<B: Bus>(&mut self, bus: &mut B) {
        self.mode = Mode::Locked;
        bus.event(Events::CPU_LOCKED);
    }
}
