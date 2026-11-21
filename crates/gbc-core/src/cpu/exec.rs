//! SM83 instruction execution.
//!
//! Dispatch is a 256-arm `match` (a jump table) whose arms each call a
//! function monomorphized for one constant opcode (`op::<OP>`,
//! `cb::<OP>`). The bodies decode register indices and conditions from
//! `OP` with ordinary arithmetic; because `OP` is a constant, the compiler
//! folds the decoding away and every arm is straight-line code for its
//! opcode. ADR 0008.
//!
//! M-cycle order follows Gekkio's GBCTR ("Instruction set") and is checked
//! cycle by cycle by the SM83 single-step tests (`tools/sm83-tests`).
//! Register index order (bits 0-2 / 3-5 of the opcode): B C D E H L (HL) A.

use super::{Bus, Cpu, FC, FH, FN, FZ};
use crate::Events;

/// Expands to an exhaustive `match` over every `u8` that calls
/// `$f::<N, _>` for opcode `N`.
macro_rules! dispatch {
    ($self:ident, $bus:ident, $op:expr, $f:ident; $($n:literal)*) => {
        match $op {
            $($n => $self.$f::<$n, _>($bus),)*
        }
    };
}

macro_rules! dispatch_all {
    ($self:ident, $bus:ident, $op:expr, $f:ident) => {
        dispatch!($self, $bus, $op, $f;
            0x00 0x01 0x02 0x03 0x04 0x05 0x06 0x07 0x08 0x09 0x0A 0x0B 0x0C 0x0D 0x0E 0x0F
            0x10 0x11 0x12 0x13 0x14 0x15 0x16 0x17 0x18 0x19 0x1A 0x1B 0x1C 0x1D 0x1E 0x1F
            0x20 0x21 0x22 0x23 0x24 0x25 0x26 0x27 0x28 0x29 0x2A 0x2B 0x2C 0x2D 0x2E 0x2F
            0x30 0x31 0x32 0x33 0x34 0x35 0x36 0x37 0x38 0x39 0x3A 0x3B 0x3C 0x3D 0x3E 0x3F
            0x40 0x41 0x42 0x43 0x44 0x45 0x46 0x47 0x48 0x49 0x4A 0x4B 0x4C 0x4D 0x4E 0x4F
            0x50 0x51 0x52 0x53 0x54 0x55 0x56 0x57 0x58 0x59 0x5A 0x5B 0x5C 0x5D 0x5E 0x5F
            0x60 0x61 0x62 0x63 0x64 0x65 0x66 0x67 0x68 0x69 0x6A 0x6B 0x6C 0x6D 0x6E 0x6F
            0x70 0x71 0x72 0x73 0x74 0x75 0x76 0x77 0x78 0x79 0x7A 0x7B 0x7C 0x7D 0x7E 0x7F
            0x80 0x81 0x82 0x83 0x84 0x85 0x86 0x87 0x88 0x89 0x8A 0x8B 0x8C 0x8D 0x8E 0x8F
            0x90 0x91 0x92 0x93 0x94 0x95 0x96 0x97 0x98 0x99 0x9A 0x9B 0x9C 0x9D 0x9E 0x9F
            0xA0 0xA1 0xA2 0xA3 0xA4 0xA5 0xA6 0xA7 0xA8 0xA9 0xAA 0xAB 0xAC 0xAD 0xAE 0xAF
            0xB0 0xB1 0xB2 0xB3 0xB4 0xB5 0xB6 0xB7 0xB8 0xB9 0xBA 0xBB 0xBC 0xBD 0xBE 0xBF
            0xC0 0xC1 0xC2 0xC3 0xC4 0xC5 0xC6 0xC7 0xC8 0xC9 0xCA 0xCB 0xCC 0xCD 0xCE 0xCF
            0xD0 0xD1 0xD2 0xD3 0xD4 0xD5 0xD6 0xD7 0xD8 0xD9 0xDA 0xDB 0xDC 0xDD 0xDE 0xDF
            0xE0 0xE1 0xE2 0xE3 0xE4 0xE5 0xE6 0xE7 0xE8 0xE9 0xEA 0xEB 0xEC 0xED 0xEE 0xEF
            0xF0 0xF1 0xF2 0xF3 0xF4 0xF5 0xF6 0xF7 0xF8 0xF9 0xFA 0xFB 0xFC 0xFD 0xFE 0xFF
        )
    };
}

