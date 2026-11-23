//! Synthetic benchmark ROMs, hand-assembled SM83 built in memory (no ROM
//! files in git). Each one stresses a path the test-ROM benchmarks miss
//! (docs/perf.md). All are 32 KiB, no MBC, CGB-only, with a valid header
//! and checksums; execution starts at 0x0100 as the CGB boot ROM leaves it.

/// The names `bench synth:<name>` accepts.
pub const NAMES: &[&str] = &[
    "event-storm",
    "dma-storm",
    "halt-irq",
    "worst-case",
    "cgb-raster",
];

/// Nintendo logo (header 0x104-0x133), checked by the real boot ROM.
const LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// Timer and serial event storm in double speed: switch speed (KEY1 +
/// STOP), then TIMA at 262 kHz with TMA=FF (an overflow and reload every
/// 8 base clocks), and the serial port re-armed on the CGB fast internal
/// clock (SC=83) as soon as each transfer ends. IE=0, so only the event
/// scheduler and the polling loop run.
const EVENT_STORM: &[u8] = &[
    0xF3, //             DI
    0x3E, 0x01, //       LD A,01
    0xE0, 0x4D, //       LDH (KEY1),A    arm the speed switch
    0x10, 0x00, //       STOP
    0x3E, 0xFF, //       LD A,FF
    0xE0, 0x06, //       LDH (TMA),A
    0x3E, 0x05, //       LD A,05
    0xE0, 0x07, //       LDH (TAC),A     on, 262 kHz
    // wait:
    0xF0, 0x02, //       LDH A,(SC)
    0x87, //             ADD A,A         carry = transfer running
    0x38, 0xFB, //       JR C,wait
    0x3E, 0x83, //       LD A,83         start, fast clock, internal
    0xE0, 0x02, //       LDH (SC),A
    0x18, 0xF5, //       JR wait
];

/// Back-to-back OAM DMA from WRAM with the CPU running from ROM: start a
/// DMA (FF46=C0), then copy ROM bytes to WRAM for 20 iterations of an
/// 8 M-cycle loop (just over the 160 M-cycle transfer), and restart.
const DMA_STORM: &[u8] = &[
    0x21, 0x00, 0x00, // LD HL,0000
    0x11, 0x00, 0xC1, // LD DE,C100
    // dma:
    0x3E, 0xC0, //       LD A,C0
    0xE0, 0x46, //       LDH (DMA),A
    0x06, 0x14, //       LD B,20
    // copy:
    0x2A, //             LD A,(HL+)      ROM read during DMA
    0x12, //             LD (DE),A       WRAM write during DMA
    0x05, //             DEC B
    0x20, 0xFB, //       JR NZ,copy
    0x21, 0x00, 0x00, // LD HL,0000
    0x11, 0x00, 0xC1, // LD DE,C100
    0x18, 0xED, //       JR dma
];

/// HALT woken by the timer IRQ: TIMA at 262 kHz from TMA=F0 overflows
/// every 256 base clocks; the handler at 0x50 is RETI.
const HALT_IRQ: &[u8] = &[
    0x3E, 0xF0, //       LD A,F0
    0xE0, 0x06, //       LDH (TMA),A
    0x3E, 0x05, //       LD A,05
    0xE0, 0x07, //       LDH (TAC),A
    0x3E, 0x04, //       LD A,04
    0xE0, 0xFF, //       LDH (IE),A      timer only
    0xAF, //             XOR A
    0xE0, 0x0F, //       LDH (IF),A
    0xFB, //             EI
    // idle:
    0x76, //             HALT
    0x00, //             NOP
    0x18, 0xFC, //       JR idle
];

