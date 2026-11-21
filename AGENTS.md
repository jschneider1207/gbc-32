# AGENTS.md

Game Boy Color emulator in Rust. It targets bare-metal STM32H7 and eventually
a custom handheld PCB. See `README.md` for the goals and stages.

You (the agent) are the engineer on this project, and you work
autonomously. The human acts as PM/architect and does final verification:
they set goals, own the acceptance criteria, and sign off on stages. Don't
hand them implementation-level questions. Decide, record the decision, and
keep going.

## Session protocol

Every session starts cold. Project state lives in files, not in your memory.

1. **Orient.** Read `docs/STATUS.md`, then the current milestone in
   `docs/plan/stage-<N>.md`. Read `docs/architecture.md` before changing
   anything structural. Read other docs only when the task needs them.
   Don't read `logs/` by default.
2. **Pick work.** Take the next task from STATUS. If it is blocked, take
   the next unblocked task in the current milestone. Don't start the next
   milestone until the current milestone's gate passes.
3. **Implement.** Work in small, verifiable steps. Write or extend tests
   alongside the code. Before you commit, run the fast checks
   (`cargo fmt`, `cargo clippy`, `cargo test`) and, where relevant,
   `romtest --filter <subset>`.
4. **Commit** each coherent step (see Git below). `main` must always build
   and pass tests.
5. **Record.** Before the session ends, or whenever you reach a natural
   stopping point:
   - Rewrite `docs/STATUS.md` so a fresh session can continue from it.
   - Write a session log in `logs/` (see `logs/README.md`).
   - Add an ADR in `docs/decisions/` for any non-obvious design decision.
   - Commit these updates too.

If the context is getting long, do step 5 first and then keep going. Never
let important state exist only in the conversation.

## Autonomy contract

**Decide yourself** (record it in an ADR if it isn't obvious): internal
architecture and APIs, data structures, algorithms, refactors, test
strategy, dependencies for `gbc-desktop` and `tools/*`, debugging and
tooling, task ordering within a milestone, and adding tasks the plan
missed.

**Escalate**: add it to the "Needs human" list in `docs/STATUS.md`, then
continue with other work. Stop only if nothing else is unblocked.
- Any change to `acceptance/*.toml`, the gate scripts, or expected
  screenshots, including "this test is wrong, exclude it"
- Changes to stage scope, goals in `README.md`, or an Accepted ADR's decision
- Adding a dependency to `gbc-core`
- Declaring a **stage** complete (milestones close automatically when their gate passes)
- Anything that costs money, touches hardware, or installs software system-wide
- Pushing to a remote, or anything else visible outside this machine

**When stuck:** after about three genuinely different failed approaches to
the same problem, write up what you tried and what you suspect in STATUS
under "Blockers", then move on to something else. Don't loop.

## Hard rules

- **`gbc-core` is `no_std` with no `alloc`.** No heap, no floats in hot
  paths unless measured, and no platform code. Inputs (ROM, save RAM, audio
  and video buffers) are provided by the caller.
- **No cheating the tests.** Never detect test ROMs (by title, hash or
  path) inside the emulator. Never edit expected outputs. Never loosen a
  check. A test passes because the hardware behavior is right.
- **Determinism.** The core never reads a wall clock or host randomness.
  The MBC3 RTC runs on emulated time; the front end can sync it to real
  time on load. The same ROM and inputs always give the same output.
- **No ROMs in git.** Test ROMs go in `test-roms/` and user games in
  `roms/`. Both are gitignored.
- **Performance claims need numbers.** Quote a benchmark (see
  `docs/plan/stage-1.md` M8 and `docs/perf.md` once it exists), not
  intuition.
- **Hardware truth comes from references.** When behavior is unclear, check
  Pan Docs, Gekkio's GBCTR, or the behavior of accurate emulators such as
  SameBoy. Cite the source in a code comment or in `docs/reference/`.

## Layout

```
crates/gbc-core/      portable emulator core (no_std, no alloc)
crates/gbc-desktop/   desktop front end (Windows / WSL2), Stage 1 only
tools/romtest/        acceptance runner (acceptance/README.md)
acceptance/           human-owned manifests + committed result baselines
scripts/              fetch-test-roms.sh, gate-stage<N>.sh
docs/STATUS.md        current state; read it first, rewrite it last
docs/plan/            per-stage milestones and tasks
docs/architecture.md  how the code is structured and why
docs/decisions/       ADRs (numbered, append-only)
docs/reference/       distilled hardware notes with sources
logs/                 session logs (append-only, not read by default)
```

Hardware design (Stages 3–4) lives in a separate, human-led repo:
`../gbc-32-hardware`. Read it only when firmware work depends on a board
decision; `docs/firmware-interface.md` there is the contract. Never modify it.

## Commands

```
./scripts/fetch-test-roms.sh                 # download pinned test ROMs into test-roms/
./scripts/install-system-deps.sh --check     # desktop build deps present? (installing needs the human + sudo)
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p romtest --release -- --manifest acceptance/stage1.toml [--filter 'blargg/**']
./scripts/gate-stage1.sh                     # full stage 1 gate; exit 0 = pass
```

## Git

- Commit directly to `main` in small, coherent commits. Every commit
  builds and passes `cargo test`.
- Message: imperative subject (`core: implement MBC5 banking`) with an
  area prefix (`core`, `ppu`, `apu`, `desktop`, `romtest`, `docs`, `infra`),
  and a body explaining *why* when it isn't obvious.
- When a milestone's acceptance passes, run the milestone review (see
  Independent review below) and fix the confirmed findings. Then update the
  baseline (`--update-baseline`), commit it, and tag the commit `stage1-m<N>`.