impl Cpu {
    /// Executes the instruction whose opcode was just fetched (PC already
    /// points past it). Always inlined into its one caller, the step in
    /// [`Cpu::run`]'s loop (ADR 0008, amendments).
    #[inline(always)]
    pub(super) fn execute<B: Bus>(&mut self, op: u8, bus: &mut B) {
        dispatch_all!(self, bus, op, op)
    }

    /// The CB-prefixed instructions, after the CB byte.
    #[inline(never)]
    fn execute_cb<B: Bus>(&mut self, bus: &mut B) {
        let op = self.imm8(bus);
        dispatch_all!(self, bus, op, cb)
    }

    // --- Register helpers. ---

    #[inline(always)]
    fn bc(&self) -> u16 {
        u16::from_be_bytes([self.regs.b, self.regs.c])
    }
    #[inline(always)]
    fn de(&self) -> u16 {
        u16::from_be_bytes([self.regs.d, self.regs.e])
    }
    #[inline(always)]
    fn hl(&self) -> u16 {
        u16::from_be_bytes([self.regs.h, self.regs.l])
    }
    #[inline(always)]
    fn set_hl(&mut self, v: u16) {
        [self.regs.h, self.regs.l] = v.to_be_bytes();
    }

    /// 16-bit register pair `i` for LD/INC/DEC/ADD: BC DE HL SP.
    #[inline(always)]
    fn rp(&self, i: u8) -> u16 {
        match i {
            0 => self.bc(),
            1 => self.de(),
            2 => self.hl(),
            _ => self.regs.sp,
        }
    }
    #[inline(always)]
    fn set_rp(&mut self, i: u8, v: u16) {
        let [hi, lo] = v.to_be_bytes();
        match i {
            0 => (self.regs.b, self.regs.c) = (hi, lo),
            1 => (self.regs.d, self.regs.e) = (hi, lo),
            2 => (self.regs.h, self.regs.l) = (hi, lo),
            _ => self.regs.sp = v,
        }
    }

    /// 16-bit register pair `i` for PUSH/POP: BC DE HL AF.
    #[inline(always)]
    fn rp2(&self, i: u8) -> u16 {
        match i {
            3 => u16::from_be_bytes([self.regs.a, self.regs.f]),
            _ => self.rp(i),
        }
    }
    #[inline(always)]
    fn set_rp2(&mut self, i: u8, v: u16) {
        match i {
            // The low nibble of F doesn't exist and always reads 0.
            3 => [self.regs.a, self.regs.f] = (v & 0xFFF0).to_be_bytes(),
            _ => self.set_rp(i, v),
        }
    }

    /// 8-bit operand `r` (B C D E H L (HL) A); (HL) costs a read cycle.
    #[inline(always)]
    fn get_r<B: Bus>(&mut self, bus: &mut B, r: u8) -> u8 {
        match r {
            0 => self.regs.b,
            1 => self.regs.c,
            2 => self.regs.d,
            3 => self.regs.e,
            4 => self.regs.h,
            5 => self.regs.l,
            6 => bus.read(self.hl()),
            _ => self.regs.a,
        }
    }
    #[inline(always)]
    fn set_r<B: Bus>(&mut self, bus: &mut B, r: u8, v: u8) {
        match r {
            0 => self.regs.b = v,
            1 => self.regs.c = v,
            2 => self.regs.d = v,
            3 => self.regs.e = v,
            4 => self.regs.h = v,
            5 => self.regs.l = v,
            6 => bus.write(self.hl(), v),
            _ => self.regs.a = v,
        }
    }

    /// Condition `cc`: NZ Z NC C.
    #[inline(always)]
    fn cond(&self, cc: u8) -> bool {
        let f = self.regs.f;
        match cc {
            0 => f & FZ == 0,
            1 => f & FZ != 0,
            2 => f & FC == 0,
            _ => f & FC != 0,
        }
    }

    // --- Bus helpers. ---

