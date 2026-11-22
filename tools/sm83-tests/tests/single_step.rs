//! Runs every SingleStepTests/sm83 case (`test-data/sm83/v1/*.json`)
//! against `gbc_core::sm83::Cpu` on a flat 64 KiB RAM bus, checking the
//! final registers, IME/EI state and memory, and the bus activity of every
//! M-cycle (address, data and read/write for accesses; an internal cycle
//! where the test has none).
//!
//! Data integrity: every file is hashed (SHA-256, from the exact bytes
//! that are parsed) and checked against the committed list
//! `tools/sm83-tests/sm83-v1.sha256`, written by
//! `scripts/fetch-sm83-tests.sh` from the hash-checked tarball. A changed,
//! missing or extra file fails the test. If the data directory is absent
//! the test prints SKIPPED and passes, unless `GBC_REQUIRE_TEST_DATA=1`
//! is set, in which case it fails (for gates that must not skip).
//!
//! Format notes (from the suite's README and data): each case starts with
//! PC at the opcode, and its `cycles` list starts with the opcode fetch.
//! `ie` in the state is to be ignored. The final state has `"ei": 1` when
//! EI was just executed (IME pending). Cases are run by stepping the CPU
//! until it has used as many M-cycles as the case lists (HALT and STOP
//! cases list idle cycles after the instruction).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gbc_core::sm83::{Bus, Cpu, Registers};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// `sha256sum` output for `test-data/sm83/v1/*.json`: "<hex>  <file name>".
const HASHES: &str = include_str!("../sm83-v1.sha256");

/// Set to 1 to fail instead of skipping when the data is missing.
const REQUIRE_ENV: &str = "GBC_REQUIRE_TEST_DATA";

/// STOP (opcode 10) is two bytes on hardware when no interrupt is pending
/// (IE & IF & 1F = 0): its second byte is read before the pause, so PC ends
/// at +2 and the bus shows a read of pc+1 where SingleStepTests expects an
/// idle cycle (SameBoy `Core/sm83_cpu.c` `stop()`, which encodes the Pan
/// Docs "Using the STOP instruction" flowchart). SingleStepTests expects one
/// byte in every case, and none of its 10 cases sets IE or IF, so all 1000
/// have nothing pending. The human's decision (2026-11-22; ADR 0020): run
/// all 1000 and compare everything except the two things the length rule
/// changes by definition, PC and the bus-cycle list. Registers A-L, F, SP,
/// IME, EI and the final RAM are still compared. The exemption is pinned
/// (file and case count) so it can't silently grow to other opcodes.
const STOP_FILE: &str = "10";
const STOP_CASES: usize = 1000;

/// One M-cycle as listed: address pins, data pins, "r-m" / "-wm" / "---".
type RawCycle = (Option<u16>, Option<u8>, String);

#[derive(Deserialize)]
struct Case {
    name: String,
    initial: State,
    #[serde(rename = "final")]
    end: State,
    cycles: Vec<Option<RawCycle>>,
}

#[derive(Deserialize)]
struct State {
    pc: u16,
    sp: u16,
    a: u8,
    b: u8,
    c: u8,
    d: u8,
    e: u8,
    f: u8,
    h: u8,
    l: u8,
    ime: u8,
    #[serde(default)]
    ei: u8,
    ram: Vec<(u16, u8)>,
}

