//! Fuzz targets for gbc-core's untrusted inputs, driven through the public
//! API only. The property: no panic and no hang for any input.
//!
//! Every target takes raw bytes. The first [`CTRL_LEN`] bytes are control
//! bytes (ROM length and header fields, save buffer, RTC state, run length,
//! buttons; missing bytes read as 0); the rest is code placed at 0x150 of
//! the ROM. The same bytes feed the deterministic gate pass (`fuzz-pass`)
//! and the coverage-guided cargo-fuzz targets (`fuzz/`).

use gbc_core::{
    Buttons, Config, DmgPalettes, EjectedCart, Emulator, Events, Frame, Memory, RtcState, RtcTime,
    SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, cart_ram_size,
};

/// Number of control bytes at the start of an input.
pub const CTRL_LEN: usize = 64;

/// Cap on ROM size so one input stays cheap (the largest real ROM is 8 MiB;
/// bank masking is exercised well below that).
const MAX_ROM: usize = 0x40_0000;
/// Cap on the save buffer length.
const MAX_RAM: usize = 0x4_0000;

/// What a target emphasizes. They share one scenario; the profile only sets
/// run length and which header fields are forced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    /// Header and buffer handling: load, eject, re-load, short runs.
    Load,
    /// The ROM's code executes for a long time, with button input.
    Run,
    /// MBC3+timer cartridge with arbitrary RTC state.
    Rtc,
}

/// The target functions: `(name, function)`.
pub type Target = fn(&[u8]);

pub const TARGETS: [(&str, Target); 3] =
    [("load", fuzz_load), ("run", fuzz_run), ("rtc", fuzz_rtc)];

pub fn fuzz_load(data: &[u8]) {
    scenario(data, Profile::Load);
}
pub fn fuzz_run(data: &[u8]) {
    scenario(data, Profile::Run);
}
pub fn fuzz_rtc(data: &[u8]) {
    scenario(data, Profile::Rtc);
}

/// Reads bytes from an input; 0 once exhausted.
struct Reader<'d> {
    data: &'d [u8],
    pos: usize,
    /// Wrap around instead of returning 0 at the end.
    cyclic: bool,
}

impl Reader<'_> {
    fn u8(&mut self) -> u8 {
        let b = if self.cyclic && !self.data.is_empty() {
            self.data[self.pos % self.data.len()]
        } else {
            self.data.get(self.pos).copied().unwrap_or(0)
        };
        self.pos += 1;
        b
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }
    fn u32(&mut self) -> u32 {
        u32::from(self.u16()) | u32::from(self.u16()) << 16
    }
    fn u64(&mut self) -> u64 {
        u64::from(self.u32()) | u64::from(self.u32()) << 32
    }
    fn bool(&mut self) -> bool {
        self.u8() & 1 != 0
    }
}

/// ROM lengths worth trying: header edges, bank edges, odd sizes.
const ROM_LENS: [usize; 16] = [
    0, 1, 0x14F, 0x150, 0x151, 0x4000, 0x7FFF, 0x8000, 0x8001, 0xC000, 0x1_0000, 0x1_8001,
    0x2_0000, 0x5_0000, 0x10_0000, 0x20_0000,
];

fn rom_len(r: &mut Reader) -> usize {
    let mode = r.u8();
    let raw = r.u32();
    let len = match mode % 4 {
        0 | 1 => ROM_LENS[usize::from(mode >> 2) % ROM_LENS.len()],
        2 => raw as usize % 0x1_0000,
        _ => raw as usize % MAX_ROM,
    };
    len.min(MAX_ROM)
}

fn rtc_time(r: &mut Reader) -> RtcTime {
    RtcTime {
        seconds: r.u8(),
        minutes: r.u8(),
        hours: r.u8(),
        days: r.u16(),
        halted: r.bool(),
        day_carry: r.bool(),
    }
}

fn rtc_state(r: &mut Reader) -> RtcState {
    RtcState {
        live: rtc_time(r),
        latched: rtc_time(r),
        // Often out of range on purpose (set_rtc_state clamps it).
        subsecond_clocks: r.u32(),
    }
}

