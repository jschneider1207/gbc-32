//! Synthetic benchmark ROMs, hand-assembled SM83 built in memory (no ROM
//! files in git). Each one stresses a path the test-ROM benchmarks miss
//! (docs/perf.md). All are 32 KiB, no MBC, CGB-only, with a valid header
//! and checksums; execution starts at 0x0100 as the CGB boot ROM leaves it.

/// The names `bench synth:<name>` accepts.
pub const NAMES: &[&str] = &["event-storm", "dma-storm", "halt-irq"];

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

/// The ROM image for `name`, or `None` if there is no such ROM.
pub fn build(name: &str) -> Option<Vec<u8>> {
    let (code, timer_vector): (&[u8], &[u8]) = match name {
        "event-storm" => (EVENT_STORM, &[]),
        "dma-storm" => (DMA_STORM, &[]),
        "halt-irq" => (HALT_IRQ, &[0xD9]), // RETI
        _ => return None,
    };
    Some(image(name, code, timer_vector))
}

fn image(title: &str, code: &[u8], timer_vector: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
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
}
