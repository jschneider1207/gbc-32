# 0018: romtest's `gambatte` check

- Status: Accepted
- Date: 2026-11-22

## Context
The contract (`acceptance/README.md`, "Checks") defines `gambatte = true`
by reference to Gambatte's `test/testrunner.cpp`
(pokemon-speedrunning/gambatte-core @ d819bad196) and the howto in
`test-roms/gambatte/`. The check reported `unsupported` until now. The
stretch group `gambatte-cgb` selects 3154 ROMs (`*cgb04c*`; 3151 `.gbc`
and 3 `.gb`).

testrunner.cpp is GPL-licensed. **Human decision (2026-11-22):** it may be
read to learn the rules, but its code and data tables (in particular the
hex-digit glyph table) must not be copied. The glyphs must be re-encoded
clean-room, in our own representation, and only then checked against
testrunner.

## Decision
`tools/romtest/src/gambatte.rs` holds the rules; `checks::gambatte_check`
applies them to the final frame, through the same machine boundary as
every other check (ADR 0007).

**Rules learned from testrunner (for a CGB run):**
- The result tag comes from the extension-stripped name: a name with
  `dmg08_cgb04c_out` uses that; otherwise a name with `dmg08_out` has a
  CGB result only if it also contains `cgb04c_out` (in any order), and
  then uses `cgb04c_out`; otherwise `_out` is used. The value follows the
  first occurrence of the tag. We parse only the file name, not the
  directories (testrunner searches the whole path it is given, which
  depends on how it is invoked; no gambatte directory contains `_out`).
- After the tag: `audio0` = silence, `audio1` = sound; otherwise a run of
  hex digits (either case), ending at the first non-hex character.
- Digit *i* must appear as an 8x8 cell at x = 8*i..8*i+7, y = 0..7, ink
  black and paper white. Nothing else on screen is compared.
- A reference PNG `{stem}_dmg08_cgb04c.png` is used if it exists,
  otherwise `{stem}_cgb04c.png`. A ROM can have both a name result and a
  PNG; then both must pass. (None of the 3154 selected ROMs has a PNG
  today; the path is implemented and unit-tested.)
- Colors: testrunner installs Gambatte's CGB formula for every CGB run,
  including DMG (`.gb`) games running in compatibility mode, and also
  compares the shared `_dmg08_cgb04c.png` against it. The DMG shades
  `#000000 #555555 #AAAAAA #FFFFFF` only apply to testrunner's DMG runs,
  which this project (CGB C only) never does. The formula, from the
  howto, with integer division: R' = (13R + 2G + B) / 2,
  G' = (3G + B) * 2, B' = (3R + 2G + 11B) / 2. Every channel's maximum is
  248, so no clamping is needed; white 0x7FFF is #F8F8F8 and black is
  #000000.

**Judgment calls (all at least as strict as testrunner):**
- *Exact colors.* testrunner compares only the top 5 bits of each channel.
  We compare exactly: digits must be RGB555 0x0000 / 0x7FFF, and a PNG
  must equal the formula output byte for byte. On the first full run, no
  failing ROM would have passed with the 5-bit compare. The failure detail
  says so when one would ("testrunner's 5-bit compare would accept it"),
  so a future case is visible for triage without changing the status.
- *Empty result.* A tag followed by neither digits nor audio0/1 would make
  testrunner compare zero cells and pass. We treat it as a runner error
  (exit 2). The same applies to more than 20 digits (wider than the
  screen).
- *No rule.* A ROM in a `gambatte` group with no CGB result in its name and
  no reference PNG is a runner error (exit 2), also when the ROM crashed
  and its checks did not run (`not_run`). testrunner would run no CGB test
  for it, so selecting it is a manifest mistake. It never passes by
  default.
- *Audio.* `_outaudio0/1` is `unsupported` (with a message) until the core
  has an APU (M6). It is never evaluated as pass or fail.

**How the glyphs were obtained (clean-room).**
1. The parser above was written from the rules. A temporary harness (not
   committed) ran every cgb04c `_out<hex>` ROM on our core for 15 frames.
   For each expected digit it recorded the 8x8 cell that the core drew at
   that digit's position, if the cell was purely black and white and not
   empty, and counted how often each distinct bitmap appeared per digit.
2. For 15 of the 16 digits the most frequent bitmap was clearly dominant
   (e.g. 0: 1092 samples against 107 for the runner-up). Wrong results put other
   digits' shapes in a cell, so the majority is the correct shape as long
   as the core passes most ROMs.
3. B is expected by only one ROM, which the core fails. Its shape was
   taken from the test ROMs' own font: the 16 digit tiles are stored as
   2bpp tiles (both planes equal) at 0x7A00 in the ROMs that print A-F
   (e.g. `speedchange/speedchange2_tima01_nop_2_cgb04c_out0B.gbc`). The
   ROM tiles agree with the majority bitmaps for the other 15 digits.