/// Wall-clock seconds a front end adds to a restored RTC
/// (`RtcState::advance_seconds`): none, hours, around the 512-day
/// overflow, or any `u64` (up to `u64::MAX`).
fn rtc_seconds(r: &mut Reader) -> u64 {
    let (mode, raw) = (r.u8(), r.u64());
    match mode % 4 {
        0 => 0,
        1 => raw % 0x1_0000,
        2 => raw % (2 * 512 * 86_400),
        _ => raw,
    }
}

/// Runs `total` clocks in calls of at most `chunk`, changing buttons
/// between calls. Panics if the emulator stops making progress (a hang).
fn run(
    emu: &mut Emulator<'_>,
    r: &mut Reader,
    total: u64,
    frame: &mut Frame,
    audio: &mut [StereoSample],
) {
    let end = emu.clock().saturating_add(total);
    let mut calls = 0u64;
    while emu.clock() < end {
        let before = emu.clock();
        emu.set_buttons(Buttons::from_bits_truncate(r.u8()));
        let chunk = match r.u8() % 4 {
            0 => 1,
            1 => 100,
            2 => 5_000,
            _ => 70_224,
        };
        let res = emu.run_until(end.min(before + chunk), frame, audio);
        assert!(
            res.audio_samples <= audio.len(),
            "audio_samples past the buffer"
        );
        if res.events.contains(Events::CPU_LOCKED) && r.bool() {
            // Locked CPUs are legal; keep stepping to check they idle.
            let _ = emu.step(frame, audio);
        }
        calls += 1;
        // Each call must advance the clock, except when a stop event ended
        // it immediately; allow a generous number of those.
        assert!(
            emu.clock() > before || calls < 4 * (total / 4 + 64),
            "no progress at clock {before}"
        );
        assert!(calls < 8 * (total + 64), "too many run calls");
    }
}

