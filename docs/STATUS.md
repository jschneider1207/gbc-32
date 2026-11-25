# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M10 (in-depth review), in progress. M9 is `stage1-m9`
  (49c0756); M1-M8 tagged likewise.
- **Last updated:** 2026-11-24, session 17 (log
  `logs/2026-11-24-17-m10-perf-bench.md`; session 16:
  `logs/2026-11-24-16-m10-review.md`).
- **Model (human, 2026-11-24): the main session for the rest of M10 must
  be Fable (`claude-fable-5-2`), as session 16 was.** If the main session
  is running on another model, stop and tell the human before doing any
  work. Subagents run on whatever model the main (Fable) session judges
  best for each task.

## Next up: M10 (in-depth review), second half

**Read `docs/reviews/stage1-m10.md` first** (triage: every finding and its
outcome). Raw reports and the inventory of budget-driven code shapes
(`F-budget-distortions.md`: 55 items, 12 themes, 22 keep-as-is) are in
`docs/reviews/stage1-m10/`. The human gave this milestone more latitude
than the usual rules ("improve it instead of just copying"; "ensure the
core is written in the most logical and high performant way"); their
decisions are under "Decided in session 16".

**Done (all on `main`):** the eight reviews and triage; nets (checked
profile in the fuzz gate; `check-sync-invariance.sh --long` in the gate:
eager-sync equality with an empty allowlist, clock offsets past 2^32,
30-minute run; `tests/apu_output.rs`); every confirmed correctness finding
(palette/LCD-off, WY latch, HDMA cancel + stale flag, speed-switch order,
STOP mode + RP, two APU fixes, DMA retime, RTC mask, unsupported mappers
refused (ADR 0031), HuC1 + HuC3 (ADR 0032)); `codegen-units = 1`; size
gate sized for the STM32H735 (only total `.text` <= 122,880 B gates);
perf: lazy timer, serial one event per transfer, timed interrupt requests
(ADR 0033), renderer tile-loop specialisation, APU loops + mixer spans,
PPU fetch state out of `Memory` + `Stalls` type, interpreter loop
(attention flag, offset-0 tick, little-endian register pairs).
Also landed: the LLVM address-folding option in every build (f4e16f5,
human approved; guards in `check-code-size.sh` and `check-bench.sh`), and
the PPU walk theme incl. the line-153 LYC=0 timed request (L5, c08c22a).
Numbers at the last merge: worst-case 278.1 M Ir (2.33 M/frame; 488.4 M this
morning after the cgu=1 pin), cpu_instrs 1,380 M (1,703 M), `.text`
113,154 B (margin 9,726 B).

**Remaining, in order** (one agent per item, see "How work is run"):
1. DONE (session 17, c2f19a0): bus dispatch reorder (cpu_instrs -7.52%,
   cgb_sound -5.22%, dma-storm -5.24%, +754 B); A1 inline forms and L1
   measured and dropped (docs/perf.md "CPU write path"/"CPU read path";
   the push-only inline write, +1,664 B for ~0.75%, is the best leftover).
2. DONE (session 17): ADR 0027 retired, ROM is a plain slice, VRAM DMA
   copies ROM blocks as 16-byte chunks (-328 B); `check-rom-loads.sh`
   removed from the gate and deleted (6d8434f). Baseline eb31bf6:
   worst-case 276.1 M Ir, cpu_instrs 1,275.7 M, `.text` 113,580 B
   (margin 9,300 B).
3. PARTLY DONE (session 17, m10-sched merged): one-pass scheduler (B5),
   theme 10 attributes by measurement, theme 4 move (read clears in
   `Sys`), T2. Numbers in docs/perf.md "Scheduler and attribute pass".
   Left: theme 1 (S2/S3/S9/S10), theme 11 (A14/A8/A11, L7), S4 `&mut`
   read path; re-measure the dropped `schedule_overflow` inline
   (+1,270 B) on the new serviced-timer worst-case.
4. DONE (session 17): `synth:worst-case` services a 16,384 Hz timer IRQ
   playing PCM through NR50 (2.422 M Ir/frame at merge, 60.2% of the
   4.02 M line); new `synth:dmg-raster` line. Baseline 782d01e:
   worst-case 287.7 M Ir, cpu_instrs 1,269.5 M, `.text` 113,832 B
   (margin 9,048 B).
   The new program found a core bug (`check-sync-invariance.sh` failed):
   a mode-3 write that moves mode 0 (SCX in the discard, window
   start/cut) left the HBlank HDMA trigger at the old time; fixed cab6e24
   (`write_ppu` re-arms it; test seen failing; +120 B; only the five
   `synth:worst-case` audio fingerprints changed). Baseline not
   re-recorded after it (max +0.16%).
   Small leftovers: add `synth:worst-case@0` (no APU) to the invariance
   corpus (stricter: agent decision); `synth.rs` comment says the SCX
   write lands 18-42 M-cycles into mode 3 but some land in the discard.
   Item 7's oracle is DONE (0e674bc, session 17): `ppu/mode3_oracle.rs`
   (test-only, SameBoy-derived dot-stepped mode 3; doc
   `docs/reference/mode3-oracle.md`); 28,949 static and 28,040 SCX-write
   lines agree with the core. Two disagreement classes are on its
   enumerated exclusion list and need a verdict (one small agent each,
   references first; no core change without one):
   (a) WX=0 with a fine scroll and an object: DECIDED (human, 2026-11-24,
   "go with your judgement for now and ensure it is documented"): the
   stalls follow the renderer's 7+f window grid (ADR 0034, f21ea36 line;
   test seen failing). References split 7 / 7+f / 8+f; the oracle
   exclusion stays for 7+f vs SameBoy's 8+f (892 of 1,348 lines).
   (b) SCX write with an object at X=0: FIXED (session 17, 2244e84 +
   705757c): X=0 objects are fetched before the fine-scroll discard
   (SameBoy, GateBoy netlist, Pan Docs; no ROM reaches it), plus a
   junk-tile sharing correction when the write wraps the discard;
   exclusion removed, 40,400 fast / 536,400 exhaustive lines agree.
   (c) WX=0 with a mid-line SCX write: FIXED (695a55b line, session 17):
   the fine-scroll discard runs after a window that starts at WX=0
   (SameBoy, GateBoy, Pan Docs; no ROM reaches it); 14,000 WX=0 lines
   added to the sweep; on-screen-object lines stay under the ADR 0034
   exclusion. Open edges (plan tasks, no capture pins any of them):
   (d) a later window write on a line whose discard wrapped rebuilds the
   stalls without the junk-tile correction or the kept WX=0 dot;
   (e) pixels when the discard wraps (n >= 8) at WX=0: the renderer cuts
   7 + n%8, not 7 + n. Decisive capture for (c): CGB, WX=0, SCX 1, SCX=0
   written at 89..=97, mode 0 at 263 / 271 (at 96) / 264.
   Not covered by the oracle: double speed, first line after LCD on,
   WX/WY/LCDC mid-line writes, OAM DMA lines, pixels. Audio alias
   measurement (A3-1) is still open under item 7.
   `.text` 116,358 B (margin 6,522 B); baseline 0270845 (latest core is
   up to +0.72% on dmg-raster over it: re-record at the next perf merge).
   **First real M7 numbers** (NUCLEO-H755ZI-Q, all code in flash, caches
   on, HSI 64 MHz, hashes match the host): cycles/frame mean/max:
   worst-case 4.35 M / 4.40 M, cgb-raster 2.93 / 2.99, dmg-raster
   2.22 / 2.26, event-storm 2.22 / 2.25, cpu_instrs 1.44 / 2.05. About
   1.8 cycles per x86 Ir on worst-case; 4.40 M is ~55% of a frame at
   480 MHz, ~48% at 550 MHz (extrapolated). DTCM-stack and ITCM images
   fault on the board (agent debugging on `m10-h7-spike`; results go to
   docs/perf.md). Flash large images with a native Windows copy
   (`cmd.exe copy` to E:), not `cp` to /mnt/e (silently fails).
