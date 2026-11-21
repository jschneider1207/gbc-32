//! The memory bus: everything the CPU reaches ([`Sys`]), the per-run-call
//! context the CPU drives ([`Ctx`]), the memory map and the I/O registers.
//! Design: ADR 0008.
//!
//! Each CPU bus call is one M-cycle: the access happens at the start of the
//! cycle (it sees the state as of the current clock), then [`Ctx::tick`]
//! advances the clock, the timer and the serial port by one M-cycle, and
//! runs scheduled events (the PPU's VBlank) when the clock reaches
//! `Sys::next_event`. The PPU is otherwise caught up only when one of its
//! registers is written (ADR 0002).

use crate::apu::StereoSample;
use crate::cart::Cart;
use crate::config::Config;
use crate::cpu::{Bus, INT_MASK};
use crate::joypad::{p1_interrupt, p1_lines};
use crate::memory::Memory;
use crate::ppu::{Frame, Ppu};
use crate::serial::{INT_SERIAL, Serial};
use crate::timer::Timer;
use crate::{Buttons, Emulator, Events};

/// Everything the bus reaches except the RAM arrays (in [`Memory`]) and the
/// CPU itself. Lives in `Hw`, so it counts toward the 1 KiB limit there.
pub(crate) struct Sys<'a> {
    /// Base clocks since the last load or reset.
    pub(crate) clock: u64,
    /// Base clocks per M-cycle: 4, or 2 in CGB double speed.
    pub(crate) mcycle: u32,
    /// The clock at which [`Ctx::run_events`] must next run (the PPU's
    /// next VBlank; `u64::MAX` if nothing is scheduled).
    pub(crate) next_event: u64,
    pub(crate) ppu: Ppu,
    pub(crate) timer: Timer,
    pub(crate) serial: Serial,
    pub(crate) cart: Cart<'a>,
    pub(crate) config: Config,
    pub(crate) buttons: Buttons,
    /// IE (FFFF), all 8 bits.
    pub(crate) ie: u8,
    /// IF (FF0F), low 5 bits.
    pub(crate) iflags: u8,
    /// P1/JOYP select bits (4-5) as written.
    pub(crate) joyp_select: u8,
    /// The cartridge runs in CGB mode (header 0143 bit 7); otherwise DMG
    /// compatibility mode, where the CGB-only registers are locked (KEY0 =
    /// 04; Pan Docs: Power Up Sequence; SameBoy `cgb_mode`).
    pub(crate) cgb_mode: bool,
    /// VBK (bit 0) and SVBK (bits 0-2) as written.
    pub(crate) vbk: u8,
    pub(crate) svbk: u8,
    /// Offsets of the VRAM bank at 8000 and the WRAM bank at D000.
    vram_off: usize,
    wram_off: usize,
    /// KEY1 bit 0 (speed switch armed).
    pub(crate) key1: u8,
}

impl Sys<'_> {
    pub(crate) const fn new() -> Self {
        Self {
            clock: 0,
            mcycle: 4,
            next_event: u64::MAX,
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
        self.ppu.reset();
        self.next_event = self.ppu.next_event();
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
        // TODO(M3): DIV, LY and STAT where the boot ROM hands over
        // (boot_hwio).
        for &(addr, value) in POST_BOOT_IO {
            mem.io[usize::from(addr & 0x7F)] = value;
        }
        // OPRI bit 0: the boot ROM selects coordinate priority (1) for
        // DMG compatibility mode, and leaves 0 (OAM index priority) in CGB
        // mode (Pan Docs: FF6C - OPRI; SameBoy Core/memory.c, OPRI write
        // allowed while the boot ROM runs). Mooneye `boot_hwio-C` expects
        // FF6C = FF in DMG compatibility mode.
        mem.io[0x6C] = u8::from(!self.cgb_mode);
    }
}

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
    (0xFF26, 0xF1),
    (0xFF47, 0xFC),
];

/// The context of one run call: what the CPU's bus reaches, plus the
/// caller's buffers and the events raised so far. The CPU gets one pointer
/// to it, so a lazy PPU/APU catch-up on any bus access reaches the frame
/// and audio buffers without threading them through every call (ADR 0008).
pub(crate) struct Ctx<'r, 'a> {
    pub(crate) mem: &'r mut Memory,
    pub(crate) sys: &'r mut Sys<'a>,
    pub(crate) frame: &'r mut Frame,
    /// The caller's audio buffer and how much of it is written. TODO(M6).
    #[allow(dead_code)]
    pub(crate) audio: &'r mut [StereoSample],
    #[allow(dead_code)]
    pub(crate) audio_len: usize,
    pub(crate) events: Events,
}