fn scenario(data: &[u8], profile: Profile) {
    let (ctrl, body) = data.split_at(data.len().min(CTRL_LEN));
    let mut r = Reader {
        data: ctrl,
        pos: 0,
        cyclic: false,
    };
    // Run-time choices (buttons, chunk sizes) cycle through the body.
    let mut rr = Reader {
        data: body,
        pos: 0,
        cyclic: true,
    };

    // --- ROM ---
    let len = rom_len(&mut r);
    let (cart_type, rom_size, ram_size, cgb, patch) = (r.u8(), r.u8(), r.u8(), r.u8(), r.u8());
    let cart_type = if profile == Profile::Rtc {
        [0x0F, 0x10][usize::from(cart_type & 1)]
    } else {
        cart_type
    };
    let fill = match r.u8() % 3 {
        0 => 0x00,
        1 => 0xFF,
        _ => 0xFE,
    };
    let mut rom = vec![fill; len];
    if len >= 0x150 {
        // Entry point: NOP; JP 0150. Header fields from the control bytes.
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        if patch & 1 == 0 || profile == Profile::Rtc {
            rom[0x147] = cart_type;
        }
        if patch & 2 == 0 {
            rom[0x148] = rom_size;
        }
        if patch & 4 == 0 {
            rom[0x149] = ram_size;
        }
        if patch & 8 == 0 {
            rom[0x143] = cgb;
        }
        let n = body.len().min(len - 0x150);
        rom[0x150..0x150 + n].copy_from_slice(&body[..n]);
    } else {
        let n = body.len().min(len);
        rom[..n].copy_from_slice(&body[..n]);
    }

    // --- Save buffer ---
    let needed = cart_ram_size(&rom).unwrap_or(0);
    // All fields are read unconditionally so offsets stay fixed.
    let (mode, extra, small, raw) = (r.u8(), r.u16(), r.u8(), r.u32());
    let ram_len = match mode % 4 {
        0 => needed,
        1 => needed + usize::from(extra),
        2 => needed.saturating_sub(1 + usize::from(small % 3)),
        _ => raw as usize % MAX_RAM,
    };
    let ram_fill = r.u8();
    let mut ram: Vec<u8> = (0..ram_len)
        .map(|i| match ram_fill % 3 {
            0 => 0,
            1 => 0xFF,
            _ => (i as u8).wrapping_mul(ram_fill | 1) ^ body.get(i).copied().unwrap_or(0),
        })
        .collect();

    // --- Run parameters ---
    let cycles = u64::from(r.u16()) * 4;
    let cycles = match profile {
        Profile::Load => cycles % 20_000,
        Profile::Run => cycles,
        Profile::Rtc => cycles * 8,
    };
    let rtc = rtc_state(&mut r);
    let stop = Events::from_bits_truncate(r.u16());
    let audio_len = [0, 1, 7, 512][usize::from(r.u8() % 4)];
    let rtc_secs = rtc_seconds(&mut r);

    let rom2 = vec![0u8; 0x8000];
    let mut ram2 = [0u8; 0];
    let mut spare: Option<(&[u8], &mut [u8])> = Some((&rom2, &mut ram2));
    let mut mem = Box::new(Memory::new());
    let mut frame: Box<Frame> = Box::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]);
    let mut audio = vec![StereoSample::default(); audio_len];
    let mut emu = Emulator::new(&mut mem);

    // Operations before a cartridge exists must be harmless.
    emu.set_buttons(Buttons::from_bits_truncate(0xFF));
    emu.reset();
    let _ = emu.rtc_state();
    emu.set_rtc_state(rtc.advance_seconds(rtc_secs));
    assert!(emu.eject().is_none());

    let rom: &[u8] = &rom;
    let mut ram_slot: Option<&mut [u8]> = Some(&mut ram);
    let mut rom_slot: Option<&[u8]> = Some(rom);

    for round in 0..2 {
        let (Some(rom), Some(ram)) = (rom_slot.take(), ram_slot.take()) else {
            unreachable!("buffers are returned before each round");
        };
        match emu.load(rom, ram, Config::default()) {
            Ok(prev) => assert!(prev.is_none() || round > 0),
            Err(e) => {
                let (rom, ram) = e.into_buffers();
                rom_slot = Some(rom);
                ram_slot = Some(ram);
                // The emulator must still be usable (and unloaded).
                emu.reset();
                let res = emu.run_cycles(1000, &mut frame, &mut audio);
                assert!(res.clocks <= 1000 + 64);
                continue;
            }
        }
        assert_eq!(emu.cart_ram().len(), needed);
        // A front end's restore: the saved state as read from a file, raw
        // (unmasked), advanced by the wall-clock time since the save.
        emu.set_rtc_state(rtc.advance_seconds(rtc_secs));
        emu.set_stop_events(stop);
        if r.bool() {
            emu.set_dmg_palettes(DmgPalettes::default());
        }
        run(&mut emu, &mut rr, cycles, &mut frame, &mut audio);
        // Save and restore mid-run, with time passing in between.
        if let Some(saved) = emu.rtc_state() {
            emu.set_rtc_state(saved.advance_seconds(rtc_secs));
        }
        let _ = (emu.registers(), emu.serial_byte(), emu.lcd_enabled());
        let _ = (emu.screen_blank(), emu.cpu_locked(), emu.cart_ram().len());
        let _ = (emu.cart_info(), emu.save_dirty());
        emu.clear_save_dirty();
        if profile != Profile::Load || r.bool() {
            emu.reset();
            run(&mut emu, &mut rr, cycles / 4, &mut frame, &mut audio);
        }
        // Swap cartridges: load over the loaded one (which hands back its
        // buffers), then eject to get the second cartridge's buffers back.
        if let (true, Some((rom2, ram2))) = (r.bool(), spare.take()) {
            let prev = emu.load(rom2, ram2, Config::default());
            let (prev_rom, prev_ram) = prev
                .expect("rom2 is valid")
                .expect("a cartridge was loaded")
                .into_buffers();
            rom_slot = Some(prev_rom);
            ram_slot = Some(prev_ram);
            run(&mut emu, &mut rr, 2_000, &mut frame, &mut audio);
            spare = emu.eject().map(EjectedCart::into_buffers);
        } else {
            let (rom, ram) = emu.eject().expect("a cartridge is loaded").into_buffers();
            assert_eq!(ram.len(), ram_len);
            rom_slot = Some(rom);
            ram_slot = Some(ram);
        }
        let _ = emu.run_cycles(100, &mut frame, &mut audio);
    }
}

// --- Deterministic input generation (gate pass) ---

/// SplitMix64: tiny, fixed, identical on every platform.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }
}