5. Structure: line timeline as one table (E3), `bus.rs` split along
   D-rust.md D2's nine seams, `Speed` enum (D4), speed-switch DIV-reset
   experiment (A1-06).
6. Docs/comment sweep: `docs/architecture.md` (D7), stale and budget-era
   comments (D8, inventory section 6), stale names (`Memory::obj_rank` in
   ADR 0019 and plan line ~117, `Emulator` size in lib.rs), perf.md tables.
7. Test-only dot-stepped mode-3 oracle (E1); audio alias measurement
   (A3-1; any Ir cost goes to the human).
8. Triage lows into plan tasks; M8 review lows still open in the plan;
   full `./scripts/gate-stage1.sh`; `romtest --update-baseline`; session
   log; tag `stage1-m10`. Declaring Stage 1 complete is the human's.

**How work is run:** one agent per theme in a scratch worktree under the
session scratchpad (`git worktree add <scratch>/wt-x -b m10-x main`), every
cargo/valgrind command under `flock -x <scratch>/build.lock` (absolute
path). The orchestrator rebases the branch on `main`, re-runs clippy,
tests, `check-code-size.sh`, `check-itcm-set.sh`, `diff-fingerprint.sh main
--audio-buffer 7 --hash-samples`, `check-sync-invariance.sh`, romtest full
manifest and `check-bench.sh`, then `git merge --ff-only`, removes the
worktree, and re-records `perf/bench-baseline.txt` after perf merges.
Correctness fixes need a test seen failing first; refactors must be
fingerprint-identical. Briefs that worked are in the session transcript
only; the pattern is: goal, files, constraints, what to measure, exact
report format, commit trailer.

