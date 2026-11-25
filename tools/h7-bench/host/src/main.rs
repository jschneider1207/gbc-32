//! Prints the report lines of the h7-bench firmware as a correct run must
//! produce them, with the cycle figures zero: the same ROMs (the bench's
//! generator, and cpu_instrs from `H7_BENCH_CPU_INSTRS`), the same frame
//! loop and the same checksum (`../../src/workload.rs`, compiled into both).
//!
//! `cargo run --release` prints them; `cargo test --release` compares them
//! with the committed `../expected-hashes.txt`.

#[path = "../../src/workload.rs"]
mod workload;

#[allow(dead_code)]
#[path = "../../../bench/src/synth.rs"]
mod synth;

use gbc_core::{Frame, Memory, StereoSample, WHITE};
use workload::{AUDIO_SLOTS, CART_RAM_SLOTS, WORKLOADS};

const CPU_INSTRS_DEFAULT: &str = "/home/jess/gbc-32/test-roms/blargg/cpu_instrs/cpu_instrs.gb";

fn rom(name: &str) -> Result<Vec<u8>, String> {
    if name == "cpu_instrs" {
        let path =
            std::env::var("H7_BENCH_CPU_INSTRS").unwrap_or_else(|_| CPU_INSTRS_DEFAULT.into());
        return std::fs::read(&path).map_err(|e| format!("{path}: {e}"));
    }
    synth::build(name).ok_or_else(|| format!("no synthetic ROM {name}"))
}

/// The report: one line per workload, each ending in "\r\n" as on the UART.
fn report() -> Result<String, String> {
    let mut mem = Box::new(Memory::new());
    let mut frame: Box<Frame> = vec![WHITE; size_of::<Frame>() / 2]
        .into_boxed_slice()
        .try_into()
        .map_err(|_| "frame size")?;
    let mut audio = Box::new([StereoSample::default(); AUDIO_SLOTS]);
    let mut cart_ram = Box::new([0u8; CART_RAM_SLOTS]);
    let mut out = String::new();
    for (i, w) in WORKLOADS.iter().enumerate() {
        let rom = rom(w.name)?;
        let r = workload::run(
            &rom,
            &mut cart_ram,
            &mut mem,
            &mut frame,
            &mut audio,
            w.calls,
            || 0,
            |_| (),
        );
        workload::report_line(&mut out, i, &r);
    }
    Ok(out)
}

fn main() -> std::process::ExitCode {
    match report() {
        Ok(s) => {
            print!("{}", s.replace('\r', ""));
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("h7-bench-host: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    /// The committed table is what this build computes. A change of the
    /// core's behavior on these ROMs, of the frame loop or of the checksum
    /// shows up here before it shows up as a mismatch on the board.
    #[test]
    fn matches_committed_table() {
        let got = super::report().expect("report").replace('\r', "");
        let want = include_str!("../../expected-hashes.txt");
        assert_eq!(
            got, want,
            "regenerate with: cargo run --release > ../expected-hashes.txt"
        );
    }
}