/// Header values that exercise the interesting branches: every cartridge
/// type the core loads, ROM/RAM size codes, and a few wild values.
const TYPES: [u8; 19] = [
    0x00, 0x01, 0x02, 0x03, 0x05, 0x06, 0x08, 0x09, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x19, 0x1A, 0x1B,
    0x1C, 0x1D, 0x1E,
];
/// Types the core refuses (`LoadErrorKind::UnsupportedMapper`, ADR 0031):
/// one per unsupported mapper and two unassigned codes. Kept a minority
/// of the generated inputs, since a refused cartridge never runs.
const REFUSED_TYPES: [u8; 9] = [0x0B, 0x0D, 0x20, 0x22, 0xFC, 0xFD, 0xFE, 0xFF, 0x16];

/// One structure-aware input for the gate pass: control bytes biased
/// towards interesting header values, then a body of random bytes mixed
/// with snippets that write mapper registers, cart RAM and I/O.
pub fn generate(rng: &mut Rng) -> Vec<u8> {
    let mut v = vec![0u8; CTRL_LEN];
    for b in &mut v {
        *b = rng.byte();
    }
    // 0: ROM length mode (bias to the interesting-length table).
    v[0] = if rng.below(4) == 0 {
        rng.byte()
    } else {
        (rng.byte() & !3) | u8::from(rng.below(2) == 0)
    };
    // 5..9: type, rom size, ram size, cgb flag.
    // 1 in 16 keeps the random byte (most values are unassigned codes), 1
    // in 8 of the rest names an unsupported mapper.
    if rng.below(16) != 0 {
        v[5] = if rng.below(8) == 0 {
            REFUSED_TYPES[rng.below(REFUSED_TYPES.len() as u64) as usize]
        } else {
            TYPES[rng.below(TYPES.len() as u64) as usize]
        };
    }
    if rng.below(2) == 0 {
        v[6] = rng.below(12) as u8;
    }
    if rng.below(2) == 0 {
        v[7] = rng.below(8) as u8;
    }
    if rng.below(2) == 0 {
        v[8] = [0x00, 0x80, 0xC0][rng.below(3) as usize];
    }
    // Patch mask: usually let the control bytes set the header.
    if rng.below(8) != 0 {
        v[9] = 0;
    }
    // RTC advance (offsets 43..52): sometimes u64::MAX seconds.
    if rng.below(8) == 0 {
        v[43] = 3;
        v[44..52].fill(0xFF);
    }
    // Cycles field (offset 20): cap some inputs short.
    if rng.below(3) == 0 {
        v[20..22].copy_from_slice(&((rng.next_u64() % 600) as u16).to_le_bytes());
    }
    let body_len = rng.below(3000) as usize;
    let mut i = 0;
    while i < body_len {
        match rng.below(10) {
            // LD A,n; LD (nn),A  with nn in ROM (mapper regs), cart RAM or I/O.
            0..=4 => {
                let addr: u16 = match rng.below(4) {
                    0 => rng.below(0x8000) as u16,
                    1 => 0xA000 + rng.below(0x2000) as u16,
                    2 => 0xFF00 + rng.below(0x100) as u16,
                    _ => rng.next_u64() as u16,
                };
                v.extend_from_slice(&[0x3E, rng.byte(), 0xEA, addr as u8, (addr >> 8) as u8]);
                i += 5;
            }
            // LD A,(nn)
            5 | 6 => {
                let addr = rng.next_u64() as u16;
                v.extend_from_slice(&[0xFA, addr as u8, (addr >> 8) as u8]);
                i += 3;
            }
            // EI / HALT / STOP / LD (HL+),A style noise
            7 => {
                v.push([0xFB, 0x76, 0x10, 0x22, 0x08, 0xE8, 0xF8][rng.below(7) as usize]);
                i += 1;
            }
            _ => {
                v.push(rng.byte());
                i += 1;
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let a: Vec<_> = (0..8).map(|_| generate(&mut Rng(42))).collect();
        assert!(a.windows(2).all(|w| w[0] == w[1]));
        let mut rng = Rng(42);
        assert_ne!(generate(&mut rng), generate(&mut rng));
    }

    #[test]
    fn targets_survive_a_few_inputs() {
        let mut rng = Rng(1);
        for _ in 0..6 {
            let input = generate(&mut rng);
            for (_, f) in TARGETS {
                f(&input);
            }
        }
        for (_, f) in TARGETS {
            f(&[]);
            f(&[0xFF; 200]);
        }
    }
}
