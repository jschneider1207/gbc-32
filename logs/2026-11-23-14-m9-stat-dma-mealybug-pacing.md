# 2026-11-23-14: M9 STAT/DMA/Mealybug fixes, sign-offs, desktop pacing

- Milestone: M9
- Commits: da13f6f..(this log)

## Done
- gambatte `m0enable` 11: CGB mode-0 IRQ uses STAT selects/LYC latched a
  few dots before the HBlank point (1ef1e28; stat-irq.md, ADR 0029 amendment).
- Square NRx4 write within 4 clocks of a duty step sets the running
  period (91ff0de). `channel_1_freq_change_timing-cgb0BC` still needs the
  CGB C PCM12 read glitch: human signed it off (22fe162).
- OAM DMA 5: dispatch pushes PC in M-cycles 4-5 (9fb207f); restart tail
  (e4f54b6, 30609c3); OAM DMA during a VRAM DMA takes the VRAM DMA's byte
  (71f747a, gambatte cgb04c); speed switch holds the last step (f4dad64).
  ADR 0017/0028 amendments.
- Double speed line-0 mode-2 pulse at t = 5 and LYC=0 at 153:13 (b309904,
  +4 incl. one lcd_offset).
- Mealybug 8: boot ROM logo/(R) tiles left in VRAM (a044342, f53c49c;
  SameBoy notice added b965ceb), LCDC.2 per object bit plane (d015465).
  Leftover groups C-F written up in `docs/design/mid-line-ppu.md`.
- Human sign-off: 5 Mealybug ROMs without a CGB C reference (d801a6c).
- Code size pass -1,416 B, no behavior change, worst-case -0.22% (core-size
  branch, e2c908e).
- Desktop: `GBC_FRAME_STATS=1`; pacing on a smoothed audio fill (WASAPI
  10 ms chunks made frames land 10/20 ms apart); `hitch:` lines; save
  writes on a `SaveWriter` thread (ADR 0023/0024 amendments; 31905eb,
  bc07793).
- Baseline 186d4bc: stretch 3,376 -> 3,404 (must/should full). Full gate
  passed at bc07793.

## Real-game checks (human, native Windows)
- Wario Land 3: looks/sounds right; saves survive Esc and Ctrl+C; audio
  device switch recovered twice. Smoothness: choppy before the pacing fix,
  "much better" after, still slightly behind SameBoy (vsync off there too).
  Remaining gaps are whole-frame multiples with no wake lateness; the agent
  showed the game blanking the LCD during boot (22 of 23 frames blank).
- Shantae: plays well; long run on e2c908e clean (p1 ~16.0 / p99 ~17.5 ms).
- Human decisions: no colour correction; vsync decision deferred until
  after the stall fix.

## Learned / surprises
- The Windows timer theory for choppiness was wrong (winit 0.30 already
  uses a high-resolution wait); the cause was the audio fill's 10 ms steps.
- Frame-multiple gaps with tiny wake lateness = LCD-off blank frames, not
  stalls. The fingerprint hashes VRAM, so boot-tile changes alter 3,490 of
  4,020 runs; compare with 8010-819F excluded.
- A shared `flock` (shared for builds, exclusive for bench/fingerprint/
  gate) kept heavy steps alone across parallel agents.

## Review
- None run this session (no milestone close; no runner pass/fail change;
  no core public API change). M10 notes in STATUS: `obj_size_fetch`
  temporary VRAM byte swap; boot-tile setup.

## Left open
- 33 M9 fix targets (STATUS Next up).
- Desktop smoothness vs SameBoy: present-path jitter or display sync.