impl State {
    fn registers(&self) -> Registers {
        Registers {
            a: self.a,
            f: self.f,
            b: self.b,
            c: self.c,
            d: self.d,
            e: self.e,
            h: self.h,
            l: self.l,
            sp: self.sp,
            pc: self.pc,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cycle {
    Read(u16, u8),
    Write(u16, u8),
    Idle,
}

struct FlatBus {
    mem: Box<[u8; 0x10000]>,
    log: Vec<Cycle>,
}

impl Bus for FlatBus {
    fn read(&mut self, addr: u16) -> u8 {
        let v = self.mem[usize::from(addr)];
        self.log.push(Cycle::Read(addr, v));
        v
    }
    fn write(&mut self, addr: u16, value: u8) {
        self.mem[usize::from(addr)] = value;
        self.log.push(Cycle::Write(addr, value));
    }
    fn idle(&mut self) {
        self.log.push(Cycle::Idle);
    }
    // No interrupt controller: the suite has flat RAM only.
    fn interrupt_enable(&self) -> u8 {
        0
    }
    fn interrupt_flags(&self) -> u8 {
        0
    }
    fn set_interrupt_flags(&mut self, _: u8) {}
}

fn expected_cycles(case: &Case) -> Result<Vec<Cycle>, String> {
    case.cycles
        .iter()
        .map(|c| match c {
            None => Ok(Cycle::Idle),
            Some((addr, data, kind)) => match (kind.as_bytes().first(), kind.as_bytes().get(1)) {
                (Some(b'r'), _) => Ok(Cycle::Read(addr.unwrap_or(0), data.unwrap_or(0))),
                (_, Some(b'w')) => Ok(Cycle::Write(addr.unwrap_or(0), data.unwrap_or(0))),
                _ if kind == "---" => Ok(Cycle::Idle),
                _ => Err(format!("unknown cycle kind {kind:?}")),
            },
        })
        .collect()
}

/// What went wrong with one case, if anything: (state mismatch, cycle
/// mismatch).
fn run_case(case: &Case, bus: &mut FlatBus, exempt: bool) -> (Option<String>, Option<String>) {
    bus.mem.fill(0);
    bus.log.clear();
    for &(addr, v) in &case.initial.ram {
        bus.mem[usize::from(addr)] = v;
    }
    let mut cpu = Cpu::new();
    cpu.set_registers(case.initial.registers());
    cpu.set_ime(case.initial.ime != 0);
    cpu.set_ei_pending(case.initial.ei != 0);
    let expected = match expected_cycles(case) {
        Ok(c) => c,
        Err(e) => return (Some(e), None),
    };
    loop {
        cpu.step(bus);
        if bus.log.len() >= expected.len() {
            break;
        }
    }

    let mut state = Vec::new();
    let (mut got, want) = (cpu.registers(), case.end.registers());
    if exempt {
        // See [`STOP_FILE`]: PC and the cycle list are not compared.
        got.pc = want.pc;
    }
    if got != want {
        state.push(format!("registers {got:02X?}, expected {want:02X?}"));
    }
    if cpu.ime() != (case.end.ime != 0) {
        state.push(format!("ime {}, expected {}", cpu.ime(), case.end.ime));
    }
    if cpu.ei_pending() != (case.end.ei != 0) {
        state.push(format!("ei {}, expected {}", cpu.ei_pending(), case.end.ei));
    }
    for &(addr, v) in &case.end.ram {
        let got = bus.mem[usize::from(addr)];
        if got != v {
            state.push(format!("[{addr:04X}] = {got:02X}, expected {v:02X}"));
        }
    }
    let state = (!state.is_empty()).then(|| state.join("; "));
    let cycles = (!exempt && bus.log != expected)
        .then(|| format!("cycles {:X?}, expected {expected:X?}", bus.log));
    (state, cycles)
}

struct FileResult {
    name: String,
    cases: usize,
    /// Cases whose PC and cycles were not compared ([`STOP_FILE`] only).
    exempt: usize,
    state_failures: usize,
    cycle_failures: usize,
    first_failure: Option<String>,
}

/// The expected SHA-256 (lowercase hex) of each data file, by file name.
fn expected_hashes() -> BTreeMap<&'static str, &'static str> {
    HASHES
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (hash, name) = l
                .split_once("  ")
                .unwrap_or_else(|| panic!("bad line in sm83-v1.sha256: {l:?}"));
            (name, hash)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Runs one file, after checking its hash against `want`.
fn run_file(path: &Path, want: &str) -> FileResult {
    let name = path.file_stem().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let got = hex(&Sha256::digest(&text));
    assert_eq!(
        got,
        want,
        "{}: SHA-256 doesn't match tools/sm83-tests/sm83-v1.sha256; re-fetch with \
         scripts/fetch-sm83-tests.sh",
        path.display()
    );
    let cases: Vec<Case> =
        serde_json::from_slice(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let exempt = name == STOP_FILE;
    let mut bus = FlatBus {
        mem: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
        log: Vec::with_capacity(8),
    };
    let mut r = FileResult {
        name,
        cases: cases.len(),
        exempt: 0,
        state_failures: 0,
        cycle_failures: 0,
        first_failure: None,
    };
    for case in &cases {
        r.exempt += usize::from(exempt);
        let (state, cycles) = run_case(case, &mut bus, exempt);
        if state.is_some() {
            r.state_failures += 1;
        }
        if cycles.is_some() {
            r.cycle_failures += 1;
        }
        if r.first_failure.is_none() && (state.is_some() || cycles.is_some()) {
            let what = [state, cycles].into_iter().flatten().collect::<Vec<_>>();
            r.first_failure = Some(format!("{}: {}", case.name, what.join("; ")));
        }
    }
    r
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-data/sm83/v1")
}

#[test]
fn sm83_single_step_tests() {
    let dir = data_dir();
    let required = std::env::var(REQUIRE_ENV).is_ok_and(|v| v == "1");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        assert!(
            !required,
            "SM83 single-step tests not found in {} and {REQUIRE_ENV}=1; run \
             scripts/fetch-sm83-tests.sh",
            dir.display()
        );
        eprintln!(
            "SKIPPED: SM83 single-step tests not found in {}; run scripts/fetch-sm83-tests.sh",
            dir.display()
        );
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();

    // Exactly the files in the hash list: none missing, none extra.
    let hashes = expected_hashes();
    // 244 base opcodes (256 minus CB and the 11 invalid ones) + 256 CB.
    assert_eq!(hashes.len(), 500, "sm83-v1.sha256 should list 500 files");
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let missing: Vec<&str> = hashes
        .keys()
        .copied()
        .filter(|k| !names.iter().any(|n| n == k))
        .collect();
    let extra: Vec<&String> = names
        .iter()
        .filter(|n| !hashes.contains_key(n.as_str()))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "test data in {} doesn't match tools/sm83-tests/sm83-v1.sha256 (missing {missing:?}, \
         extra {extra:?}); re-fetch with scripts/fetch-sm83-tests.sh",
        dir.display()
    );

    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut results: Vec<FileResult> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = files.get(i) else { break };
                        out.push(run_file(path, hashes[names[i].as_str()]));
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    results.sort_by(|a, b| a.name.cmp(&b.name));

    // Every case runs; only the 1000 STOP cases skip the PC/cycle checks
    // (see [`STOP_FILE`]), exactly as many as counted.
    for r in &results {
        let want = if r.name == STOP_FILE { STOP_CASES } else { 0 };
        assert_eq!(
            (r.exempt, r.cases),
            (want, if want > 0 { STOP_CASES } else { r.cases }),
            "{}: (PC/cycles-exempt, total) cases",
            r.name
        );
    }

    let cases: usize = results.iter().map(|r| r.cases).sum();

    let failed: Vec<&FileResult> = results
        .iter()
        .filter(|r| r.state_failures + r.cycle_failures > 0)
        .collect();
    let state_failures: usize = results.iter().map(|r| r.state_failures).sum();
    let cycle_failures: usize = results.iter().map(|r| r.cycle_failures).sum();
    eprintln!(
        "SM83 single-step: {} files, {cases} cases run ({STOP_CASES} STOP cases without PC/cycle checks); \
         {state_failures} final-state and {cycle_failures} cycle failures in {} files",
        results.len(),
        failed.len()
    );
    for r in &failed {
        eprintln!(
            "  {}: {} state / {} cycle failures of {}; first: {}",
            r.name,
            r.state_failures,
            r.cycle_failures,
            r.cases,
            r.first_failure.as_deref().unwrap_or("")
        );
    }
    assert!(
        failed.is_empty(),
        "{} opcode files have failures",
        failed.len()
    );
}
