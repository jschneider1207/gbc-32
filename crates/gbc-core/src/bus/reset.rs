//! Power-on: the blank [`Sys`] and the state the boot ROM leaves behind
//! (Pan Docs: Power Up Sequence).

use super::{Hot, Sys};
use crate::Buttons;
use crate::cart::Cart;
use crate::config::Config;
use crate::dma::ReadClears;
use crate::hdma::VramDma;
use crate::joypad::InputQueue;
use crate::memory::Memory;
use crate::ppu::Ppu;
use crate::schedule::{EventKind, Schedule};
use crate::serial::Serial;
use crate::speed::Speed;
use crate::timer::Timer;

impl Sys<'_> {
    #[inline(never)]
    pub(crate) const fn new() -> Self {
        Self {
            hot: Hot::new(),
            sched: Schedule::new(),
            inputs: InputQueue::new(),
            vdma: VramDma::new(),
            dma_read_clears: ReadClears::new(),
            stop_black: false,
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
            switch_halt_end: 0,
        }
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
        self.sched
            .set(EventKind::Ppu, self.ppu.next_event(Speed::Normal));
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
        self.set_div_apu_deadline(mem.apu.fs, 0);
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
fn boot_logo(vram: &mut [u8; 0x4000], rom: &[u8]) {
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
        let b = rom.get(0x104 + i / 2).copied().unwrap_or(0);
        let d = DOUBLED[usize::from(if i & 1 == 0 { b >> 4 } else { b & 0x0F })];
        vram[0x10 + 4 * i] = d;
        vram[0x12 + 4 * i] = d;
    }
    vram[0x190..0x1A0].copy_from_slice(&TRADEMARK);
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
    (0xFF47, 0xFC),
];

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::bus::test_util::{load, rom};

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
    fn serial_control_after_boot() {
        // Pan Docs: Power Up Sequence (CGB 7F); Mooneye boot_hwio-C (DMG
        // compatibility mode on CGB: 7E).
        assert_eq!(load(rom(0x8000, 0, 0, 0x80)).peek(0xFF02), 0x7F);
        assert_eq!(load(rom(0x8000, 0, 0, 0x00)).peek(0xFF02), 0x7E);
    }
}