/// Combined worst case: the event storm plus the busiest PPU frame. After
/// the speed switch, with the LCD off: tiles 0 and 1 solid, 40 objects in
/// four groups of 10 (8x16, Y=16/52/88/124, X=8+6i so they overlap) giving
/// 10 objects on 64 of the 144 lines (40 objects cannot cover more), then
/// LCD on with BG, window (WY=0, WX=87), objects, every STAT source
/// (modes 0/1/2 and LYC=72) and only IE=STAT. The STAT handler at 0x48 bumps
/// a WRAM counter. Then the event-storm timer and serial loop runs, which
/// also keeps a CGB HBlank HDMA going: 128 blocks from ROM 0000 to VRAM
/// 8800 (tiles 128-255, which nothing displays), restarted as soon as one
/// ends, so every visible line copies a 16-byte block.
const WORST_CASE: &[u8] = &[
    0xF3, //                   DI
    0x3E, 0x01, //             LD A,01
    0xE0, 0x4D, //             LDH (KEY1),A
    0x10, 0x00, //             STOP
    0x3E, 0xFF, //             LD A,FF
    0xE0, 0x06, //             LDH (TMA),A
    0x3E, 0x05, //             LD A,05
    0xE0, 0x07, //             LDH (TAC),A     on, 262 kHz
    0xAF, //                   XOR A
    0xE0, 0x40, //             LDH (LCDC),A    LCD off: free VRAM/OAM access
    0x21, 0x00, 0x80, //       LD HL,8000
    0x3E, 0xFF, //             LD A,FF
    0x06, 0x20, //             LD B,32         tiles 0 and 1 solid
    0x22, //                   LD (HL+),A
    0x05, //                   DEC B
    0x20, 0xFC, //             JR NZ,-4
    0x21, 0x00, 0xFE, //       LD HL,FE00
    0x16, 0x10, //             LD D,16         first group's Y
    0x1E, 0x04, //             LD E,4          groups
    // group:
    0x06, 0x0A, //             LD B,10
    0x0E, 0x08, //             LD C,8          first X
    // obj:
    0x7A, //                   LD A,D
    0x22, //                   LD (HL+),A      Y
    0x79, //                   LD A,C
    0x22, //                   LD (HL+),A      X
    0xAF, //                   XOR A
    0x22, //                   LD (HL+),A      tile 0
    0x22, //                   LD (HL+),A      attributes 0
    0x79, //                   LD A,C
    0xC6, 0x06, //             ADD A,6
    0x4F, //                   LD C,A
    0x05, //                   DEC B
    0x20, 0xF2, //             JR NZ,obj
    0x7A, //                   LD A,D
    0xC6, 0x24, //             ADD A,36
    0x57, //                   LD D,A
    0x1D, //                   DEC E
    0x20, 0xE7, //             JR NZ,group
    0xAF, //                   XOR A
    0xE0, 0x4A, //             LDH (WY),A
    0x3E, 0x57, //             LD A,87
    0xE0, 0x4B, //             LDH (WX),A
    0x3E, 0x48, //             LD A,72
    0xE0, 0x45, //             LDH (LYC),A
    0x3E, 0x78, //             LD A,78         STAT: LYC, mode 2, 1, 0
    0xE0, 0x41, //             LDH (STAT),A
    0x3E, 0x02, //             LD A,02
    0xE0, 0xFF, //             LDH (IE),A      STAT only
    0xAF, //                   XOR A
    0xE0, 0x0F, //             LDH (IF),A
    0x3E, 0xB7, //             LD A,B7         LCD, window, 8x16 obj, 8000 tiles, obj, BG
    0xE0, 0x40, //             LDH (LCDC),A
    0xFB, //                   EI
    // wait:
    0xF0, 0x02, //             LDH A,(SC)
    0x87, //                   ADD A,A         carry = transfer running
    0x38, 0x04, //             JR C,hdma
    0x3E, 0x83, //             LD A,83
    0xE0, 0x02, //             LDH (SC),A
    // hdma:
    0xF0, 0x55, //             LDH A,(HDMA5)
    0x87, //                   ADD A,A         no carry = HDMA active
    0x30, 0xF2, //             JR NC,wait
    0xAF, //                   XOR A           restart: 0000 -> 8800 (tiles 128-255, unused)
    0xE0, 0x51, //             LDH (HDMA1),A
    0xE0, 0x52, //             LDH (HDMA2),A
    0xE0, 0x54, //             LDH (HDMA4),A
    0x3E, 0x88, //             LD A,88
    0xE0, 0x53, //             LDH (HDMA3),A
    0x3E, 0xFF, //             LD A,FF         HBlank, 128 blocks
    0xE0, 0x55, //             LDH (HDMA5),A
    0x18, 0xE1, //             JR wait
];