    #[inline(always)]
    fn imm8<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read(self.regs.pc);
        self.regs.pc = self.regs.pc.wrapping_add(1);
        v
    }
    #[inline(always)]
    fn imm16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.imm8(bus);
        let hi = self.imm8(bus);
        u16::from_le_bytes([lo, hi])
    }

    /// Internal cycle, then high byte to SP-1, low byte to SP-2 (PUSH,
    /// CALL, RST all have this shape).
    #[inline(always)]
    fn push<B: Bus>(&mut self, bus: &mut B, v: u16) {
        bus.idle();
        let [hi, lo] = v.to_be_bytes();
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, hi);
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, lo);
    }
    #[inline(always)]
    fn pop<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = bus.read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        let hi = bus.read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        u16::from_le_bytes([lo, hi])
    }

    // --- ALU. ---

    /// ALU operation `op` on A: ADD ADC SUB SBC AND XOR OR CP.
    #[inline(always)]
    fn alu(&mut self, op: u8, v: u8) {
        let a = self.regs.a;
        let carry = (self.regs.f & FC != 0) as u8;
        let (r, f) = match op {
            0 | 1 => {
                let c = if op == 1 { carry } else { 0 };
                let sum = u16::from(a) + u16::from(v) + u16::from(c);
                let h = (a & 0xF) + (v & 0xF) + c > 0xF;
                (sum as u8, flag(h, FH) | flag(sum > 0xFF, FC))
            }
            2 | 3 | 7 => {
                let c = if op == 3 { carry } else { 0 };
                let r = a.wrapping_sub(v).wrapping_sub(c);
                let h = (a & 0xF) < (v & 0xF) + c;
                let cy = u16::from(a) < u16::from(v) + u16::from(c);
                (r, FN | flag(h, FH) | flag(cy, FC))
            }
            4 => (a & v, FH),
            5 => (a ^ v, 0),
            _ => (a | v, 0),
        };
        self.regs.f = f | flag(r == 0, FZ);
        if op != 7 {
            self.regs.a = r;
        }
    }

    #[inline(always)]
    fn inc8(&mut self, v: u8) -> u8 {
        let r = v.wrapping_add(1);
        self.regs.f = (self.regs.f & FC) | flag(r == 0, FZ) | flag(v & 0xF == 0xF, FH);
        r
    }
    #[inline(always)]
    fn dec8(&mut self, v: u8) -> u8 {
        let r = v.wrapping_sub(1);
        self.regs.f = (self.regs.f & FC) | FN | flag(r == 0, FZ) | flag(v & 0xF == 0, FH);
        r
    }

    /// SP + signed e8 with the flags of ADD SP,e8 / LD HL,SP+e8: Z and N
    /// clear, H and C from the unsigned add of the low bytes.
    #[inline(always)]
    fn sp_plus(&mut self, e: u8) -> u16 {
        let sp = self.regs.sp;
        let h = (sp & 0xF) + u16::from(e & 0xF) > 0xF;
        let c = (sp & 0xFF) + u16::from(e) > 0xFF;
        self.regs.f = flag(h, FH) | flag(c, FC);
        sp.wrapping_add(e as i8 as u16)
    }

    /// The CB shift/rotate group `op` (RLC RRC RL RR SLA SRA SWAP SRL),
    /// also used with Z forced clear for RLCA/RRCA/RLA/RRA.
    #[inline(always)]
    fn shift(&mut self, op: u8, v: u8) -> u8 {
        let cin = (self.regs.f & FC != 0) as u8;
        let (r, c) = match op {
            0 => (v.rotate_left(1), v >> 7),
            1 => (v.rotate_right(1), v & 1),
            2 => ((v << 1) | cin, v >> 7),
            3 => ((v >> 1) | (cin << 7), v & 1),
            4 => (v << 1, v >> 7),
            5 => ((v >> 1) | (v & 0x80), v & 1),
            6 => (v.rotate_left(4), 0),
            _ => (v >> 1, v & 1),
        };
        self.regs.f = flag(r == 0, FZ) | flag(c != 0, FC);
        r
    }

    fn daa(&mut self) {
        // Pan Docs: DAA; adjusts after an add (N clear) or subtract (N set)
        // using H and C from that operation.
        let f = self.regs.f;
        let mut a = self.regs.a;
        let mut carry = f & FC != 0;
        if f & FN == 0 {
            if carry || a > 0x99 {
                a = a.wrapping_add(0x60);
                carry = true;
            }
            if f & FH != 0 || a & 0xF > 9 {
                a = a.wrapping_add(0x06);
            }
        } else {
            if carry {
                a = a.wrapping_sub(0x60);
            }
            if f & FH != 0 {
                a = a.wrapping_sub(0x06);
            }
        }
        self.regs.a = a;
        self.regs.f = (f & FN) | flag(a == 0, FZ) | flag(carry, FC);
    }

    // --- The instruction set. ---

    /// Opcode `OP` (not CB-prefixed). Everything below folds to code for
    /// this one opcode.
    #[inline(always)]
    fn op<const OP: u8, B: Bus>(&mut self, bus: &mut B) {
        // x = bits 6-7, y = bits 3-5, z = bits 0-2, p = bits 4-5.
        let y = (OP >> 3) & 7;
        let z = OP & 7;
        let p = y >> 1;
        match OP {
            0x00 => {} // NOP
            0x10 => self.stop(bus),
            // LD (nn),SP: fetch, lo, hi, write lo, write hi.
            0x08 => {
                let addr = self.imm16(bus);
                let [hi, lo] = self.regs.sp.to_be_bytes();
                bus.write(addr, lo);
                bus.write(addr.wrapping_add(1), hi);
            }
            // JR e8: fetch, e8, internal.
            0x18 => {
                let e = self.imm8(bus);
                bus.idle();
                self.regs.pc = self.regs.pc.wrapping_add(e as i8 as u16);
            }
            // JR cc,e8.
            0x20 | 0x28 | 0x30 | 0x38 => {
                let e = self.imm8(bus);
                if self.cond(y - 4) {
                    bus.idle();
                    self.regs.pc = self.regs.pc.wrapping_add(e as i8 as u16);
                }
            }
            // LD rr,nn.
            0x01 | 0x11 | 0x21 | 0x31 => {
                let v = self.imm16(bus);
                self.set_rp(p, v);
            }
            // ADD HL,rr: fetch, internal. Z kept, N clear, H from bit 11,
            // C from bit 15.
            0x09 | 0x19 | 0x29 | 0x39 => {
                let hl = self.hl();
                let v = self.rp(p);
                let (r, c) = hl.overflowing_add(v);
                let h = (hl & 0xFFF) + (v & 0xFFF) > 0xFFF;
                self.regs.f = (self.regs.f & FZ) | flag(h, FH) | flag(c, FC);
                self.set_hl(r);
                bus.idle();
            }
            // LD (BC),A / LD (DE),A / LD (HL+),A / LD (HL-),A.
            0x02 | 0x12 | 0x22 | 0x32 => {
                let addr = self.indirect_addr(p);
                bus.write(addr, self.regs.a);
            }
            // LD A,(BC) / LD A,(DE) / LD A,(HL+) / LD A,(HL-).
            0x0A | 0x1A | 0x2A | 0x3A => {
                let addr = self.indirect_addr(p);
                self.regs.a = bus.read(addr);
            }
            // INC rr / DEC rr: fetch, internal.
            0x03 | 0x13 | 0x23 | 0x33 => {
                self.set_rp(p, self.rp(p).wrapping_add(1));
                bus.idle();
            }
            0x0B | 0x1B | 0x2B | 0x3B => {
                self.set_rp(p, self.rp(p).wrapping_sub(1));
                bus.idle();
            }
            // INC r / DEC r ((HL): fetch, read, write).
            _ if OP < 0x40 && z == 4 => {
                let v = self.get_r(bus, y);
                let r = self.inc8(v);
                self.set_r(bus, y, r);
            }
            _ if OP < 0x40 && z == 5 => {
                let v = self.get_r(bus, y);
                let r = self.dec8(v);
                self.set_r(bus, y, r);
            }
            // LD r,n8.
            _ if OP < 0x40 && z == 6 => {
                let v = self.imm8(bus);
                self.set_r(bus, y, v);
            }
            // RLCA RRCA RLA RRA: like the CB versions but Z always clear.
            0x07 | 0x0F | 0x17 | 0x1F => {
                self.regs.a = self.shift(y, self.regs.a);
                self.regs.f &= FC;
            }
            0x27 => self.daa(),
            0x2F => {
                // CPL
                self.regs.a = !self.regs.a;
                self.regs.f |= FN | FH;
            }
            0x37 => self.regs.f = (self.regs.f & FZ) | FC, // SCF
            0x3F => self.regs.f = (self.regs.f & (FZ | FC)) ^ FC, // CCF
            0x76 => self.halt(bus),
            // LD B,B: also the test-ROM breakpoint (Mooneye convention).
            0x40 => bus.event(Events::LD_B_B),
            // LD r,r'.
            0x41..=0x7F => {
                let v = self.get_r(bus, z);
                self.set_r(bus, y, v);
            }
            // ALU A,r.
            0x80..=0xBF => {
                let v = self.get_r(bus, z);
                self.alu(y, v);
            }
            // RET cc: fetch, internal (condition check), then RET's cycles.
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                bus.idle();
                if self.cond(y) {
                    self.regs.pc = self.pop(bus);
                    bus.idle();
                }
            }
            // RET / RETI: fetch, pop lo, pop hi, internal. RETI enables
            // interrupts at once (no EI delay).
            0xC9 | 0xD9 => {
                self.regs.pc = self.pop(bus);
                bus.idle();
                if OP == 0xD9 {
                    self.ime = true;
                }
            }
            // POP rr.
            0xC1 | 0xD1 | 0xE1 | 0xF1 => {
                let v = self.pop(bus);
                self.set_rp2(p & 3, v);
            }
            // PUSH rr: fetch, internal, write hi, write lo.
            0xC5 | 0xD5 | 0xE5 | 0xF5 => self.push(bus, self.rp2(p & 3)),
            // JP nn / JP cc,nn: fetch, lo, hi, internal (if taken).
            0xC3 | 0xC2 | 0xCA | 0xD2 | 0xDA => {
                let addr = self.imm16(bus);
                if OP == 0xC3 || self.cond(y) {
                    bus.idle();
                    self.regs.pc = addr;
                }
            }
            // JP HL: one cycle.
            0xE9 => self.regs.pc = self.hl(),
            // CALL nn / CALL cc,nn: fetch, lo, hi, (internal, push hi, push lo).
            0xCD | 0xC4 | 0xCC | 0xD4 | 0xDC => {
                let addr = self.imm16(bus);
                if OP == 0xCD || self.cond(y) {
                    self.push(bus, self.regs.pc);
                    self.regs.pc = addr;
                }
            }
            // ALU A,n8.
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let v = self.imm8(bus);
                self.alu(y, v);
            }
            // RST: fetch, internal, push hi, push lo.
            _ if OP >= 0xC0 && z == 7 => {
                self.push(bus, self.regs.pc);
                self.regs.pc = u16::from(y) * 8;
            }
            0xCB => self.execute_cb(bus),
            // LDH (n8),A / LDH A,(n8).
            0xE0 => {
                let n = self.imm8(bus);
                bus.write(0xFF00 | u16::from(n), self.regs.a);
            }
            0xF0 => {
                let n = self.imm8(bus);
                self.regs.a = bus.read(0xFF00 | u16::from(n));
            }
            // LDH (C),A / LDH A,(C).
            0xE2 => bus.write(0xFF00 | u16::from(self.regs.c), self.regs.a),
            0xF2 => self.regs.a = bus.read(0xFF00 | u16::from(self.regs.c)),
            // LD (nn),A / LD A,(nn).
            0xEA => {
                let addr = self.imm16(bus);
                bus.write(addr, self.regs.a);
            }
            0xFA => {
                let addr = self.imm16(bus);
                self.regs.a = bus.read(addr);
            }
            // ADD SP,e8: fetch, e8, internal, internal.
            0xE8 => {
                let e = self.imm8(bus);
                self.regs.sp = self.sp_plus(e);
                bus.idle();
                bus.idle();
            }
            // LD HL,SP+e8: fetch, e8, internal.
            0xF8 => {
                let e = self.imm8(bus);
                let v = self.sp_plus(e);
                self.set_hl(v);
                bus.idle();
            }
            // LD SP,HL: fetch, internal.
            0xF9 => {
                self.regs.sp = self.hl();
                bus.idle();
            }
            0xF3 => {
                // DI: at once, and cancels a pending EI.
                self.ime = false;
                self.ei_pending = false;
            }
            0xFB => {
                // EI: IME turns on after the next instruction.
                self.ei_pending = true;
            }
            // D3 DB DD E3 E4 EB EC ED F4 FC FD.
            _ => self.lock(bus),
        }
    }

    /// The address of LD (BC)/(DE)/(HL+)/(HL-) by `p`, post-incrementing
    /// or decrementing HL.
    #[inline(always)]
    fn indirect_addr(&mut self, p: u8) -> u16 {
        match p {
            0 => self.bc(),
            1 => self.de(),
            2 => {
                let hl = self.hl();
                self.set_hl(hl.wrapping_add(1));
                hl
            }
            _ => {
                let hl = self.hl();
                self.set_hl(hl.wrapping_sub(1));
                hl
            }
        }
    }

    /// CB-prefixed opcode `OP`. Register forms: fetch CB, fetch op (2
    /// cycles). (HL) forms: + read + write (4), BIT b,(HL): + read (3).
    #[inline(always)]
    fn cb<const OP: u8, B: Bus>(&mut self, bus: &mut B) {
        let y = (OP >> 3) & 7;
        let z = OP & 7;
        let v = self.get_r(bus, z);
        match OP >> 6 {
            0 => {
                let r = self.shift(y, v);
                self.set_r(bus, z, r);
            }
            // BIT: Z = !bit, N clear, H set, C kept.
            1 => self.regs.f = (self.regs.f & FC) | FH | flag(v & (1 << y) == 0, FZ),
            2 => self.set_r(bus, z, v & !(1 << y)), // RES
            _ => self.set_r(bus, z, v | (1 << y)),  // SET
        }
    }
}

