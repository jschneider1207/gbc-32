//! The deterministic fuzz pass the gate runs (scripts/check-fuzz.sh).
//!
//! `fuzz-pass [--seed N] [--count N] [--corpus DIR] [--known-failures DIR]`
//! runs every target over the committed corpus, then over `count` inputs
//! generated from `seed`. Same arguments, same inputs, same result. A panic
//! writes the input to `target/fuzz-failures/` and exits 1.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use gbc_fuzz::{CTRL_LEN, Rng, TARGETS, generate, huc3_command};

const DEFAULT_SEED: u64 = 0x0006_BC32_F022;
const DEFAULT_COUNT: u64 = 5000;

fn fail(name: &str, origin: &str, input: &[u8]) -> ExitCode {
    let dir = PathBuf::from("target/fuzz-failures");
    let file = dir.join(format!("{name}-{}", origin.replace(['/', ' '], "_")));
    let _ = std::fs::create_dir_all(&dir);
    let saved = std::fs::write(&file, input).map(|()| file.display().to_string());
    eprintln!(
        "FUZZ FAILURE: target {name}, input {origin} ({} bytes), saved to {saved:?}",
        input.len()
    );
    ExitCode::from(1)
}

fn run_one(f: fn(&[u8]), input: &[u8]) -> bool {
    panic::catch_unwind(AssertUnwindSafe(|| f(input))).is_ok()
}

fn main() -> ExitCode {
    let mut seed = DEFAULT_SEED;
    let mut count = DEFAULT_COUNT;
    let mut corpus = PathBuf::from("tools/fuzz/corpus");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next();
        match (a.as_str(), v) {
            ("--seed", Some(v)) => seed = v.parse().expect("--seed N"),
            ("--count", Some(v)) => count = v.parse().expect("--count N"),
            ("--corpus", Some(v)) => corpus = PathBuf::from(v),
            ("--emit-corpus", Some(v)) => return emit_corpus(Path::new(&v)),
            _ => {
                eprintln!("usage: fuzz-pass [--seed N] [--count N] [--corpus DIR]");
                return ExitCode::from(2);
            }
        }
    }
    let start = Instant::now();
    let mut inputs = 0u64;
    let mut digest = 0xCBF2_9CE4_8422_2325u64;

    for (name, f) in TARGETS {
        let dir = corpus.join(name);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.sort();
        for path in &files {
            let input = std::fs::read(path).expect("read corpus file");
            inputs += 1;
            if !run_one(f, &input) {
                return fail(name, &format!("corpus-{}", file_name(path)), &input);
            }
        }
        let mut rng = Rng(seed ^ fnv(name.as_bytes()));
        for i in 0..count {
            let input = generate(&mut rng);
            for &b in &input {
                digest = (digest ^ u64::from(b)).wrapping_mul(0x100_0000_01B3);
            }
            inputs += 1;
            if !run_one(f, &input) {
                return fail(name, &format!("seed{seed}-index{i}"), &input);
            }
        }
    }
    println!(
        "fuzz pass OK: {} targets, {inputs} inputs (seed {seed}, input digest {digest:016x}) in {:.1?}",
        TARGETS.len(),
        start.elapsed()
    );
    ExitCode::SUCCESS
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xCBF2_9CE4_8422_2325, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x100_0000_01B3)
    })
}

/// Writes the hand-made edge-case inputs (every ROM length class crossed
/// with the cartridge types, all-0xFF control bytes, a CPU that never wakes)
/// to `dir/<target>/`. Run once to (re)create `tools/fuzz/corpus`.
fn emit_corpus(dir: &Path) -> ExitCode {
    let types = [0x00u8, 0x01, 0x06, 0x0F, 0x10, 0x1B, 0x20, 0xFF];
    for (name, _) in TARGETS {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).expect("create corpus dir");
        for len_idx in 0..16u8 {
            let mut v = vec![0u8; CTRL_LEN];
            v[0] = len_idx << 2; // mode 0: the ROM_LENS table
            v[5] = types[usize::from(len_idx) % types.len()];
            v[6] = len_idx; // ROM size code
            v[7] = 1 + len_idx % 5; // RAM size code
            v[20..22].copy_from_slice(&2000u16.to_le_bytes());
            // LD A,1; LD (6000),A; LD A,0A; LD (A000),A; JR -2
            v.extend_from_slice(&[
                0x3E, 1, 0xEA, 0, 0x60, 0x3E, 0x0A, 0xEA, 0, 0xA0, 0x18, 0xFE,
            ]);
            write(&d, &format!("len{len_idx:02}"), &v);
        }
        let mut ff = vec![0xFFu8; CTRL_LEN];
        ff.extend_from_slice(&[0xFF; 64]);
        write(&d, "all-ff", &ff);
        // DI; HALT: a CPU that never wakes.
        let mut halt = vec![0u8; CTRL_LEN];
        halt[0] = 1; // table entry 0: an empty ROM
        halt.extend_from_slice(&[0xF3, 0x76]);
        write(&d, "halt", &halt);
        write(&d, "empty", &[]);
        // HuC3 (type FE), a hostile clock state (all control bytes FF from
        // the RTC fields on) and clock commands: set the access address
        // to FE, read and write across the FF -> 00 wrap, copy the time
        // out and in, then every extended command.
        let mut huc3 = vec![0xFFu8; CTRL_LEN];
        huc3[..10].copy_from_slice(&[0x1C, 0, 0, 0, 0, 0xFE, 1, 3, 0x80, 0]);
        huc3[10..22].fill(0); // save buffer: exactly the header's size
        huc3[20..22].copy_from_slice(&4000u16.to_le_bytes());
        let commands = [0x4E, 0x5F, 0x10, 0x3A, 0x3B, 0x2C, 0x60, 0x61]
            .into_iter()
            .chain(0x60..=0x6F)
            .chain([0x00, 0x7F, 0xFF]);
        for c in commands {
            huc3.extend_from_slice(&huc3_command(c));
        }
        huc3.extend_from_slice(&[0x18, 0xFE]); // JR -2
        write(&d, "huc3-clock", &huc3);
    }
    ExitCode::SUCCESS
}

fn write(dir: &Path, name: &str, data: &[u8]) {
    std::fs::write(dir.join(name), data).expect("write corpus file");
}
