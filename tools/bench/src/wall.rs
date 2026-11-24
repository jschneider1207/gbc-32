//! Wall-clock benchmark: `bench --wall` (docs/perf.md, "Wall-clock
//! benchmark"). Not part of the gate: wall time is noisy. It is the tool,
//! not the core, so reading the host clock here is fine.
//!
//! Each workload is run from power-on for a fixed number of emulated frames
//! with the same call pattern as `run_emulation`. One warm-up run is
//! discarded, then the median of `--runs` timed runs is reported.
//! Host cycles per frame = ns per frame x `--ghz` (a nominal clock, not a
//! counter: turbo and the TSC rate make a counter no more exact).

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use gbc_core::{
    Config, DmgPalettes, Emulator, FRAME_CLOCKS, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH,
    StereoSample, WHITE, cart_ram_size,
};

use crate::{AUDIO_SLOTS, run_emulation, synth};

/// Fixed workloads: (label, source, what it stresses). Sources are
/// `synth:<name>` or a path under the test-ROM root.
const WORKLOADS: &[(&str, &str, &str)] = &[
    ("cpu_instrs", "blargg/cpu_instrs/cpu_instrs.gb", "CPU"),
    ("cgb_sound", "blargg/cgb_sound/cgb_sound.gb", "APU"),
    ("cgb-acid2", "cgb-acid2/cgb-acid2.gbc", "PPU, halted idle"),
    (
        "cgb-acid-hell",
        "cgb-acid-hell/cgb-acid-hell.gbc",
        "PPU, objects",
    ),
    (
        "hdma_timing-C",
        "mealybug-tearoom-tests/dma/hdma_timing-C.gb",
        "HDMA",
    ),
    ("hdma_mode0", "same-suite/dma/hdma_mode0.gb", "HDMA"),
    ("synth:worst-case", "synth:worst-case", "all paths at once"),
    ("synth:dma-storm", "synth:dma-storm", "DMA"),
    (
        "synth:cgb-raster",
        "synth:cgb-raster",
        "mid-frame PPU writes",
    ),
    (
        "synth:apu-writes",
        "synth:apu-writes",
        "APU register writes",
    ),
    ("synth:dmg-raster", "synth:dmg-raster", "DMG render path"),
];

/// Median of a non-empty slice (mean of the middle two when even).
pub fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

fn load(src: &str, root: &Path) -> Result<Vec<u8>, String> {
    if let Some(name) = src.strip_prefix("synth:") {
        return synth::build(name).ok_or_else(|| format!("unknown synthetic ROM {name}"));
    }
    let p = root.join(src);
    std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
}

/// One timed run from power-on: nanoseconds for `frames` emulated frames.
fn time_run(rom: &[u8], frames: u64) -> Result<f64, String> {
    let mut cart_ram = vec![0u8; cart_ram_size(rom).map_err(|e| format!("{e:?}"))?];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    let config = Config {
        dmg_palettes: DmgPalettes::default(),
    };
    emu.load(rom, &mut cart_ram, config)
        .map_err(|e| e.to_string())?;
    let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size");
    let mut audio = vec![StereoSample::default(); AUDIO_SLOTS];
    let target = frames * u64::from(FRAME_CLOCKS);
    let t = Instant::now();
    run_emulation(&mut emu, target, &mut frame, &mut audio);
    let ns = t.elapsed().as_nanos() as f64;
    if emu.clock() < target {
        return Err("CPU locked before the target".into());
    }
    Ok(ns)
}

/// `bench --wall [--frames N] [--runs N] [--ghz F] [--root DIR] [--rom PATH]...`
pub fn main(args: &[String]) -> ExitCode {
    let (mut frames, mut runs, mut ghz) = (1200u64, 7usize, 4.0f64);
    let mut root = "test-roms".to_string();
    let mut extra: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().map(String::as_str).unwrap_or("");
        match a.as_str() {
            "--frames" if v.parse::<u64>().is_ok_and(|n| n >= 1) => frames = v.parse().unwrap(),
            "--runs" if v.parse::<usize>().is_ok_and(|n| n >= 1) => runs = v.parse().unwrap(),
            "--ghz" if v.parse::<f64>().is_ok_and(|n| n > 0.0) => ghz = v.parse().unwrap(),
            "--root" => root = v.to_string(),
            "--rom" => extra.push(v.to_string()),
            _ => {
                eprintln!(
                    "usage: bench --wall [--frames N] [--runs N] [--ghz F] [--root DIR] [--rom PATH]..."
                );
                return ExitCode::from(2);
            }
        }
    }
    println!("frames/run {frames}, runs {runs} (median, after 1 warm-up), nominal {ghz} GHz");
    println!("| workload | stresses | frames/s | ns/frame | Mcycles/frame | min..max ns/frame |");
    println!("|---|---|---:|---:|---:|---|");
    let mut list: Vec<(String, String, String)> = WORKLOADS
        .iter()
        .map(|(l, s, w)| (l.to_string(), s.to_string(), w.to_string()))
        .collect();
    for p in &extra {
        let label = Path::new(p)
            .file_name()
            .map_or(p.clone(), |f| f.to_string_lossy().into());
        list.push((label, p.clone(), "extra ROM".into()));
    }
    let mut failed = false;
    for (label, src, what) in &list {
        let rom = match load(src, Path::new(&root)).or_else(|e| {
            // Extra ROMs are plain paths, not under the root.
            std::fs::read(src).map_err(|_| e)
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("bench: {label}: {e}");
                failed = true;
                continue;
            }
        };
        let mut samples = Vec::new();
        for i in 0..=runs {
            match time_run(&rom, frames) {
                Ok(ns) if i > 0 => samples.push(ns / frames as f64),
                Ok(_) => {}
                Err(e) => {
                    eprintln!("bench: {label}: {e}");
                    failed = true;
                    break;
                }
            }
        }
        if samples.is_empty() {
            continue;
        }
        let m = median(&samples);
        let lo = samples.iter().copied().fold(f64::MAX, f64::min);
        let hi = samples.iter().copied().fold(0.0, f64::max);
        println!(
            "| {label} | {what} | {:.0} | {m:.0} | {:.3} | {lo:.0}..{hi:.0} |",
            1e9 / m,
            m * ghz / 1e6
        );
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::median;

    #[test]
    fn median_odd_even_unsorted() {
        assert_eq!(median(&[5.0, 1.0, 3.0]), 3.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 10.0]), 3.5);
        assert_eq!(median(&[7.0]), 7.0);
    }
}