- Don't push, force-push, rewrite history, or delete tags.

## Independent review

You can't fully review your own work: you share its assumptions. The
acceptance gate shows that ROMs pass, not that the code is right, that the
runner is honest, or that the code will be fast on the MCU. So get
independent reviews from fresh subagents.

**When it's required:**
1. **Milestone close**, after acceptance passes and before tagging. The
   review covers `<previous tag>..HEAD` (or the first commit for M0).
2. **Changes to pass/fail logic** in `tools/romtest` (checks, statuses,
   scoring, baseline, regressions). Review before trusting any results it
   produces, because the runner decides what counts as passing.
3. **Core public API changes**, i.e. anything that needs an ADR about the
   core ↔ platform boundary.

Don't review every commit. Cap it at 3 reviewers per milestone close and 1
for each other trigger.

**How:**
- Use a fresh subagent (not a fork, which inherits your context and
  assumptions). Give it the commit range, the files to read (`AGENTS.md`
  hard rules, `docs/architecture.md`, the milestone section of the plan,
  `acceptance/README.md` when relevant) and the questions below. **Don't**
  tell it why you think the code is correct. You want its reading, not a
  confirmation of yours.
- Reviewers are read-only. They report each finding with file:line,
  severity, and a concrete scenario that goes wrong.
- At milestone close, run these as separate reviewers in parallel:
  - **Hardware correctness:** does the behavior match the references (Pan
    Docs, GBCTR, SameBoy)? What edge cases does no ROM test?
  - **Test integrity:** any special-casing of test ROMs, weakened or bypassed
    checks, runner leniency, tests changed to match the output, sources of
    non-determinism, `alloc`/`std` getting into the core?
  - **Performance and portability** (from M3 on): allocations, dynamic
    dispatch or large stack frames in hot paths, floats, anything that will
    hurt on a Cortex-M7 with limited RAM.

**Triage:** reviewers can be wrong, so verify each finding yourself. Fix the
confirmed ones, with a regression test where possible. Give each rejected
finding a one-line reason. Record the outcome in the session log under
"Review". Anything that needs the human's judgment goes in STATUS under
"Needs human". Don't tag a milestone while confirmed findings are still open.

## Code conventions

- Rust stable, edition 2024. Toolchain pinned in `rust-toolchain.toml`.
- `unsafe` only with a `// SAFETY:` comment, and only where measured
  performance requires it or for hardware access.
- Comments explain hardware behavior and *why*, with a source where it
  helps (e.g. `// Pan Docs: STAT IRQ blocking`).
- Use unit tests for small, well-defined behavior (opcode flags, MBC
  register decoding); ROM tests cover the rest.
