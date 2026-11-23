# 2026-11-23-12: m9-edges-window

- Milestone: M9 (in progress)
- Commits: eb0dc3b..5ad73ed (+ this log/STATUS)

## Done
- M10 added to the plan (human request): in-depth Stage 1 review on Fable,
  four dimensions (correctness, perf, size, idiomatic Rust); part of the
  stage gate (eb0dc3b).
- AGE `spsw-interrupts-cgbBC` (1543bd8): CGB B/C hold the DIV reset 4 T
  also with an interrupt pending and IME on (AGE source `OFS_B`; we had the
  CGB E table, SameBoy's model). ADR 0020 amendment. Should tier 40/40.
- `ppu-edges-step1b` + `hdma-trigger` landed via option (b), plus design
  steps 3, 7 (VRAM part) and 10 (95e948d): +51/-0. The HDMA period needed
  no change: the lcdoffset3 regression was the single-speed VRAM block start
  (84 -> 86), the ds swap was the ds compare point (t=4 -> 5) plus 4 ds
  write-view constants.
- Per-speed LYC walk compare point removed (2c1df77), fingerprint identical,
  -206 B.
- Human sign-off: 9 SameSuite APU ROMs as CGB E results (cde49e4, 303bc6d).
- Stalls at pixel 159 don't delay the HBlank IRQ (88a5b41): +10.
- Walk `End` follows mode-3 writes that move the line end (101f752); found by
  the pixel-159 agent; fingerprint identical, debug_assert reproduced.
- Late WY writes (ea5e484): last WY check at dot 455, WY_LEAD 5 after the
  discard, pre-pipeline mode-3 write handed to `latch`: +5.
- Baselines: b59d9a1, 303bc6d, 208f522, 5ad73ed. Gate passed at 95e948d.

## Learned / surprises
- Triage "garbage registers" for AGE ROMs = AGE's normal failure exit (B=0);
  dump the result table before assuming divergence.
- The 5 ds late-WY ROMs look like a ds LYC=153 sync problem (3 dots), not a
  WY rule; their ds_lcdoffset1 siblings agree with single speed.
- `diff-fingerprint.sh` in a worktree needs `--root <test-roms>` (ignores
  TEST_ROMS).
- `git push` is denied for agents by permission settings; the human pushes.

## Left open
- Running: ds LYC=153 sync (design step 5) for `lyc153int_m2irq_*_ds_1` and
  the 5 ds late-WY ROMs.
- Group 4 left (16): the 5 ds late-WY, late_enable_afterVblank, late_wx_ff,
  re-trigger (on_screen/*, Mealybug m3_lcdc_win_*, m3_wx_4_change_sprites).
- `.text` 96,306 of 97,500 B: little room left for M9.
