# 2026-11-23-15: M9 APU group 15, Mealybug C/D/E, lcd_offset, TILE_SEL glitch

- Milestone: M9
- Commits: b217092..(this log)

## Done
- SameSuite group 15: sweep overflow check `shift` 1 MHz ticks after the
  write-back (b217092); CH4 trigger keeps the free-running counter with
  SameBoy's start delay, CGB-C branches (49ff774, ADR 0021 amendment).
  3 fixed + 4 already-signed-off cgbE ROMs now pass. Human signed off
  ch1/2 `volume`, ch4 `align`, ch1 `sweep_restart` as CGB E (0be5600).
- Mealybug C/D: BG fetch keeps its dots through an object stall
  (`tile_lead`), window tile 0 from its activation (b0470fd..632d083):
  `m3_scy_change`, `m3_lcdc_win_map_change` pass.
- lcd_offset 3: old LY=LYC bit to dot 2 on lines 144-152; line-0
  single-speed pulse scheduler deadline one dot late (13be1bd, 4b091ef).
- lcdoffset1 CPU access 3: ds palette block from dot 86, ss OAM-write
  block from t = 3 (gambatte CGB rules; 90b1bc7).
- `scx_during_m3/scx_0367c0` 4: first tile's map column has no SCX borrow
  (4b86fcd; hot span path, +0.03% worst-case).
- TILE_SEL glitch: LCDC.4 clear on a plane read dot -> tile index as data
  (96995df: `cgb-acid-hell`, AGE `m3-bg-lcdc`); set -> latched last
  8000-addressed byte (a93a6b1: Mealybug `*_tile_sel_change2`,
  `*_tile_sel_win_change2`).
- Speed switch: trigger phase follows the post-switch APU frame-sequencer
  delay (bbad441; gambatte `speedchange_ch1_..._timing_2`).
- Baselines 0656b07, 0be5600, 6def429: stretch 3,410 -> 3,434. Full gate
  passed at 0be1ad0.
- Human: total `.text` budget 97,500 -> 99,000 B for the last three
  Mealybug ROMs (c6c4ea7).

## Learned / surprises
- 4 ROMs signed off in session 12 as "CGB E only" pass on our CGB C model
  once the CH4 trigger phase is right: sign-off evidence can be wrong.
- bbad441's 1 MHz phase shift under the ds delay is inferred from one ROM
  pair (AGE only delays the frame sequencer); DIV-based rule regressed
  SameSuite `channel_{1,2}_duty`. M10 review item.
- The old "LCDC.4 set lands on time next to an object stall" calibration
  was really the set glitch; it is gone from the timing.
- Worst-case bench is ~4.003 M Ir/frame (99.6% of the 4.02 M line), not
  ~3.98 M as STATUS said mid-session.

## Left open
- `m3_lcdc_tile_sel_change`, `_win_change` (8 px, latch carries over
  lines), `m3_wx_4_change_sprites` (CGB WX-match glitch): agents in
  flight at log time (branches `m9-tile-sel-latch`, `m9-cgb-wx-glitch`).
