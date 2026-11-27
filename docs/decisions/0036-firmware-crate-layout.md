# 0036: Firmware crate layout and the board boundary

- Status: Accepted; amended by ADR 0043 (the link servers move to
  `gbc-link-board`, and what a bin holds)
- Date: 2026-11-25
- Builds on: ADR 0005 and its 2026-11-25 amendment, ADR 0026 (ITCM placement
  at link time), ADR 0009 (unsafe-free core), plan `docs/plan/stage-2.md`
  "Crate layout" and decisions D8, D9 and D11

## Context
The human approved the crate layout on 2026-11-25 (plan "Crate layout",
ADR 0005 amendment). This ADR records that layout and settles what the plan
leaves to M1: how the firmware is built, how `tools/h7-bench` shares its
bring-up code, where the `memory.x` generator lives, the trait boundary
between shared and board code, the board lock, and when each new crate is
created.

## Decision

**Layout (human-approved; recorded, not redesigned).**
```
gbc-core ◄── gbc-firmware ◄── gbc-nucleo-h753 ──► gbc-link ◄── tools/gbc-pc
                          ◄── gbc-handheld (Stage 3) ···► gbc-link if USB serial
```
- `crates/gbc-firmware`: `no_std`, no alloc, `#![forbid(unsafe_code)]`,
  root-workspace member, host-tested. It depends on `gbc-core` only and
  never on `gbc-link` (D9).
- `crates/gbc-link`: `no_std`, no dependencies, no unsafe, root-workspace
  member, transport-agnostic serial protocol.
- `crates/gbc-nucleo-h753`: all chip and board code for the NUCLEO-H753ZI.
  PAC only: `stm32h7` (feature `stm32h753v`), `cortex-m` and `cortex-m-rt`,
  the same versions as h7-bench. No HAL and no async runtime. It wires
  `gbc-link` to the UART. `gbc-handheld` takes the same role in Stage 3, and
  shared H7 code is extracted only once Stage 3 actually duplicates it.
- `tools/gbc-pc`: a native Windows `.exe`, root-workspace member,
  cross-built like `gbc-desktop`. Its dependencies are `gbc-link` and
  `windows-sys` (D9: no `serialport`).

**`gbc-nucleo-h753` is a lib plus bins in its own workspace.** It has an
empty `[workspace]` table, as h7-bench does, so root `cargo test
--workspace` never builds it for the host.
- `.cargo/config.toml` sets `target = thumbv7em-none-eabihf` and the
  rustflags `-C target-cpu=cortex-m7`,
  `-C llvm-args=-cgp-max-address-users-to-scan=100000` (ADR 0008) and
  `-C link-arg=-Tlink.x`. Target rustflags replace the root `[build]`
  rustflags, so the CodeGenPrepare flag has to be repeated here. The root
  `jobs = 4` cap still applies, because cargo merges parent configs.
- `[profile.release]`: `codegen-units = 1`, `debug = "line-tables-only"`,
  opt-level 3 and no LTO. These match the conditions under which
  `perf/itcm-sections.txt` and the sizes in `docs/perf.md` were measured.
- The lib supports only the H753 at the D11 clock: HSI 64 MHz into PLL1 at
  472 MHz nominal (DIVM1 5, fractional N, b4d6c81), VOS0 through ODEN, flash
  at 4 WS, AXI/AHB 236 MHz and APB 118 MHz. It has no board or clock
  features.