**Gates now:** total gbc-core `.text` <= 122,880 B (128 KiB ITCM on the
H735 minus 8 KiB reserve; provisional until a firmware ELF exists); +3% Ir
vs `perf/bench-baseline.txt`; fuzz under the `checked` profile; invariance
checks.
Per-function size lines and the H753 ITCM hot set are report-only (the
dev-board set is over its figure: expected, WARN only). Release profile:
`codegen-units = 1` plus
`-C llvm-args=-cgp-max-address-users-to-scan=100000` (human approved).

**Human (session 14): the desktop client is a throwaway used to validate
the core; spend minimal time on it.** Fix only what blocks core validation
(crashes, lost saves, wrong output). No more smoothness/pacing polish after
the in-flight present-phase fix. The SameBoy differential harness stays
worthwhile (it validates the core), at lower priority than M9 ROM fixes.

**Safety (read AGENTS.md "Machine resources"):** never open the Linux
window under WSLg and never use the GPU from WSL. Window/audio/input smoke
tests use `scripts/run-windows.sh <rom> --frames N [--out shot.png]` on a
scratch ROM copy (it forwards `GBC_FRAME_STATS`/`GBC_AUDIO_STATS` via
WSLENV); short, and only for front-end changes. Windows Update can shut WSL
down mid-session: commit often.

Desktop front end (`crates/gbc-desktop`): `gbc-desktop <rom> [--scale K]
[--frames N] [--screenshot out.png] [--dmg-palette auto|gray]
[--headless]`. Default scale 4. Blank LCD shows white. Raw RGB555->888
colours, no colour correction (human decision, session 14). Save:
`<stem>.sav` (ADR 0023). Keys: arrows, Z = B, X = A, Enter = Start, Right
Shift/Backspace = Select, Esc quits (same game keys as SameBoy).

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change" (in a
  worktree pass `--root /home/jess/gbc-32/test-roms`).
- Resources: at most 2 concurrent builds; heavy steps (check-bench,
  diff-fingerprint, gate) alone. Session 14 used a shared flock
  (`flock -s` for builds, `flock -x` for heavy steps) on one lock file in
  the session scratchpad; it serialises heavy steps but doesn't cap builds
  at 2, so launch at most 2 building agents.
- Core work in scratch worktrees, merged by rebase + `--ff-only`.
- ADR numbers: check `docs/decisions/` before committing one (next free 0030).
- PPU design size estimates ran ~2-3x low; measure early.

## In flight
- None at cab6e24 (session 17 clean point; log
  `logs/2026-11-24-17-m10-perf-bench.md`): `m10-bus-fast`, `m10-rom-wide`,
  `m10-bench-irq`, `m10-sched`, `m10-sync-apu` merged by fast-forward,
  worktrees removed (branches left for the human to delete).
- Before session 17: Session 16 ended at a clean point: every `m10-*` branch is merged
  by fast-forward and its worktree removed; `main` builds, passes tests and
  the checks listed under "How work is run" as of the last merge
  (c08c22a + baseline). The full `gate-stage1.sh` has NOT been run since
  the M10 work started: run it before tagging (remaining item 8).

## Needs human

Open (M10, session 17):
- **Hardware capture that settles ADR 0034** (WX=0 window grid): CGB,
  WX=0, WY=0, each SCX&7 in 1..=7, window tiles with distinguishable
  columns: the window pixel at screen x=0 is column 7, 7+f, or 8+f.
