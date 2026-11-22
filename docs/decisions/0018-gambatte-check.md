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
- If a future test-ROM release adds `_cgb04c.png` references, they are
  compared exactly in Gambatte's colors. If they turn out not to be
  formula-exact, the exact compare will show it as failures, not hide it.