- The lib holds the bring-up and support code:
  - power supply config (`__pre_init`) and clocks;
  - the UART (VCP, USART3) as a `core::fmt::Write` sink;
  - LEDs and the DWT cycle counter;
  - caches and the MPU;
  - the TCM copy (`tcm::place`);
  - the LSE clock check (h7-bench's `clockcheck`, which the banner runs, D11);
  - the panic handler, HardFault (on its own stack) and DefaultHandler.

  Each bin (`headless` in M1, later `loader` and `player`) supplies only
  `#[entry]` and its mode logic (amended by ADR 0043: and the glue between
  the board-independent crates and the lib's drivers).
- **h7-bench depends on the lib** (`path = "../../crates/gbc-nucleo-h753"`)
  and deletes its own `board.rs` and `tcm.rs`. One copy of the bring-up code
  means the two cannot drift. The fault and panic handlers exist once per
  image, so h7-bench uses the lib's. h7-bench keeps its workloads, banner
  lines and `tcm_probe`.
- h7-bench's `h755` feature and the `pll` (392 MHz) and HSI-64 images are
  removed. The H755 is retired (D8), and D11 fixes the clock. The old figures
  stay in history and in `docs/perf.md`. The `m4-stub/` and `host/` sources
  are unchanged, but `build.sh` stops building the stub.

**One `memory.x` generator: a small host crate, `tools/memory-x`.** It is a
lib with no dependencies, a root-workspace member, and a
`[build-dependencies]` path dependency of both `gbc-nucleo-h753` and
h7-bench.
- It owns the following:
  - the chip maps (H753 from RM0433 Table 7, and QEMU `mps2-an500` for the
    smoke runs; H735 later);
  - the data placements (AXI SRAM or DTCM);
  - the parser for `perf/itcm-sections.txt` (globs, `FLASH_OK`, tier drops);
  - the `.itcm`, `.dtcm_rodata` and `.axisram` sections and their ASSERTs;
  - a reserved fault-stack region.

  The current code moves there from h7-bench's `build.rs`.
- A crate is better than a `#[path]` include (the `synth.rs` pattern) for one
  reason: root `cargo test --workspace` unit-tests the parser and the output,
  including tier drops, the odd-glob panic, and the "set nearly empty" and
  "does not fit" ASSERTs. An included file has no test home.
- Each final image's `build.rs` passes its layout and writes `memory.x` into
  its own `OUT_DIR`. `gbc-nucleo-h753` adds that directory to the link with
  `cargo:rustc-link-arg-bins=-L<OUT_DIR>` instead of `rustc-link-search`.
  The bins-only arg does not propagate to dependents, so h7-bench's own
  `memory.x` is the only one on its link line. This still has to be checked
  in M1 (see Consequences).
- The sections are always emitted, empty when unused, together with
  `__sitcm`, `__eitcm`, `__siitcm`, the `__*dtcm_ro` symbols and
  `__fault_stack_top`. That way the lib's TCM copy and fault handler need
  no placement features and cannot disagree with the map the bin linked.

**Board boundary.** `gbc-firmware` defines only the traits that M1's
headless loop uses:
- `CycleClock`: `fn now(&self) -> u32` (wrapping) and `fn hz(&self) -> u32`.
- Output goes to `core::fmt::Write`. No new trait is defined for it.
- The ROM is a `&[u8]`, and save RAM and the frame and audio buffers are
  `&mut` slices that the caller provides.

Display, audio-out and button traits are added in M3, M4 and M5, each when
its driver exists and is shaped by that driver. `gbc-firmware` holds the
headless loop (scripted input, frame and audio-stream FNV-1a hashes, the
line format). The board image and the host counterpart in `tools/` both
call it, so their lines cannot diverge.

Everything with `unsafe` or registers stays board-side:
- startup and `__pre_init`;
- the PAC, clocks, power, the MPU and caches;
- the TCM copy;
- the fault and panic handlers;
- the UART and the DWT;
- the generated `memory.x`;
- the static buffers in `.axisram` and DTCM. `link_section` is unsafe in
  edition 2024, so `gbc-firmware` cannot place statics. The board declares
  them and lends them to the firmware.

**Board lock.** Every script that flashes or talks to the board takes one
exclusive `flock`. This covers h7-bench's `flash.sh` and `read-serial.sh`,
the firmware's flash and run scripts, `gbc-pc` runs through
`run-windows.sh`, and the on-board steps of `gate-stage2.sh`.
- The lock file is `${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock}`, not
  `target/board.lock`. Scratch worktrees have their own `target/`, and
  `cargo clean` could delete the file while it is held. There is one board
  per machine, so the path is per machine.
- The helper is `scripts/lib/board-lock.sh` (`with_board_lock CMD...`,
  `flock -x -w 1800`, a clear failure message on timeout).
- It is re-entrant through `GBC_BOARD_LOCK_HELD=1`. A flash-then-read
  sequence holds the lock across both steps, and the nested scripts don't
  deadlock on a second `flock` of the same file.
- The board lock is separate from h7-bench's build lock (`H7_BENCH_LOCK`).

**When the crates appear.**
- `gbc-firmware` and `tools/memory-x` are created in M1 with real content:
  the headless loop and `CycleClock`, and the moved generator. They are not
  empty placeholders.
- `gbc-nucleo-h753` is created in M1.
- `gbc-link` and `tools/gbc-pc` are created in M2 together with the UART
  loader (D5), their first user. An empty crate before that would test
  nothing.

## Consequences
- The bring-up code exists once. A change to the clocks or the fault handler
  reaches h7-bench's measurements and the firmware together, so h7-bench
  numbers taken after the move have to be compared with that in mind.
- The `memory.x` logic gets host unit tests. h7-bench's `build.rs` shrinks
  to the ROM generation plus one call into the generator.
- `cargo test --workspace` gains `gbc-firmware` and `memory-x`, and in M2
  `gbc-link` and `gbc-pc`. The thumbv7em builds of `gbc-nucleo-h753` and
  h7-bench run only in `gate-stage2.sh` (C-T6).
- The rustflags exist in three copies: root, h7-bench and
  `gbc-nucleo-h753`. The gate should check that the firmware copies contain
  the root's LLVM flag (`scripts/lib/rustflags.sh` already checks the root
  copy). This is a stricter check.
- Two separate workspaces mean two `Cargo.lock` files that can resolve
  `cortex-m`, `cortex-m-rt` or `stm32h7` differently. Pin them to the same
  versions and check that in the gate.
- Follow-ups for M1:
  - a `hard_rules` test for `gbc-firmware`, like gbc-core's (forbid unsafe,
    no `alloc` or `std`);
  - a build check that h7-bench's ELF carries its own map (placement-specific
    initial SP and `__ebss`, as `build.sh` checks now), which verifies the
    link-arg-bins choice;
  - the board-lock helper, mutation-checked with two concurrent holders.
