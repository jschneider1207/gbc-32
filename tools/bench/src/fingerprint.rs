//! Behavior fingerprint (docs/perf.md, ADR 0011 "Verification").
//!
//! Runs a fixed corpus of test ROMs for N emulated seconds and prints one
//! hash per ROM. Two builds that print the same lines behave identically on
//! everything the hash covers, so a restructuring of the core's timing can be
//! checked against its parent commit (`scripts/diff-fingerprint.sh`).
//!
//! Per ROM: the emulator is loaded as romtest does (the only model is CGB C,
//! ADR 0003, with the default palettes), then run in 456-clock `run_until`
//! chunks with `LD_B_B` and `SERIAL_TX` stops enabled. After each chunk the
//! hash folds: the clock, the chunk's events, the CPU registers, the
//! side-effect-free `peek` of SB SC DIV TIMA TMA TAC IF LY, and the byte that
//! `SERIAL_TX` reported. At the end it folds WRAM (`peek` C000-DFFF: fixed
//! bank 0 and the currently mapped bank; `peek` cannot reach the other CGB
//! banks without writing SVBK).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use gbc_core::{
    CLOCK_HZ, Config, DmgPalettes, Emulator, Events, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH,
    WHITE, cart_ram_size,
};

/// Directories (relative to the root) searched recursively for `.gb`/`.gbc`.
const CORPUS: &[&str] = &[
    "mooneye-test-suite/acceptance",
    "mooneye-test-suite/misc",
    "mooneye-test-suite/emulator-only",
    "blargg",
    "age-test-roms",
    "same-suite",
    "cgb-acid2",
];

const CHUNK_CLOCKS: u64 = 456;
/// SB SC DIV TIMA TMA TAC IF LY.
const IO_REGS: [u16; 8] = [
    0xFF01, 0xFF02, 0xFF04, 0xFF05, 0xFF06, 0xFF07, 0xFF0F, 0xFF44,
];

/// FNV-1a, 64 bit.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn byte(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    fn bytes(&mut self, bs: &[u8]) {
        for &b in bs {
            self.byte(b);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("gb" | "gbc")) {
            out.push(p);
        }
    }
}

/// `Err` if the ROM fails to read or load.
fn fingerprint(path: &Path, secs: u64) -> Result<u64, String> {
    let rom = std::fs::read(path).map_err(|e| e.to_string())?;
    let ram_size = cart_ram_size(&rom).map_err(|e| format!("{e:?}"))?;
    let mut cart_ram = vec![0u8; ram_size];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    let config = Config {
        dmg_palettes: DmgPalettes::default(),
    };
    emu.load(&rom, &mut cart_ram, config)
        .map_err(|e| e.to_string())?;
    emu.set_stop_events(Events::LD_B_B | Events::SERIAL_TX);
    let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size");

    let mut h = Fnv::new();
    let end = secs * u64::from(CLOCK_HZ);
    while emu.clock() < end {
        let r = emu.run_until(emu.clock() + CHUNK_CLOCKS, &mut frame, &mut []);
        h.u64(emu.clock());
        h.u64(r.clocks);
        h.bytes(&r.events.bits().to_le_bytes());
        if r.events.contains(Events::SERIAL_TX) {
            h.byte(emu.serial_byte());
        }
        let g = emu.registers();
        h.bytes(&[g.a, g.f, g.b, g.c, g.d, g.e, g.h, g.l]);
        h.bytes(&g.sp.to_le_bytes());
        h.bytes(&g.pc.to_le_bytes());
        for a in IO_REGS {
            h.byte(emu.peek(a));
        }
    }
    for a in 0xC000..=0xDFFFu16 {
        h.byte(emu.peek(a));
    }
    Ok(h.0)
}

pub fn run(root: &Path, secs: u64) -> std::process::ExitCode {
    let mut roms = Vec::new();
    for d in CORPUS {
        collect(&root.join(d), &mut roms);
    }
    roms.sort();
    if roms.is_empty() {
        eprintln!(
            "bench: no ROMs under {} (run scripts/fetch-test-roms.sh)",
            root.display()
        );
        return std::process::ExitCode::from(2);
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<u64, String>>>> = Mutex::new(vec![None; roms.len()]);
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = roms.get(i) else { break };
                    let r = fingerprint(p, secs);
                    results.lock().expect("lock")[i] = Some(r);
                }
            });
        }
    });
    let results = results.into_inner().expect("lock");
    for (p, r) in roms.iter().zip(results) {
        let rel = p.strip_prefix(root).unwrap_or(p).display();
        match r {
            Some(Ok(h)) => println!("{h:016x} {rel}"),
            _ => println!("LOAD-ERROR {rel}"),
        }
    }
    std::process::ExitCode::SUCCESS
}