- **NUCLEO-H755ZI-Q spike** (human, 2026-11-24): board on USB (drive E:
  = /mnt/e label NOD_H755ZIQ, VCP COM3), full chip erase done, option
  bytes BCM7+BCM4 enabled (not to be changed). Standing permission to
  flash it and read the serial port without asking; CubeProgrammer is
  the human's recovery tool (connect under reset). Firmware:
  `tools/h7-bench` on branch `m10-h7-spike` (agent, build only).
  `qemu-system-arm` 8.2.2 is installed (mps2-an500).
- Item 3 leftovers as plan tasks: inventory L7; `Ctx::write_high` and
  `Apu::write_rest` unpin never measured; romtest worker `eprintln!`
  deadlocks (main.rs holds the stderr lock).
- **M7 instruction counts without hardware:** the human offered to install
  `qemu-system-arm` in WSL (`sudo apt install qemu-system-arm`) when they
  get a chance; then build a bare-metal Cortex-M7 harness (e.g. machine
  mps2-an500) counting thumb instructions for worst-case and cpu_instrs
  (TCG insn plugin, or `-icount`). STM32CubeIDE (Windows) has no simulator;
  useful only once a board exists.
- **Gate edit made on the retire decision:** the byte-wide ROM load step
  was removed from `gate-stage1.sh` and `check-rom-loads.sh` deleted
  (6d8434f); say if a replacement check is wanted.
- **Branches to delete** (session 17, merged): `m10-bus-fast`,
  `m10-rom-wide`, `m10-bench-irq`, `m10-sched`, `m10-sync-apu`.
- FYI: Shantae (heavy HDMA) is worth a replay after cab6e24 (HBlank HDMA
  trigger fix); no evidence it was affected.

Open (M10, session 16):
- **A better M7 cycle proxy without hardware** (review B3): x86 Ir tracks
  M7 cycles poorly (thumb/x86 instruction ratio 0.9-3.35 per function; all
  clock math is u64). Options that need a system install: `qemu-system-arm`
  with an instruction-count plugin, or an i686 valgrind. Or a one-day
  NUCLEO-H753ZI spike measuring two bench workloads. Until then the 4.02 M
  line is a belief, and u32 clocks (B4) / dot-stepped mode 3 in the core
  (E1) / the CPU fetch rework (E5) wait for Stage 2.
- **HDMA cancel with a non-zero value while a block is already triggered**
  (A4-2, 6094acb): we now copy that one block (gambatte); SameBoy master
  copies value+1 blocks (what we did before). The CGB captures only cancel
  with 00. Single-source; a hardware capture would settle it.
- **Filler-header homebrew** (ADR 0031): a ROM-only image whose header byte
  0147 is FF/FE (padding) is now refused as HuC1/HuC3 (after those mappers
  land it will load as them). SameBoy and real no-MBC boards ignore 0147.
- **Audio aliasing** (A3-1): fix candidates roughly double the channel and
  mixing cost; will come back with a measurement before changing anything.
- **Branches to delete** (agent `git branch -D` is denied), merged by
  fast-forward in session 16: `m10-timer-lazy`, `m10-ppu-lcdoff`,
  `m10-cart-unsupported`, `m10-hdma-cancel`, `m10-speed-switch`.

Open (earlier sessions):
- **Stage 3 ROM memory (hardware repo, human-owned):** independent review
  `docs/reference/stage3-rom-memory-options.md` (session 12) recommends
  3.0 V HyperRAM on OCTOSPI (as on the STM32H735G-DK) over the ADR 0001 FMC
  SDRAM, medium confidence, pending a DK measurement; revisiting ADR 0001 is
  the human's call. Also found: the ADR 0002 pin draft puts SDNE0 on PC2_C,
  which adds 4.5 ns (DS13312 Table 76 note 2, checked) and breaks 95 MHz
  SDRAM timing if SDRAM stays.
- **Real-game checklist** (plan, bottom; M9 task), on native Windows.
  Session 14: Wario Land 3 and Shantae played (look and sound right);
  Ctrl+C and Esc saves, `.sav` across restart and the audio device switch
  all passed. Open: smoothness vs SameBoy (see Next up, desktop), a
  gamepad, Pokémon Crystal (RTC across a restart), Alone in the Dark, Link's
  Awakening DX, and Pokémon Red (DMG compat palette; not in the plan table).