/// CGB palette raster effect ("hi-color") in double speed: after the speed
/// switch, with the LCD off, tile 0 gets a pattern using all four colors
/// and the BG map attributes (VRAM bank 1) select palette (column & 7), so
/// every BG palette is on screen. Then BCPS=80 (auto-increment from 0) and
/// LCD on with BG only (LCDC=91). Per line: wait for mode 0, write 32 BCPD
/// bytes (LD (C),A; INC A at 3 M-cycles each, so they run on through mode
/// 2 into mode 3 of the next line), wait for mode 3, then write BCPD, SCX,
/// BCPD mid-line. E carries the value from line to line, so the colors
/// keep changing.
const CGB_RASTER_SETUP: &[u8] = &[
    0xF3, //             DI
    0x3E, 0x01, //       LD A,01
    0xE0, 0x4D, //       LDH (KEY1),A
    0x10, 0x00, //       STOP
    0xAF, //             XOR A
    0xE0, 0x40, //       LDH (LCDC),A    LCD off: free VRAM access
    0x21, 0x00, 0x80, // LD HL,8000
    0x06, 0x10, //       LD B,16
    // tile:
    0x3E, 0x55, //       LD A,55
    0x22, //             LD (HL+),A
    0x3E, 0x33, //       LD A,33
    0x22, //             LD (HL+),A      colors 0-3 across each row
    0x05, //             DEC B
    0x20, 0xF7, //       JR NZ,tile
    0x3E, 0x01, //       LD A,01
    0xE0, 0x4F, //       LDH (VBK),A     attributes
    0x21, 0x00, 0x98, // LD HL,9800
    // attr:
    0x7D, //             LD A,L
    0xE6, 0x07, //       AND 7           palette = column & 7
    0x22, //             LD (HL+),A
    0x7C, //             LD A,H
    0xFE, 0x9C, //       CP 9C
    0x20, 0xF7, //       JR NZ,attr
    0xAF, //             XOR A
    0xE0, 0x4F, //       LDH (VBK),A
    0x3E, 0x80, //       LD A,80
    0xE0, 0x68, //       LDH (BCPS),A    auto-increment from 0
    0x0E, 0x69, //       LD C,69         BCPD
    0x3E, 0x91, //       LD A,91         LCD on, 8000 tiles, BG
    0xE0, 0x40, //       LDH (LCDC),A
];

/// The `synth:cgb-raster` code: [`CGB_RASTER_SETUP`], then the per-line
/// loop (32 unrolled BCPD writes).
fn cgb_raster() -> Vec<u8> {
    let mut code = CGB_RASTER_SETUP.to_vec();
    code.extend_from_slice(&[
        // line:
        0xF0, 0x41, //   LDH A,(STAT)
        0xE6, 0x03, //   AND 3
        0x20, 0xFA, //   JR NZ,line      until mode 0
        0x7B, //         LD A,E
    ]);
    for _ in 0..32 {
        code.extend_from_slice(&[0xE2, 0x3C]); // LD (C),A; INC A
    }
    code.extend_from_slice(&[
        0x5F, //         LD E,A
        // mode3:
        0xF0, 0x41, //   LDH A,(STAT)
        0xE6, 0x03, //   AND 3
        0xFE, 0x03, //   CP 3
        0x20, 0xF8, //   JR NZ,mode3
        0x7B, //         LD A,E
        0xE2, //         LD (C),A        BCPD in mode 3
        0xE0, 0x43, //   LDH (SCX),A     SCX in mode 3
        0x3C, //         INC A
        0xE2, //         LD (C),A        BCPD in mode 3
        0x5F, //         LD E,A
        0x18, 0xA6, //   JR line
    ]);
    code
}

/// STAT handler at 0x48 (8 bytes): PUSH AF; PUSH HL; LD HL,C000; INC (HL);
/// POP HL; POP AF; RETI.
const STAT_HANDLER: &[u8] = &[0xF5, 0xE5, 0x21, 0x00, 0xC0, 0x34, 0xE1, 0xF1, 0xD9];

/// The ROM image for `name`, or `None` if there is no such ROM.
pub fn build(name: &str) -> Option<Vec<u8>> {
    let raster = cgb_raster();
    let (code, stat_vector, timer_vector): (&[u8], &[u8], &[u8]) = match name {
        "event-storm" => (EVENT_STORM, &[], &[]),
        "dma-storm" => (DMA_STORM, &[], &[]),
        "halt-irq" => (HALT_IRQ, &[], &[0xD9]), // RETI
        "worst-case" => (WORST_CASE, STAT_HANDLER, &[]),
        "cgb-raster" => (&raster, &[], &[]),
        _ => return None,
    };
    Some(image(name, code, stat_vector, timer_vector))
}

fn image(title: &str, code: &[u8], stat_vector: &[u8], timer_vector: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x48..0x48 + stat_vector.len()].copy_from_slice(stat_vector);
    rom[0x50..0x50 + timer_vector.len()].copy_from_slice(timer_vector);
    // Entry: NOP; JP 0150.
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x104..0x134].copy_from_slice(&LOGO);
    let t = title.to_ascii_uppercase();
    let t = &t.as_bytes()[..t.len().min(15)];
    rom[0x134..0x134 + t.len()].copy_from_slice(t);
    rom[0x143] = 0xC0; // CGB only
    // 0x147 = 00 (ROM only), 0x148 = 00 (32 KiB), 0x149 = 00 (no RAM).
    rom[0x150..0x150 + code.len()].copy_from_slice(code);
    // Pan Docs: The Cartridge Header, 014D (header checksum) and 014E-014F
    // (global checksum, big-endian, over every byte except itself).
    rom[0x14D] = rom[0x134..0x14D]
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
    let sum = rom
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != 0x14E && i != 0x14F)
        .fold(0u16, |s, (_, &b)| s.wrapping_add(u16::from(b)));
    rom[0x14E..0x150].copy_from_slice(&sum.to_be_bytes());
    rom
}