impl Ctx<'_, '_> {
    /// Advances everything but the CPU by one M-cycle.
    #[inline(always)]
    fn tick(&mut self) {
        let s = &mut *self.sys;
        s.clock += u64::from(s.mcycle);
        let fell = s.timer.tick(&mut s.iflags);
        if fell & s.serial.clock_bit != 0 {
            self.serial_clock(fell);
        }
        if self.sys.clock >= self.sys.next_event {
            self.run_events();
        }
    }

    #[inline(never)]
    fn serial_clock(&mut self, fell: u16) {
        if self.sys.serial.clock(fell) {
            self.sys.iflags |= INT_SERIAL;
            self.events |= Events::SERIAL_TX;
        }
    }

    /// Scheduled work at `Sys::next_event`.
    #[inline(never)]
    fn run_events(&mut self) {
        self.sync_ppu();
    }

    /// Catches the PPU up to the current clock and reschedules.
    fn sync_ppu(&mut self) {
        let s = &mut *self.sys;
        self.events |= s.ppu.sync(s.clock, self.frame, &mut s.iflags);
        s.next_event = s.ppu.next_event();
    }

    /// A CPU write at the current clock (no M-cycle; the caller ticks).
    #[inline]
    pub(crate) fn write_mapped(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match addr >> 13 {
            0..=3 => self.sys.cart.write_rom(addr, value),
            4 => self.mem.vram[(self.sys.vram_off | (a & 0x1FFF)) & 0x3FFF] = value,
            5 => self.sys.cart.write_ram(addr, value),
            6 => *wram_mut(self.mem, self.sys, a) = value,
            _ if addr < 0xFE00 => *wram_mut(self.mem, self.sys, a) = value,
            _ => self.write_high(addr, value),
        }
    }

    /// FE00-FFFF.
    fn write_high(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match addr {
            0xFE00..=0xFE9F => self.mem.oam[a & 0xFF] = value,
            0xFEA0..=0xFEFF => self.mem.extra_oam[extra_oam_index(a)] = value,
            0xFF00..=0xFF7F => self.write_io(a & 0x7F, value),
            0xFF80..=0xFFFE => self.mem.hram[a - 0xFF80] = value,
            0xFFFF => self.sys.ie = value,
            _ => {}
        }
    }