/// `bit` if `cond`, else 0.
#[inline(always)]
const fn flag(cond: bool, bit: u8) -> u8 {
    if cond { bit } else { 0 }
}

#[cfg(test)]
mod tests {
    //! A few hand-written cases; the exhaustive check is the SM83
    //! single-step suite in `tools/sm83-tests`.

    extern crate std;
    use super::super::INT_MASK;
    use super::*;
    use std::vec::Vec;

    /// Flat 64 KiB RAM that logs every M-cycle: (addr, value, kind).
    struct TestBus {
        mem: [u8; 0x10000],
        log: Vec<(u16, u8, char)>,
        ie: u8,
        iflags: u8,
        events: Events,
        /// IF bits set at the end of the M-cycle that reads this address.
        irq_on_read: Option<(u16, u8)>,
    }

    impl TestBus {
        fn new(program: &[u8], at: u16) -> Self {
            let mut mem = [0u8; 0x10000];
            mem[usize::from(at)..usize::from(at) + program.len()].copy_from_slice(program);
            Self {
                mem,
                log: Vec::new(),
                ie: 0,
                iflags: 0,
                events: Events::EMPTY,
                irq_on_read: None,
            }
        }
    }

    impl Bus for TestBus {
        fn read(&mut self, addr: u16) -> u8 {
            let v = self.mem[usize::from(addr)];
            self.log.push((addr, v, 'r'));
            if let Some((at, bits)) = self.irq_on_read
                && at == addr
            {
                self.iflags |= bits;
            }
            v
        }
        fn write(&mut self, addr: u16, value: u8) {
            self.mem[usize::from(addr)] = value;
            self.log.push((addr, value, 'w'));
        }
        fn idle(&mut self) {
            self.log.push((0, 0, '-'));
        }
        fn interrupt_enable(&self) -> u8 {
            self.ie
        }
        fn interrupt_flags(&self) -> u8 {
            self.iflags
        }
        fn set_interrupt_flags(&mut self, value: u8) {
            self.iflags = value;
        }
        fn event(&mut self, event: Events) {
            self.events |= event;
        }
    }

