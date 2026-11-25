//! The measured frame loop and its checksum, shared by the firmware
//! (`main.rs`) and the host command that prints the expected values
//! (`host/src/main.rs` includes this file with `#[path]`), so both run the
//! same calls and fold the same bytes.
//!
//! Calls are made as `tools/bench` makes them (`run_emulation` there): at
//! most one frame ([`FRAME_CLOCKS`]) per `run_until` call, each with an
//! [`AUDIO_SLOTS`]-sample audio buffer.

use core::fmt::Write;

use gbc_core::{
    Config, DmgPalettes, Emulator, Events, FRAME_CLOCKS, Frame, Memory, StereoSample, WHITE,
    cart_ram_size,
};

/// As `tools/bench`: one frame is 548.6 samples, so a frame-sized call
/// never fills the buffer and `AUDIO_BUFFER_FULL` never splits a call.
pub const AUDIO_SLOTS: usize = 1024;

/// Cartridge RAM buffer size: enough for every workload (the synthetic ROMs
/// have none, cpu_instrs is MBC1 without RAM); `run` fails on a ROM that
/// needs more.
pub const CART_RAM_SLOTS: usize = 0x2000;

/// A workload: its name on the report line, its ROM file under `OUT_DIR`
/// (firmware) or how the host finds it, and the number of `run_until` calls.
pub struct Workload {
    pub name: &'static str,
    pub calls: u32,
}

/// The workloads, in report order. The synthetic ROMs come from
/// `tools/bench/src/synth.rs` (`synth:<name>`), cpu_instrs from test-roms/.
pub const WORKLOADS: [Workload; 15] = [
    Workload {
        name: "worst-case",
        calls: 120,
    },
    Workload {
        name: "cgb-raster",
        calls: 120,
    },
    Workload {
        name: "dmg-raster",
        calls: 120,
    },
    Workload {
        name: "event-storm",
        calls: 120,
    },
    Workload {
        name: "cpu_instrs",
        calls: 600,
    },
    Workload {
        name: "dma-storm",
        calls: 120,
    },
    Workload {
        name: "halt-irq",
        calls: 120,
    },
    Workload {
        name: "apu-writes",
        calls: 120,
    },
    Workload {
        name: "oam-lines",
        calls: 120,
    },
    Workload {
        name: "storm-control",
        calls: 120,
    },
    Workload {
        name: "storm-apu",
        calls: 120,
    },
    Workload {
        name: "storm-spsw",
        calls: 120,
    },
    Workload {
        name: "storm-div",
        calls: 120,
    },
    Workload {
        name: "storm-gdma",
        calls: 120,
    },
    Workload {
        name: "storm-ppu",
        calls: 120,
    },
];

/// FNV-1a, 64 bit: the same folding as `Fnv` in
/// `tools/bench/src/fingerprint.rs` (bytes, little-endian words, and a
/// frame four pixels per step).
pub struct Fnv(pub u64);