4. The result is written as `GLYPHS` in our own encoding: one byte per
   row, bit 7 = leftmost pixel, set = ink.
5. Only then was testrunner's table decoded in a scratch directory and
   compared: all 16 glyphs agree. Nothing from it was committed.

The unit tests draw digits from a third, independent description (a
`#`/`.` picture per glyph in `checks.rs`), so the tests do not check
`GLYPHS` against itself.

## Consequences
- The gambatte-cgb group reports real results. First run at 70532df:
  2399 pass, 614 fail, 10 crash, 131 unsupported (all audio), of 3154.
- The 10 crashes are `undef_ops/undef_op_*_dmg08_cgb04c_out01`. The ROM
  executes an undefined opcode, which locks up the CPU, and the runner
  reports a lockup as `crash` (contract, "Statuses") before the expected
  `01` is on screen. Whether those ROMs can ever pass under the current
  lockup rule is a contract question for the human.
- When the APU lands (M6), the audio rule needs a new runner capability
  (the core's audio output across the boundary). Until then 131 ROMs stay
  `unsupported`.
- The pinned release already ships 198 `*_cgb04c.png` references (e.g.
  `scy/`, `bgen/`, `bgtilemap/`, `scx_during_m3/`), but their ROM names
  carry no `cgb04c` tag, so the `gambatte-cgb` glob never selects them and
  the PNG path has only unit-test coverage. Selecting them is a manifest
  change for the human (session 07 review); a directory glob would also
  pick up ROMs with no CGB rule (runner error). Where pixels match in a
  scratch run of 39 of them, the colors were formula-exact, so the exact
  compare fits these references.

## Amendment (2026-11-22): CPU lockup in gambatte groups

**Rule.** In a group whose checks are all `gambatte = true` and whose exit
is `run_frames = N`, a CPU lockup on an invalid opcode is no longer a
`crash`. The run goes on to frame N and the gambatte check decides on the
final frame as usual: the right screen is `pass`, a wrong one `fail`. The
JSON `detail` records the lockup ("CPU locked up (PC=...) at clock ...;
ran on to frame N"). Everything else stays `crash`: a lockup in any other
group (another check kind, a mix of kinds, another exit), a panic, a
rejected load, stalled emulated time and a late input change. The
contract text is in `acceptance/README.md` ("Gambatte lockup rule").

**Why.** The 10 `undef_ops/undef_op_*_dmg08_cgb04c_out01` ROMs execute an
invalid opcode on purpose. On hardware the CPU hangs while the LCD keeps
running, so the `01` drawn earlier stays on screen (Pan Docs, "CPU
Instruction Set": the invalid opcodes lock up the CPU). Gambatte's
testrunner runs a fixed 15 frames whatever the CPU does and then reads
the screen. Under the old rule these ROMs could never pass, whatever the
emulator did.

**Core.** No change needed. The core already models the lockup this way:
`Mode::Locked` idles the CPU, interrupts don't wake it, `CPU_LOCKED`
fires once, and `run_until` keeps advancing time with the PPU and timers
running (`gbc_core::Events::CPU_LOCKED`; `cpu::exec` test
`invalid_opcode_locks_once`). The runner just keeps calling it.

**Approval.** This loosens a check, so it needed the human. The human
approved it on 2026-11-22: "Those specific tests should probably have
looser restrictions then right? If the whole point is to crash." The
scope (gambatte groups with a `run_frames` exit only) was set narrowly so
that a lockup under the serial, screenshot and fibonacci checks, where it
means the emulator went wrong, still fails loudly.

## Amendment 2 (2026-11-22): the lockup rule is a per-group opt-in

**Why.** The M5 test-integrity review found the first amendment broader
than approved: it applied to every all-`gambatte` group with a
`run_frames` exit, so it covered all 3,154 `gambatte-cgb` ROMs, while the
human only meant the 10 `gambatte/undef_ops` ROMs. The human chose to
narrow it (decision recorded in `docs/STATUS.md`, 2026-11-22).

**Decision.** Run-on depends only on an explicit group key,
`lockup = "run_on"` (default `"crash"`). The first amendment's scope
becomes validation: the key is accepted only on a group whose checks are
all `gambatte = true` with a `run_frames` exit, otherwise the manifest is
bad (exit 2). Every group without it, including `gambatte-cgb`, scores a
lockup as `crash` again. The undef_ops ROMs are meant to move to their
own opt-in group, with `gambatte-cgb` excluding them.

**Keeping the groups apart.** romtest already allows a ROM in several
groups (results are keyed by group and ROM, and it runs once per group);
the manifest keeps groups apart with `exclude` (as the mooneye groups
do). To stop an exclude from rotting silently, e.g. after a rename that
would let the undef_ops ROMs back into `gambatte-cgb` as crashes or into
both groups, an `exclude` glob that removes no ROM the group's `roms`
match is now a bad manifest (exit 2). This only makes the runner
stricter; every existing exclude in `stage1.toml` removes ROMs.

**Manifest.** `acceptance/stage1.toml` is human-owned, so the new group
and the exclude were drafted as a diff for the human, who approved them
in advance ("You have my ok", 2026-11-22); committed in 8543723 after
validation (10/10 undef_ops pass in their own group, none left in
`gambatte-cgb`).

## Amendment 3 (2026-11-22): the `_outaudio0/1` rule

**Rule.** With the APU in place (M6, ADR 0021), the audio results are
judged instead of `unsupported`. A group with a `gambatte = true` check
captures the last frame of its run: every run call before
`limit - FRAME_CLOCKS` (the exit's clock limit minus one frame) gets an
empty audio buffer, one call ends exactly there, and from then on the
calls share one buffer of `(FRAME_CLOCKS / AUDIO_SAMPLE_CLOCKS) + 2` = 550
slots (ADR 0021 section 10). The window is therefore `[start, end)` with
`start` the first clock at or past `limit - FRAME_CLOCKS` and `end` where
the run stopped (at or just past `limit`); the core's window grid restarts
at `start` (ADR 0021 section 9). Silent means every returned sample
equals the first one, both sides compared, at any constant level (DC is
silence). `audio0` passes when silent, `audio1` when not. A ROM with both
an audio result and a reference PNG must pass both.

**Failure modes, all strict.** A run that never reached the last frame,
or ended inside it (`end < limit`, e.g. an `ld_b_b` exit or a crash),
fails, as does an empty capture. Filling all 550 slots means samples may
have been dropped (the core would stop on `AUDIO_BUFFER_FULL` and the
rest of the frame would get no room), which cannot happen at 128-clock
windows and is a runner error (exit 2).

**Deviations from testrunner** (read for the rule only, nothing copied):
- *Window position.* testrunner runs chunks of up to 35,112 two-clock
  samples (`runFor` returns early at each video frame) while
  `samplesLeft = 35112 * 15` stays >= 0, so about 16 frames, and judges
  the first 35,112 entries of the buffer after the last chunk. When that
  chunk ended early at a frame boundary, the tail of the buffer still
  holds the previous chunk's samples. We judge exactly the last
  70,224 clocks of the manifest's run (`run_frames = 15`), the same run
  the screen verdict uses, and only samples produced in it. Same length,
  a frame earlier, no stale data.
- *Resolution.* testrunner's samples are 2-clock points; ours are
  128-clock averages (ADR 0021 section 8). ADR 0021 section 10 names the
  blind spot (a tone whose period divides 128 clocks averages to a
  constant). **Cross-check (session of this amendment):** in a scratch
  worktree with `SAMPLE_PERIOD_LOG2 = 1` (2-clock windows, Gambatte's
  resolution) all 131 `_outaudio` verdicts were identical to the 32,768 Hz
  run (98 pass, 33 fail in both). At 2-clock windows the run's overshoot
  past `limit` (up to one instruction) can span several windows, so that scratch run needed 64 slots of
  headroom instead of 2 (it reported the full-buffer runner error with
  2); at 128-clock windows the overshoot is under one window. So no
  test-only rate option is needed for the pinned ROMs.

**Results (`--filter 'gambatte/**'`, 3352 group/ROM runs):** before,
2599 pass, 622 fail, 0 crash, 131 unsupported; after, 2697 pass,
655 fail, 0 crash, 0 unsupported. The 131 audio ROMs: 98 pass, 33 fail.
No other ROM's verdict changed (all 3221 compared). With this rule no
check produces `unsupported` any more; the status stays in the contract
for future check kinds.

**Review.** This changes the runner's pass/fail logic (AGENTS.md
"Independent review", rule 2). The M6 milestone test-integrity review
covers it; no result from the audio rule is relied on (baseline update,
gate, milestone tag) until then.

## Amendment 4 (2026-11-22): the audio capture must be long enough

Stricter only (M6 review low). The capture window `[start, end)` yields one
sample per `AUDIO_SAMPLE_CLOCKS` (128), so a capture with fewer than
`(end - start) / 128` samples (integer division) is a runner error, not a
silence verdict: a truncated capture could otherwise fake `audio0`. The
bound is exact for a complete capture (`floor(x) - floor(y) >= floor(x - y)`),
so no correct run falls under it. The full manifest shows no status change
from it. Report only (no status, score, baseline or exit-code change): the console
summary and the Markdown report add a line "audio: N / M pass (audio1 X,
audio0 Y; audio0 passes are weak evidence: a silent APU passes them)", since
a constant APU passes 73 of 131. Current: 105 / 131 (audio1 35, audio0 70).