    /// FF00-FF7F, by the low 7 address bits.
    fn write_io(&mut self, reg: usize, value: u8) {
        let s = &mut *self.sys;
        let cgb = s.cgb_mode;
        match reg {
            0x00 => s.set_joypad(s.buttons, value),
            0x01 => s.serial.sb = value,
            0x02 => s.serial.write_sc(value, cgb),
            0x04 => {
                let fell = s.timer.write_div();
                if fell & s.serial.clock_bit != 0 {
                    self.serial_clock(fell);
                }
            }
            0x05 => s.timer.tima = value,
            0x06 => s.timer.tma = value,
            0x07 => s.timer.write_tac(value),
            0x0F => s.iflags = value & INT_MASK,
            // Sound: plain storage until M6.
            0x10..=0x3F => self.mem.io[reg] = value,
            0x40 => {
                self.sync_ppu();
                let s = &mut *self.sys;
                s.ppu.write_lcdc(s.clock, value);
                s.next_event = s.ppu.next_event();
            }
            0x41 => s.ppu.write_stat(value),
            0x45 => s.ppu.lyc = value,
            0x46 => self.oam_dma(value),
            // SCY SCX BGP OBP0 OBP1 WY WX: plain storage until M3.
            0x42 | 0x43 | 0x47..=0x4B => self.mem.io[reg] = value,
            0x4D if cgb => s.key1 = value & 0x01,
            0x4F if cgb => {
                s.vbk = value & 0x01;
                s.vram_off = usize::from(s.vbk) * 0x2000;
            }
            // CGB palettes: plain storage until M5.
            0x68..=0x6B if cgb => self.mem.io[reg] = value,
            // OPRI: writable in CGB mode only (after the boot ROM). Plain
            // storage until the M3 renderer uses it.
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

    /// OAM DMA (FF46). Stub: copies all 160 bytes at once. TODO(M2): one
    /// byte per M-cycle, the start delay and bus conflicts.
    fn oam_dma(&mut self, value: u8) {
        self.mem.io[0x46] = value;
        let src = u16::from(value) << 8;
        for i in 0..0xA0u16 {
            let v = read(self.mem, self.sys, src.wrapping_add(i));
            self.mem.oam[usize::from(i)] = v;
        }
    }
}

impl Bus for Ctx<'_, '_> {
    #[inline(always)]
    fn read(&mut self, addr: u16) -> u8 {
        let v = read(self.mem, self.sys, addr);
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
        self.events |= event;
    }

    fn stop(&mut self) -> bool {
        // STOP resets DIV (Pan Docs: FF04 - DIV).
        let fell = self.sys.timer.write_div();
        if fell & self.sys.serial.clock_bit != 0 {
            self.serial_clock(fell);
        }
        if !self.sys.cgb_mode || self.sys.key1 & 0x01 == 0 {
            return false;
        }
        // CGB speed switch (Pan Docs: FF4D - KEY1): the armed bit clears,
        // the speed toggles, and the CPU stops for 2050 M-cycles, during
        // which DIV does not tick. TODO(M5): the exact pause length and
        // the PPU access freeze during it.
        self.sys.key1 = 0;
        self.sys.mcycle = if self.sys.mcycle == 4 { 2 } else { 4 };
        for _ in 0..2050 {
            self.sys.clock += u64::from(self.sys.mcycle);
            if self.sys.clock >= self.sys.next_event {
                self.run_events();
            }
        }
        true
    }

    fn stop_wakeup(&self) -> bool {
        !self.sys.buttons.is_empty()
    }
}

/// A read as the CPU sees it at the current clock, without side effects
/// (no read on this hardware has any). Shared by the CPU bus and
/// [`Emulator::peek`].
#[inline(always)]
pub(crate) fn read(mem: &Memory, sys: &Sys, addr: u16) -> u8 {
    let a = usize::from(addr);
    match addr >> 13 {
        0..=3 => sys.cart.read_rom(addr),
        4 => mem.vram[(sys.vram_off | (a & 0x1FFF)) & 0x3FFF],
        5 => sys.cart.read_ram(addr),
        6 => wram(mem, sys, a),
        // E000-FDFF: echo of C000-DDFF (Pan Docs: Memory Map).
        _ if addr < 0xFE00 => wram(mem, sys, a),
        _ => read_high(mem, sys, addr),
    }
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

/// FE00-FFFF.
fn read_high(mem: &Memory, sys: &Sys, addr: u16) -> u8 {
    let a = usize::from(addr);
    match addr {
        0xFE00..=0xFE9F => mem.oam[a & 0xFF],
        0xFEA0..=0xFEFF => mem.extra_oam[extra_oam_index(a)],
        0xFF00..=0xFF7F => read_io(mem, sys, a & 0x7F),
        0xFF80..=0xFFFE => mem.hram[a - 0xFF80],
        _ => sys.ie,
    }
}

/// FF00-FF7F by the low 7 address bits. Unused bits read 1; registers
/// that don't exist (or are CGB-only, in DMG compatibility mode) read FF.
fn read_io(mem: &Memory, sys: &Sys, reg: usize) -> u8 {
    let cgb = sys.cgb_mode;
    match reg {
        // P1/JOYP (Pan Docs: Joypad Input).
        0x00 => 0xC0 | sys.joyp_select | p1_lines(sys.buttons, sys.joyp_select),
        0x01 => sys.serial.sb,
        0x02 => sys.serial.read_sc(cgb),
        0x04 => sys.timer.div(),
        0x05 => sys.timer.tima,
        0x06 => sys.timer.tma,
        0x07 => 0xF8 | sys.timer.tac,
        0x0F => 0xE0 | sys.iflags,
        // TODO(M6): sound register read masks.
        0x10..=0x3F => mem.io[reg],
        0x40 => sys.ppu.lcdc,
        0x41 => sys.ppu.stat(sys.clock),
        0x44 => sys.ppu.ly(sys.clock),
        0x45 => sys.ppu.lyc,
        0x42 | 0x43 | 0x46..=0x4B => mem.io[reg],
        0x4D if cgb => 0x7E | sys.key1 | if sys.mcycle == 2 { 0x80 } else { 0 },
        0x4F if cgb => 0xFE | sys.vbk,
        0x68..=0x6B if cgb => mem.io[reg],
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
        // TODO(M6): FF76/FF77 PCM12/PCM34 (the channel amplitudes; read
        // only, CGB) read 00 with the APU silent.
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
        read(&self.mem, &self.hw.sys, addr)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::{Config, Frame, cart_ram_size};
    use std::boxed::Box;
    use std::vec;

    /// An emulator with `rom` loaded and a RAM buffer of the header's size.
    fn load(rom: &'static [u8]) -> &'static mut Emulator<'static> {
        let ram = vec![0u8; cart_ram_size(rom).unwrap()].leak();
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
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
        let mut ctx = Ctx {
            mem,
            sys: &mut hw.sys,
            frame: &mut fb,
            audio: &mut [],
            audio_len: 0,
            events: Events::EMPTY,
        };
        ctx.write_mapped(addr, value);
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
        for reg in [0xFF4D, 0xFF4F, 0xFF70] {
            assert_eq!(emu.peek(reg), 0xFF, "{reg:#06x}");
        }
        assert_eq!(emu.peek(0xD000), 0x01);
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