    fn cpu_at(pc: u16) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.regs.pc = pc;
        cpu.regs.sp = 0xFFFE;
        cpu
    }

    #[test]
    fn ei_takes_effect_after_the_next_instruction() {
        // EI; NOP; NOP with an interrupt pending.
        let mut bus = TestBus::new(&[0xFB, 0x00, 0x00], 0x100);
        bus.ie = 0x04;
        bus.iflags = 0x04;
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus); // EI
        assert!(!cpu.ime() && cpu.ei_pending());
        cpu.step(&mut bus); // NOP still runs
        assert_eq!(cpu.regs.pc, 0x102);
        assert!(cpu.ime());
        bus.log.clear();
        cpu.step(&mut bus); // dispatch to the timer vector
        assert_eq!(cpu.regs.pc, 0x50);
        assert_eq!(bus.iflags, 0);
        assert!(!cpu.ime());
        assert_eq!(bus.log.len(), 5);
        assert_eq!(bus.mem[0xFFFD], 0x01);
        assert_eq!(bus.mem[0xFFFC], 0x02);
    }

    #[test]
    fn ei_then_di_keeps_interrupts_off() {
        let mut bus = TestBus::new(&[0xFB, 0xF3, 0x00], 0x100);
        bus.ie = 0x01;
        bus.iflags = 0x01;
        let mut cpu = cpu_at(0x100);
        for _ in 0..3 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.regs.pc, 0x103);
        assert!(!cpu.ime());
    }

    #[test]
    fn halt_waits_for_an_interrupt_even_with_ime_off() {
        let mut bus = TestBus::new(&[0x76, 0x3C], 0x100); // HALT; INC A
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus);
        assert!(cpu.halted());
        for _ in 0..10 {
            cpu.step(&mut bus);
        }
        assert!(cpu.halted());
        assert_eq!(bus.log.len(), 11);
        bus.ie = 0x01;
        bus.iflags = 0x01;
        cpu.step(&mut bus); // wakes and runs INC A (IME off: no dispatch)
        assert!(!cpu.halted());
        assert_eq!(cpu.regs.a, 1);
        assert_eq!(cpu.regs.pc, 0x102);
    }

    #[test]
    fn halt_bug_reads_the_next_byte_twice() {
        // IME off, interrupt pending: HALT doesn't halt and the INC A after
        // it runs twice (Pan Docs: halt bug).
        let mut bus = TestBus::new(&[0x76, 0x3C, 0x00], 0x100);
        bus.ie = 0x01;
        bus.iflags = 0x01;
        let mut cpu = cpu_at(0x100);
        for _ in 0..3 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.regs.a, 2);
        assert_eq!(cpu.regs.pc, 0x102);
    }

    #[test]
    fn halt_bug_before_rst_pushes_the_rst_address() {
        // The bugged fetch doesn't advance PC, so RST 38 pushes its own
        // address (Pan Docs: halt bug).
        let mut bus = TestBus::new(&[0x76, 0xFF], 0x100);
        bus.ie = 0x01;
        bus.iflags = 0x01;
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus); // HALT (bugged)
        cpu.step(&mut bus); // RST 38
        assert_eq!(cpu.regs.pc, 0x38);
        assert_eq!((bus.mem[0xFFFD], bus.mem[0xFFFC]), (0x01, 0x01));
    }

    #[test]
    fn halt_with_ime_and_pending_interrupt_returns_to_the_halt() {
        // IME on and an interrupt raised during HALT's fetch, so pending at
        // HALT's check: no halt, and the dispatch pushes the HALT's own
        // address (SameBoy `halt()`).
        let mut bus = TestBus::new(&[0x76, 0x3C], 0x100); // HALT; INC A
        bus.ie = 0x04;
        bus.irq_on_read = Some((0x100, 0x04));
        let mut cpu = cpu_at(0x100);
        cpu.ime = true;
        cpu.step(&mut bus); // HALT
        assert!(!cpu.halted());
        cpu.step(&mut bus); // dispatch
        assert_eq!(cpu.regs.pc, 0x50);
        assert_eq!((bus.mem[0xFFFD], bus.mem[0xFFFC]), (0x01, 0x00));
        assert_eq!(cpu.regs.a, 0);
    }

    #[test]
    fn ei_halt_with_pending_interrupt_returns_to_the_halt() {
        // EI; HALT; INC A with an interrupt pending: EI's delay has set IME
        // by HALT, so the handler returns to the HALT, which then halts
        // (Pan Docs: halt bug; SameSuite `ei_delay_halt`).
        let mut bus = TestBus::new(&[0xFB, 0x76, 0x3C], 0x100);
        bus.mem[0x50] = 0xD9; // RETI
        bus.ie = 0x04;
        bus.iflags = 0x04;
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus); // EI
        cpu.step(&mut bus); // HALT
        cpu.step(&mut bus); // dispatch
        assert_eq!(cpu.regs.pc, 0x50);
        assert_eq!((bus.mem[0xFFFD], bus.mem[0xFFFC]), (0x01, 0x01));
        cpu.step(&mut bus); // RETI
        assert_eq!(cpu.regs.pc, 0x101);
        cpu.step(&mut bus); // HALT again, nothing pending now
        assert!(cpu.halted());
        assert_eq!(cpu.regs.a, 0);
    }

    #[test]
    fn halt_wake_up_takes_one_extra_mcycle() {
        // CGB: once IE & IF is set, the halted CPU idles one more M-cycle
        // (4 clocks at single speed) before the next fetch (IME off) or
        // the 5-M-cycle dispatch (IME on) (SameBoy `GB_cpu_run`).
        for ime in [false, true] {
            let mut bus = TestBus::new(&[0x76, 0x3C], 0x100); // HALT; INC A
            let mut cpu = cpu_at(0x100);
            cpu.ime = ime;
            cpu.step(&mut bus); // HALT
            cpu.step(&mut bus); // one idle M-cycle while halted
            assert!(cpu.halted());
            bus.ie = 0x01;
            bus.iflags = 0x01;
            bus.log.clear();
            cpu.step(&mut bus);
            assert!(!cpu.halted());
            assert_eq!(bus.log[0], (0, 0, '-'), "ime={ime}");
            if ime {
                // Idle + dispatch (2 idle, 2 pushes, 1 idle) = 24 clocks.
                assert_eq!(bus.log.len(), 6, "ime={ime}");
                assert_eq!(cpu.regs.pc, 0x40);
                assert_eq!((bus.mem[0xFFFD], bus.mem[0xFFFC]), (0x01, 0x01));
            } else {
                // Idle + the INC A fetch = 8 clocks.
                assert_eq!(bus.log.len(), 2, "ime={ime}");
                assert_eq!(bus.log[1], (0x101, 0x3C, 'r'));
                assert_eq!(cpu.regs.a, 1);
            }
        }
    }

    #[test]
    fn dispatch_cancelled_when_push_clears_ie() {
        // SP = 0: the high byte of PC is pushed to FFFF (IE) first. With
        // PC = 0x0200 that writes 0x02 to IE, so the pending VBlank
        // interrupt (bit 0) is no longer enabled: PC becomes 0000.
        struct IeBus(TestBus);
        impl Bus for IeBus {
            fn read(&mut self, a: u16) -> u8 {
                self.0.read(a)
            }
            fn write(&mut self, a: u16, v: u8) {
                if a == 0xFFFF {
                    self.0.ie = v;
                }
                self.0.write(a, v);
            }
            fn idle(&mut self) {
                self.0.idle();
            }
            fn interrupt_enable(&self) -> u8 {
                self.0.ie
            }
            fn interrupt_flags(&self) -> u8 {
                self.0.iflags
            }
            fn set_interrupt_flags(&mut self, v: u8) {
                self.0.iflags = v;
            }
        }
        let mut bus = IeBus(TestBus::new(&[0x00], 0x200));
        bus.0.ie = 0x01;
        bus.0.iflags = 0x01;
        let mut cpu = cpu_at(0x200);
        cpu.regs.sp = 0x0000;
        cpu.set_ime(true);
        cpu.step(&mut bus);
        assert_eq!(cpu.regs.pc, 0x0000);
        assert_eq!(bus.0.iflags & INT_MASK, 0x01, "not acknowledged");
    }

    #[test]
    fn invalid_opcode_locks_once() {
        let mut bus = TestBus::new(&[0xD3], 0x100);
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus);
        assert!(cpu.locked());
        assert_eq!(bus.events, Events::CPU_LOCKED);
        bus.events = Events::EMPTY;
        bus.ie = 0x1F;
        bus.iflags = 0x1F;
        cpu.set_ime(true);
        cpu.step(&mut bus);
        assert!(cpu.locked());
        assert!(bus.events.is_empty());
        assert_eq!(cpu.regs.pc, 0x101);
    }

    #[test]
    fn ld_b_b_raises_the_event() {
        let mut bus = TestBus::new(&[0x40], 0x100);
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus);
        assert_eq!(bus.events, Events::LD_B_B);
    }

    #[test]
    fn daa_after_add_and_sub() {
        // 0x15 + 0x27 = 0x3C -> DAA -> 0x42.
        let mut bus = TestBus::new(&[0x3E, 0x15, 0xC6, 0x27, 0x27], 0x100);
        let mut cpu = cpu_at(0x100);
        for _ in 0..3 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.regs.a, 0x42);
        // 0x42 - 0x15 = 0x2D -> DAA -> 0x27.
        let mut bus = TestBus::new(&[0x3E, 0x42, 0xD6, 0x15, 0x27], 0x100);
        let mut cpu = cpu_at(0x100);
        for _ in 0..3 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.regs.a, 0x27);
        assert_eq!(cpu.regs.f & FN, FN);
    }

    #[test]
    fn cycle_counts_of_representative_instructions() {
        // (program, M-cycles) per GBCTR.
        let cases: &[(&[u8], usize)] = &[
            (&[0x00], 1),
            (&[0x01, 0, 0], 3),
            (&[0x08, 0, 0xC0], 5),
            (&[0x18, 0x00], 3),
            (&[0x34], 3),
            (&[0x36, 0], 3),
            (&[0xC3, 0, 0], 4),
            (&[0xCD, 0, 0], 6),
            (&[0xC9], 4),
            (&[0xC5], 4),
            (&[0xC1], 3),
            (&[0xE8, 1], 4),
            (&[0xF8, 1], 3),
            (&[0xF9], 2),
            (&[0xCB, 0x00], 2),
            (&[0xCB, 0x46], 3),
            (&[0xCB, 0x06], 4),
            (&[0xFF], 4),
        ];
        for &(prog, cycles) in cases {
            let mut bus = TestBus::new(prog, 0x100);
            let mut cpu = cpu_at(0x100);
            cpu.step(&mut bus);
            assert_eq!(bus.log.len(), cycles, "{prog:02X?}");
        }
        // Conditional branches not taken / taken (F = 0: NZ true, Z false).
        let mut bus = TestBus::new(&[0xC0], 0x100);
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus);
        assert_eq!(bus.log.len(), 5); // RET NZ taken
        let mut bus = TestBus::new(&[0xC8], 0x100);
        let mut cpu = cpu_at(0x100);
        cpu.step(&mut bus);
        assert_eq!(bus.log.len(), 2); // RET Z not taken
    }
}