impl Fnv {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, bs: &[u8]) {
        for &b in bs {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(Self::PRIME);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn frame(&mut self, f: &Frame) {
        for px in f.as_chunks::<4>().0 {
            let w = u64::from(px[0])
                | u64::from(px[1]) << 16
                | u64::from(px[2]) << 32
                | u64::from(px[3]) << 48;
            self.0 = (self.0 ^ w).wrapping_mul(Self::PRIME);
        }
    }
}

/// What one workload did. Cycle figures are per `run_until` call, in the
/// unit of the `now` counter passed to [`run`] (CPU cycles on the MCU).
#[derive(Clone, Copy)]
pub struct Stats {
    /// `run_until` calls made (fewer than asked if the CPU locked up).
    pub calls: u32,
    /// Calls that reported `FRAME_READY`.
    pub frames: u32,
    /// Emulated base clocks.
    pub clocks: u64,
    pub min: u32,
    pub max: u32,
    pub sum: u64,
    /// Checksum of every call's clocks, events and audio samples, of every
    /// completed frame, and of the final frame buffer.
    pub hash: u64,
}

/// Why a workload did not run.
#[derive(Clone, Copy)]
pub enum Error {
    /// The header is bad or names a mapper the core does not emulate.
    Header,
    /// The ROM needs more than [`CART_RAM_SLOTS`] bytes of cartridge RAM.
    CartRam,
    /// `Emulator::load` refused the ROM.
    Load,
}

/// Loads `rom` into a fresh emulator on `mem` and makes `calls` frame-sized
/// `run_until` calls. `now` reads a free-running 32-bit cycle counter; it is
/// read right before and right after each call, so hashing and `between`
/// (called after each call with its index, e.g. to blink an LED) are not
/// counted.
// Never inlined: the firmware's `.text` layout around the call sites then
// does not depend on how many workloads there are.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn run(
    rom: &[u8],
    cart_ram: &mut [u8; CART_RAM_SLOTS],
    mem: &mut Memory,
    frame: &mut Frame,
    audio: &mut [StereoSample; AUDIO_SLOTS],
    calls: u32,
    mut now: impl FnMut() -> u32,
    mut between: impl FnMut(u32),
) -> Result<Stats, Error> {
    let need = cart_ram_size(rom).map_err(|_| Error::Header)?;
    if need > cart_ram.len() {
        return Err(Error::CartRam);
    }
    cart_ram.fill(0);
    frame.fill(WHITE);
    audio.fill(StereoSample::default());
    let mut emu = Emulator::new(mem);
    let config = Config {
        dmg_palettes: DmgPalettes::default(),
    };
    if emu.load(rom, cart_ram, config).is_err() {
        return Err(Error::Load);
    }
    let mut h = Fnv::new();
    let mut s = Stats {
        calls: 0,
        frames: 0,
        clocks: 0,
        min: u32::MAX,
        max: 0,
        sum: 0,
        hash: 0,
    };
    for i in 0..calls {
        let to = emu.clock() + u64::from(FRAME_CLOCKS);
        let t0 = now();
        let r = emu.run_until(to, frame, audio);
        let dt = now().wrapping_sub(t0);
        s.calls += 1;
        s.clocks += r.clocks;
        s.min = s.min.min(dt);
        s.max = s.max.max(dt);
        s.sum += u64::from(dt);
        h.u64(r.clocks);
        h.bytes(&r.events.bits().to_le_bytes());
        h.u64(r.audio_samples as u64);
        for a in &audio[..r.audio_samples] {
            h.bytes(&a.left.to_le_bytes());
            h.bytes(&a.right.to_le_bytes());
        }
        if r.events.contains(Events::FRAME_READY) {
            s.frames += 1;
            h.frame(frame);
        }
        between(i);
        if r.events.contains(Events::CPU_LOCKED) {
            break;
        }
    }
    h.u64(emu.clock());
    h.frame(frame);
    s.hash = h.0;
    Ok(s)
}

/// One report line for workload `i`. The host prints the same line with
/// zero cycle figures (`host/`), so the `calls`, `frames`, `clocks` and
/// `hash` fields can be compared as text.
pub fn report_line(out: &mut impl Write, i: usize, r: &Result<Stats, Error>) {
    let name = WORKLOADS[i].name;
    // A write error has nowhere to go (the UART writer never fails).
    let _ = match r {
        Ok(s) => writeln!(
            out,
            "h7bench v1 {name} calls={} frames={} clocks={} min={} mean={} max={} hash={:016x}\r",
            s.calls,
            s.frames,
            s.clocks,
            s.min,
            s.sum / u64::from(s.calls.max(1)),
            s.max,
            s.hash
        ),
        Err(e) => writeln!(
            out,
            "h7bench v1 {name} ERROR {}\r",
            match e {
                Error::Header => "bad ROM header",
                Error::CartRam => "cartridge RAM buffer too small",
                Error::Load => "load refused",
            }
        ),
    };
}