#[cfg(test)]
mod tests {
    use super::*;
    use gbc_core::{CLOCK_HZ, Config, Emulator, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, WHITE};

    /// Runs `name` for `clocks` and returns `peek` of each address.
    fn run(name: &str, clocks: u64, addrs: &[u16]) -> Vec<u8> {
        let rom = build(name).unwrap();
        let mut mem = Box::new(Memory::new());
        let mut emu = Emulator::new(&mut mem);
        emu.load(&rom, &mut [], Config::default()).unwrap();
        let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        while emu.clock() < clocks {
            let _ = emu.run_until(clocks, &mut frame, &mut []);
        }
        addrs.iter().map(|&a| emu.peek(a)).collect()
    }

    #[test]
    fn headers_are_valid() {
        for name in NAMES {
            let rom = build(name).unwrap();
            let x = rom[0x134..0x14D]
                .iter()
                .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
            assert_eq!(rom[0x14D], x, "{name}");
            assert_eq!(&rom[0x104..0x134], &LOGO, "{name}");
        }
        assert!(build("nope").is_none());
    }

    #[test]
    fn event_storm_runs_in_double_speed_with_the_timer_on() {
        // KEY1 bit 7: double speed. TAC: on, 262 kHz. SC: a transfer on
        // the fast internal clock is (re)armed.
        let r = run(
            "event-storm",
            u64::from(CLOCK_HZ) / 10,
            &[0xFF4D, 0xFF07, 0xFF02],
        );
        assert_eq!(r[0] & 0x80, 0x80, "double speed");
        assert_eq!(r[1] & 0x07, 0x05, "TAC");
        assert_eq!(r[2] & 0x03, 0x03, "SC fast internal");
    }

    #[test]
    fn dma_storm_copies_to_oam_and_wram() {
        // The last DMA was started from C000.
        let r = run("dma-storm", u64::from(CLOCK_HZ) / 10, &[0xFF46]);
        assert_eq!(r[0], 0xC0);
    }

    #[test]
    fn halt_irq_idles_with_the_timer_irq_enabled() {
        let r = run(
            "halt-irq",
            u64::from(CLOCK_HZ) / 10,
            &[0xFFFF, 0xFF07, 0xFF06],
        );
        assert_eq!(r, [0x04, 0xFD, 0xF0]);
    }
    #[test]
    fn worst_case_has_ten_objects_on_a_line_stat_irqs_and_hdma_running() {
        // OAM X of object 9 (=8+6*9), STAT sources, the handler's counter.
        let r = run(
            "worst-case",
            u64::from(CLOCK_HZ) / 10,
            &[0xFE25, 0xFF41, 0xFFFF, 0xFF4D, 0xC000],
        );
        assert_eq!(r[0], 62);
        assert_eq!(r[1] & 0x78, 0x78);
        assert_eq!(r[2], 0x02);
        assert_eq!(r[3] & 0x80, 0x80, "double speed");
        assert_ne!(r[4], 0, "STAT handler ran");
        // HBlank HDMA: ROM 0104-0133 (the logo) copied to 8904, and a
        // transfer active.
        let logo: Vec<u16> = (0x8904..0x8934).collect();
        let mut addrs = logo.clone();
        addrs.push(0xFF55);
        let r = run("worst-case", u64::from(CLOCK_HZ) / 10, &addrs);
        assert_eq!(&r[..48], &LOGO);
        assert_eq!(r[48] & 0x80, 0, "HDMA active");
    }

    #[test]
    fn cgb_raster_writes_palettes_and_scx_every_line() {
        // Double speed, LCD and BG on, BCPS auto-incrementing. One line
        // (456 dots) later the loop has advanced BCPS by 34 and written a
        // new SCX.
        let at = u64::from(CLOCK_HZ) / 10;
        let a = run("cgb-raster", at, &[0xFF4D, 0xFF40, 0xFF68, 0xFF43]);
        let b = run("cgb-raster", at + 456, &[0xFF68, 0xFF43]);
        assert_eq!(a[0] & 0x80, 0x80, "double speed");
        assert_eq!(a[1], 0x91);
        assert_eq!(a[2] & 0x80, 0x80, "auto-increment");
        assert_ne!(a[2], b[0], "BCPS advanced");
        assert_ne!(a[3], b[1], "SCX written");
    }
}
