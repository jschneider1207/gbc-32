//! Prints the report lines of the h7-bench firmware as a correct run must
//! produce them, with the cycle figures zero: the same ROMs (the bench's
//! generator, and cpu_instrs from `H7_BENCH_CPU_INSTRS`), the same frame
//! loop and the same checksum (`../../src/workload.rs`, compiled into both).
//!
//! `cargo run --release` prints them; `cargo test --release` compares them
//! with the committed `../expected-hashes.txt`.
//!
//! `cargo run --release -- pack <file>` writes the bench pack (`../../src/pack.rs`):
//! the same ROMs in one blob for the QSPI NOR (build.sh: `out/h753/bench-pack.bin`).

#[path = "../../src/pack.rs"]
mod pack;
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

/// The bench pack (`../../src/pack.rs`): every workload ROM, in report
/// order, each at the next multiple of `pack::ALIGN` after the table or
/// the previous ROM, 0xFF in the gaps (the NOR's erased value).
fn build_pack() -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(&pack::MAGIC);
    out.extend_from_slice(&(WORKLOADS.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let roms = WORKLOADS
        .iter()
        .map(|w| rom(w.name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut at = pack::TABLE_BYTES;
    for (w, r) in WORKLOADS.iter().zip(&roms) {
        at = at.next_multiple_of(pack::ALIGN);
        let name = pack::name_field(w.name).ok_or_else(|| format!("{}: name too long", w.name))?;
        out.extend_from_slice(&name);
        out.extend_from_slice(
            &u32::try_from(at)
                .map_err(|_| "pack too large")?
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u32::try_from(r.len())
                .map_err(|_| "ROM too large")?
                .to_le_bytes(),
        );
        at += r.len();
    }
    for r in &roms {
        out.resize(out.len().next_multiple_of(pack::ALIGN), 0xFF);
        out.extend_from_slice(r);
    }
    // What the firmware will see: the same ROMs back, or no pack.
    let back = pack::parse(&out).map_err(|e| format!("the pack does not parse: {e:?}"))?;
    if back.iter().zip(&roms).any(|(a, b)| *a != b.as_slice()) {
        return Err("the pack parses to other ROMs".into());
    }
    Ok(out)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => report().map(|s| print!("{}", s.replace('\r', ""))),
        [cmd, path] if cmd == "pack" => build_pack().and_then(|p| {
            std::fs::write(path, &p).map_err(|e| format!("{path}: {e}"))?;
            println!("h7-bench-host: wrote {path} ({} bytes)", p.len());
            Ok(())
        }),
        _ => Err("usage: h7-bench-host [pack <file>]".into()),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("h7-bench-host: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pack::{self, PackError};
    use super::workload::WORKLOADS;

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

    /// The pack the host writes parses back into exactly the ROMs the
    /// report runs, in order, each aligned.
    #[test]
    fn pack_round_trips() {
        let p = super::build_pack().expect("pack");
        let roms = pack::parse(&p).expect("parse");
        for (w, r) in WORKLOADS.iter().zip(roms) {
            assert_eq!(r, super::rom(w.name).expect("rom").as_slice(), "{}", w.name);
            let off = r.as_ptr() as usize - p.as_ptr() as usize;
            assert_eq!(off % pack::ALIGN, 0, "{}", w.name);
        }
    }

    /// Entry `i`'s field at `at` (0: name, 24: offset, 28: length) set to `v`.
    fn poke(p: &mut [u8], i: usize, at: usize, v: &[u8]) {
        let e = pack::FIXED_BYTES + i * pack::ENTRY_BYTES + at;
        p[e..e + v.len()].copy_from_slice(v);
    }

    /// Each malformed shape is refused with its reason.
    #[test]
    fn pack_refuses_bad_shapes() {
        let good = super::build_pack().expect("pack");
        let off = |p: &[u8], i: usize| {
            let e = pack::FIXED_BYTES + i * pack::ENTRY_BYTES + pack::NAME_BYTES;
            u32::from_le_bytes(p[e..e + 4].try_into().unwrap())
        };
        let mut p = good.clone();
        p[0] ^= 1;
        assert_eq!(pack::parse(&p), Err(PackError::BadMagic));
        let mut p = good.clone();
        p[8] += 1;
        assert_eq!(pack::parse(&p), Err(PackError::Count));
        assert_eq!(
            pack::parse(&good[..pack::TABLE_BYTES - 1]),
            Err(PackError::Short)
        );
        // Two workloads swapped: the names must be in report order.
        let mut p = good.clone();
        poke(&mut p, 1, 0, &pack::name_field(WORKLOADS[2].name).unwrap());
        assert_eq!(pack::parse(&p), Err(PackError::Name(1)));
        // A name that only starts like the workload's.
        let mut p = good.clone();
        poke(&mut p, 0, WORKLOADS[0].name.len(), b"x");
        assert_eq!(pack::parse(&p), Err(PackError::Name(0)));
        // Misaligned, inside the table, overlapping the previous ROM.
        let mut p = good.clone();
        poke(
            &mut p,
            3,
            pack::NAME_BYTES,
            &(off(&good, 3) + 1).to_le_bytes(),
        );
        assert_eq!(pack::parse(&p), Err(PackError::Offset(3)));
        let mut p = good.clone();
        poke(&mut p, 0, pack::NAME_BYTES, &0u32.to_le_bytes());
        assert_eq!(pack::parse(&p), Err(PackError::Offset(0)));
        let mut p = good.clone();
        poke(&mut p, 5, pack::NAME_BYTES, &off(&good, 4).to_le_bytes());
        assert_eq!(pack::parse(&p), Err(PackError::Offset(5)));
        // Empty, and running past the end.
        let mut p = good.clone();
        poke(&mut p, 7, pack::NAME_BYTES + 4, &0u32.to_le_bytes());
        assert_eq!(pack::parse(&p), Err(PackError::Length(7)));
        let last = WORKLOADS.len() - 1;
        assert_eq!(
            pack::parse(&good[..good.len() - 1]),
            Err(PackError::Length(last))
        );
    }
}
