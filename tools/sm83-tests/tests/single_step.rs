//! Runs every SingleStepTests/sm83 case (`test-data/sm83/v1/*.json`)
//! against `gbc_core::sm83::Cpu` on a flat 64 KiB RAM bus, checking the
//! final registers, IME/EI state and memory, and the bus activity of every
//! M-cycle (address, data and read/write for accesses; an internal cycle
//! where the test has none). Skips with a message if the data is missing.
//!
//! Format notes (from the suite's README and data): each case starts with
//! PC at the opcode, and its `cycles` list starts with the opcode fetch.
//! `ie` in the state is to be ignored. The final state has `"ei": 1` when
//! EI was just executed (IME pending). Cases are run by stepping the CPU
//! until it has used as many M-cycles as the case lists (HALT and STOP
//! cases list idle cycles after the instruction).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gbc_core::sm83::{Bus, Cpu, Registers};
use serde::Deserialize;

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
fn run_case(case: &Case, bus: &mut FlatBus) -> (Option<String>, Option<String>) {
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
    let (got, want) = (cpu.registers(), case.end.registers());
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
    let cycles =
        (bus.log != expected).then(|| format!("cycles {:X?}, expected {expected:X?}", bus.log));
    (state, cycles)
}

struct FileResult {
    name: String,
    cases: usize,
    state_failures: usize,
    cycle_failures: usize,
    first_failure: Option<String>,
}

fn run_file(path: &Path) -> FileResult {
    let name = path.file_stem().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let cases: Vec<Case> =
        serde_json::from_slice(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut bus = FlatBus {
        mem: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
        log: Vec::with_capacity(8),
    };
    let mut r = FileResult {
        name,
        cases: cases.len(),
        state_failures: 0,
        cycle_failures: 0,
        first_failure: None,
    };
    for case in &cases {
        let (state, cycles) = run_case(case, &mut bus);
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
    let Ok(entries) = std::fs::read_dir(&dir) else {
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
    // 244 base opcodes (256 minus CB and the 11 invalid ones) + 256 CB.
    assert_eq!(
        files.len(),
        500,
        "unexpected number of test files in {}",
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
                        out.push(run_file(path));
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

    let cases: usize = results.iter().map(|r| r.cases).sum();
    let failed: Vec<&FileResult> = results
        .iter()
        .filter(|r| r.state_failures + r.cycle_failures > 0)
        .collect();
    let state_failures: usize = results.iter().map(|r| r.state_failures).sum();
    let cycle_failures: usize = results.iter().map(|r| r.cycle_failures).sum();
    eprintln!(
        "SM83 single-step: {} files, {cases} cases; {state_failures} final-state and \
         {cycle_failures} cycle failures in {} files",
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