- **Branches to delete** (agent `git branch -D` is denied). Stale/superseded:
  `ppu-edges`, `edges-hdma-scratch`, `lcd-offset-step1`, `lcd-offset-phase`,
  `ppu-edges-step1b` and `hdma-trigger` (landed as rebased copies in
  session 12). Merged in session 12: `m9-spsw`, `m9-step1b-hdma`, `m9-late-wy`.
  Merged in session 15 (fast-forward): `m9-apu-15`, `m9-mealy-cd`,
  `m9-lcd-offset`, `m9-m3start-access`, `m9-scx-0367c0`,
  `m9-acid-hell-lcdc`, `m9-speedchange-ch1`, `m9-tile-sel-set`,
  `m9-tile-sel-latch`, `m9-cgb-wx-glitch`.
  Fully merged (`git cherry main <b>` shows no `+`): `bench-worst-case-midline-scx`,
  `ppu-objmix`, `wip/speed-switch-h3`, `worktree-agent-a62aec9bd37d37e1c`,
  `worktree-agent-ab9ff6f40452030b3`, `worktree-agent-ae119d2f1346ad041`.
  Not fully merged, keep until checked: `ppu-bg-attributes` (1 `+`),
  `worktree-agent-a46dc3e70b2125ffb` (1 `+`). Worktree-agent branches may be
  locked worktrees (`git worktree remove --force --force`).
- Optional, strongest guard against a repeat of the BSOD: set
  `guiApplications=false` in `%UserProfile%\.wslconfig` (disables WSLg).
- FYI: `wsl-pro.service` restarts every 2 s because a cmd.exe AutoRun entry
  points at a missing `E:\Tools\ansicon\x64\ansicon`; harmless noise.
- Optional: disable or schedule WSL auto-updates (Windows Update shut WSL down
  mid-session on 2026-11-22).

Open: `../gbc-32-hardware/docs/firmware-interface.md` says the audio rate is "decided in firmware M6"; ADR 0021 fixed it at 32,768 Hz (agents may not edit that repo).

Decided in session 16 (2026-11-24, M10 review), human:
- `stage1-m9` tagged at 49c0756 on the human's instruction ("Tag m9"); the
  M10 whole-tree review stands in for the M9 milestone review. Open M9
  plan tasks (M8 review lows, real-game checklist) carry over.
- Budget reform ("Reform"): keep the ITCM hot-set fit, the hot-function
  caps and the relative +3% Ir guard as gates; drop the `Hw` size cap;
  total `.text` and cold-function caps become reported numbers, not gates.
  State parked in the wrong struct because of the caps moves back.
- Release profile pinned to `codegen-units = 1` ("Pin to 1"): one
  re-baseline of bench, size and ITCM lists, numbers in the commit.
- `synth:worst-case` gains a serviced timer interrupt (IE=timer + RETI
  handler) once the unobservable timer/serial events are gone ("Add
  serviced IRQ"); baseline re-recorded. The absolute 4.02 M line stays
  until a real M7 measurement.
- STOP mode as implemented (ADR 0020 amendment, 6f228f9), including the
  APU channels holding their tone while stopped (SameBoy CGB path):
  "Thats fine for stop."
- Size caps must be the ones valid for the final MCU (STM32H735IGK6: ITCM
  configurable to 128/192/256 KiB, 32 KiB caches), not the dev board; a
  slower dev board (H753, Stage 2) is acceptable, it is a temporary step.
  Per-function caps and the 57,344 B ITCM hot set become a dev-board
  report; the product gate is total gbc-core `.text` against the H735 ITCM
  (branch `m10-gates-h735`).
- Stage 3 ROM memory is HyperRAM on OCTOSPI, not FMC SDRAM (the human
  referred to it as decided; hardware ADR 0001 not yet updated). ADR 0027
  (byte-wide ROM reads, for the FMC erratum) stays until retired.
- Permission given to edit `acceptance/README.md` item 5 (now: only total
  `.text` gates) and the hardware repo docs for HyperRAM/H735 facts. Done:
  hardware ADR 0003 (HyperRAM on OCTOSPI, supersedes 0001), ADR 0001 status
  line, `firmware-interface.md`, `requirements.md`: left UNCOMMITTED in
  `../gbc-32-hardware` for the human to review and commit.
- Use the hidden LLVM option `-C llvm-args=-cgp-max-address-users-to-scan=100000`
  in every build (bench, size check, firmware): "Sure go ahead and use
  that flag". It lets LLVM fold the hot-struct address in `Cpu::run`
  (measured on the cpu-loop branch: cpu_instrs -18.2% vs -12.3% without,
  thumb `.text` smaller). Tied to the pinned toolchain: re-check on any
  toolchain bump.
- Retire ADR 0027 (byte-wide ROM reads): "Yes retire that ADR". Work
  item 2 under "Remaining".
- Mappers ("Add HuC1 + HuC3"): Stage 1 adds HuC1 and HuC3 (ADR 0016 scope
  change) on top of a safe failure for every other unsupported mapper
  (caller-visible, no RAM). MBC7 and the rest stay out.

Decided in session 15 (2026-11-23), human:
- gbc-core total `.text` budget 97,500 -> 99,000 B ("Raise .text to
  99,000, try") for the last 3 Mealybug ROMs; leftovers come back for
  sign-off (c6c4ea7).
- SameSuite `channel_{1,2}_volume`, `channel_4_align`,
  `channel_1_sweep_restart`: "Sign off all 4" as CGB E results (each
  failing cell is SameBoy's CGB C value). Moved into
  `same-suite-apu-cgbE` (0be5600). FYI: 4 earlier-signed-off cgbE ROMs
  (`channel_4_lfsr_7_15`, `_lfsr_restart{,_fast}`,
  `_frequency_alignment`) now pass on our CGB C model; they stay in that
  group (moving them back would be a manifest change: stricter, the
  human's call).

Decided in session 14 (2026-11-23), human:

- SameSuite `channel_1_freq_change_timing-cgb0BC`: sign off ("Sign off
  target"): it needs the CGB C PCM12 read glitch; a model fitted to it broke
  14 passing SameSuite ROMs that expect CGB E reads. Own stretch group
  `same-suite-apu-pcm-glitch` (22fe162). The frequency-timing half landed
  (91ff0de).
- Mealybug `m3_lcdc_win_en_change_multiple_wx`, `m3_wx_{4,5,6}_change`,
  `win_without_bg`: sign off, no CGB C reference upstream ("yes do #1").
  Own group `mealybug-no-cgb-reference` (d801a6c), still run.
- Desktop: no colour correction; raw colours stay ("dont do that color
  correction"). Copied SameBoy (R) glyph bytes with notice: "fine".
- Display sync (vsync, would supersede ADR 0024's audio-driven pacing):
  "Decide after stall fix".

Decided in session 13 (2026-11-23), human:
- cgb-acid2 bench at +3.37% after the STAT 144/153/0 correctness fixes
  (perf pass recovered what it could, fingerprint identical; worst-case
  4.007 M Ir/frame stays under the 4.02 M line): **rebaseline**
  `perf/bench-baseline.txt` ("Rebaseline (Recommended)"), over raising the
  `point_irq` budget, raising the `Hw` size limit, or backing fixes out.

Decided in session 12 (2026-11-23), human:
- SameSuite group 15: sign off the 9 E-only ROMs (ch1/2 `nrx2_glitch`,
  `nrx2_speed_change`; ch4 `lfsr_7_15`, `lfsr_restart{,_fast}`,
  `freq_change`, `frequency_alignment`) as "CGB E result". "Sign off the
  9". Moved into stretch group `same-suite-apu-cgbE` (cde49e4, baseline
  303bc6d). The other 8 stay fix targets.

Decided in session 11 (2026-11-23), human:
- APU double speed: follow gambatte's CGB C linear trigger rule; sign off
  SameSuite `channel_{1,2}_align{,_cpu}` (4) as "CGB E result" (SameSuite
  apu/README: CPU-CGB-C fails most ch1/2 tests). "Yes". Manifest change
  approved: exclude them from `same-suite-apu`, own stretch group
  `same-suite-apu-cgbE` (keeps running, reported), baseline refresh with
  `--allow-removed`.

- Power-on OBJ palette RAM: sign off gambatte `scx_during_m3_spx2`,
  `scx_during_m3_spx2_ds`, `scx_attrib_during_m3_spx2_ds` as "depends on
  uninitialised palette RAM" (they draw OBJ palette 0 colour 2, never
  written; the cgb04c capture shows that console's power-on 0x21926C; we
  start at 0, deterministic). "Sign off". Manifest change approved: own
  stretch group (keeps running, reported), baseline with `--allow-removed`.

Decided in session 10 (2026-11-23), human:
- gbc-core total `.text` budget 96,000 -> 97,500 B (for mid-line PPU
  steps E/F/G and the M9 fixes; per-function budgets unchanged). "That's
  fine."
- OAM DMA CPU writes during a conflicting transfer (M9 triage groups 2 and
  13, ~65 ROMs): follow gambatte's CGB C hardware captures over SameBoy's
  rule. "gambatte is fine."
- M9 sign-off: the 7 SameSuite APU ROMs that target other CGB revisions
  (`channel_{1,2,4}_extra_length_clocking-cgb0B`,
  `channel_3_extra_length_clocking-cgb0`, `-cgbB`,
  `channel_1_freq_change_timing-A`, `-cgbDE`) are signed off as "targets
  another CGB revision" (ADR 0003: we emulate CPU CGB C). They stay in the
  manifest and keep running; they don't block Stage 1. "Sign off on the 7."
- The 50% bar covers realistic worst cases; dense mid-mode-3 register
  writes (many per line) are outside it, documented in docs/perf.md
  "Outside the bound" with a Stage 2 measurement task. "Yes that is fine."
- `synth:worst-case` gains one mid-line SCX write per line so the
  held-fetch slow path counts against the 4.02 M line (stricter; moves the
  bench baseline). "Do the other thing." Queued after the OAM DMA and
  window-timing work lands.

Decided in session 08 (2026-11-22), human:
- APU perf budget. The exact optimizations brought the APU to 224.1k; the
  human accepted that over the output-changing fallbacks ("Accept, budget
  230k"). After the perf review showed the per-write cost, the budget was
  split ("Split it"): 230k Ir/frame for channels and mixing, plus <= 950
  Ir per APU register write. It stays recorded as over the original 200k
  estimate and is an optimization target (human request; plan M8).
- Total gbc-core `.text` budget = 96,000 B (was 85,100), from the measured full-APU estimate 94,336 B + margin. Per-function budgets stay. Over it later: come back with numbers.

Decided in session 07 (2026-11-22), human: Stage 1 is done only when every
manifest test ROM passes, except groups the human signs off with a recorded
reason (README "Done when", plan M9). Both manifest drafts approved in
advance ("You have my ok") and committed after validation matched
(8543723: PNG-reference group, 198 ROMs; undef_ops lockup opt-in group).

Decided in session 07 (2026-11-22), human, after the M5 test-integrity
review (medium: the lockup rule covered all 3,154 gambatte ROMs, broader
than "those specific tests"): "Narrow to undef_ops". Plan: an explicit
per-group manifest opt-in (e.g. `lockup = "run_on"`), the 10
`gambatte/undef_ops` ROMs in their own group with it, every other group
treats a lockup as `crash` again; contract text + ADR 0018 amendment.
Manifest change drafted for the human's OK; re-review (runner pass/fail
logic changes again).

Decided in session 07 (2026-11-22), human: "Both those sound fine" to
(a) a new stretch-tier group selecting exactly the Gambatte ROMs with a
sibling `_cgb04c.png` (198 ROMs whose names lack `cgb04c`, so
`gambatte-cgb` never selects them; a directory glob would also take ROMs
with no CGB rule). The agent drafts it, the human looks it over before
commit. (b) The total `.text` budget (85,100 B; 84,942 used after M5)
will be raised once for M6, to a figure measured after the first APU
channel lands: come back to the human with the number. Per-function
budgets stay.

Also decided in session 07 (2026-11-22): STOP is 2 bytes unless IE&IF is
pending (SameBoy `stop()`, which encodes the Pan Docs flowchart). The
human first chose "Keep pending cases only", but no SST 0x10 case sets
IF, so that kept 0 of 1000. The human then chose: run all 1000 0x10
cases and compare everything except PC and the bus-cycle list
(registers, flags, SP, IME, RAM), where SST's 1-byte expectation
contradicts hardware. The 5 SameBoy STOP branches have dedicated,
mutation-checked unit tests (ADR 0020). The human also approved loosening the
lockup rule for `gambatte/undef_ops` ("Those specific tests should
probably have looser restrictions then right? If the whole point is to
crash."). Scope chosen: for the `gambatte` check only, an invalid-opcode
CPU lockup is not `crash`; the machine runs on to `run_frames` and the
screen check decides (as upstream testrunner). Panics, stalled time and
lockups elsewhere stay `crash`. The romtest `gambatte`
check may re-encode the hex-digit glyphs clean-room (read GPL
testrunner.cpp for the rules only; own data; ADR 0018). Decided in
session 06 (2026-11-22): the mbc3-tester reference
uses a counted, contract-constrained in-memory recolor (ADR 0007
amendment; a recolor may only relabel a color absent from the reference);
mbc3-tester `run_frames` 41; ADR 0015 extends ADR 0006 and doesn't
change it. Any new `recolor` or manifest change still needs the human.

FYI (session 05): the human saw "a lot of compile errors" (session 05).
build, clippy `-D warnings`, tests, the thumbv7em core build and the
nightly fuzz crate are all clean with the pinned toolchain, so this is
editor-side (likely rust-analyzer using a different toolchain, e.g. a
Windows editor on WSL files, or loading `tools/fuzz/fuzz`, which needs
nightly). Waiting on an example error if it persists.

Context on earlier human decisions:
- Acceptance screenshot fixes: a counted `recolor` on the pinned upstream
  PNG, never a committed replacement image (session 06, ADR 0007).
- `acceptance/test-roms.sha256` is human-owned.
- Strictly-tightening gate/contract changes and factual README fixes are
  agent decisions.
- README quality bar: the core uses at most 50% of an H753 at 480 MHz in
  the worst case.
- Unsafe-free core (ADR 0009).
- The project is MIT-licensed. AGENTS.md has a rule on dependency licenses
  and on not copying code from copyleft emulators.
- Token cost (session 05): agents must be small, one behavior each.
- Remote `origin` = `git@github.com:jschneider1207/gbc-32.git`. The human
  pushes; agents never do.

## Environment

- **Rust:** via rustup (user-level, `~/.rustup` and `~/.cargo`).
  - `cargo` is **not on PATH in non-interactive shells**: prefix commands
    with `source ~/.cargo/env &&`.
  - The toolchain is pinned to 1.100.0. Nightly is installed, but only for
    cargo-fuzz (`tools/fuzz/fuzz/`, outside the workspace; see
    `docs/fuzzing.md`).
- **valgrind** 3.22.0. Benchmark Ir counts are tied to it and the pinned
  toolchain; a change to either is a rebaseline commit.
- **SM83 single-step data:** `scripts/fetch-sm83-tests.sh` →
  `test-data/sm83/`. Set `GBC_REQUIRE_TEST_DATA=1` locally.
- **Desktop system libraries:** installed. Windows target `x86_64-pc-windows-gnu` added to rustup (user-level, session 09) for `cargo check`. WSL2 Ubuntu 24.04, no `jq`.
  Scripts are bash or Elixir, not Python.
- **Real-game ROMs (human):** `X:\gbc-roms` = `/mnt/x/gbc-roms` (all 6 checklist games). Read-only for agents: copy to a scratch dir before any run, never write `.sav` files there, never commit ROMs.
- **Datasheets:** `../gbc-32-hardware/datasheets/` (read-only for agents).
- **Commits:** stage by explicit path only (AGENTS.md Git). Other agents
  and the human commit concurrently.
- **Worktrees:** never point a worktree at another tree's `target/` (`CARGO_TARGET_DIR`): cargo then reuses stale binaries (session 07). Agents in `git worktree`s must not symlink `test-roms`
  (a deny rule blocks removing it); use `romtest --roms-dir`,
  `bench --root`, `TEST_ROMS=` for check-bench. Agent worktrees are
  "locked": removal needs `--force --force` (ask the human). Cherry-picked
  branches need `git branch -D` after `git cherry` shows all `-`.
- **romtest filters** match ROM paths, not group names (e.g.
  `--filter 'mooneye-test-suite/acceptance/**'`).
- **Gate:** `scripts/gate-*.sh` edits prompt the human (`.claude/settings.json`).

## Blockers

None.

## Acceptance snapshot

Baseline 6def429 (`acceptance/results/stage1.json`, session 15; session 14
ended at 3,410; M8 tag `stage1-m8` was stretch 3,126). `scripts/gate-stage1.sh`
passed at 4f32363 (session 15).

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 40 | 40 | |
| stretch | 3,437 | 3,461 | 24 fail, 5 no-reference. Signed off (still run, counted as fail where failing): 7 other-revision SameSuite, 13 failing of 17 in `same-suite-apu-cgbE`, 1 in `same-suite-apu-pcm-glitch`, 3 in `gambatte-uninit-palram`, 5 no-reference in `mealybug-no-cgb-reference`. 0 open fix targets |
