# 0033: Interrupt requests with a later IF clock are explicit pending state in the PPU

- Status: Accepted
- Date: 2026-11-24

## Context
Two STAT interrupt rules (ADR 0029, `docs/reference/stat-irq.md`) need a
request to reach IF later than the write or walk point that makes it,
both in CGB single speed:

1. A LYC write's request reaches IF 5 dots after the write lands
   (gambatte `memevent_oneshot_statirq`).
2. Line 0's mode-2 pulse is walked at dot 4, and its request reaches IF
   at dot 5.

Both were encoded indirectly while `Hw` and the PPU functions had byte
caps (dropped 2026-11-24). (1) lived in IF's unused bits 5-6 (pending,
M-cycle parity) with the PPU deadline lowered by hand: every interrupt
check had to mask IF with `INT_MASK`, `Bus::set_interrupt_flags` dropped
the request, `Bus::interrupt_flags` exposed the bits, a PPU write in the
next M-cycle rescheduled the PPU past it, and turning the LCD off left it
waiting (Stage 1 M10 review, A2-04, S1, L10, E4). (2) was only a PPU
deadline one dot past the walk point: a sync on the point itself posted
the request a dot early, the one known difference of the eager-sync
check (`scripts/check-sync-invariance.sh`, `KNOWN_EAGER`).

Review E4 proposed a typed scheduler event `Irq { at, bits }` in `Sys`.

## Decision
A request that reaches IF later is state of its own: "IF bits B are set
at clock T" (`irq::PendingIrq`: up to two requests, earliest first).

- **Owner: the PPU** (`Ppu::irq`), not a scheduler slot. `Ppu::next_event`
  is the earlier of the walk's next event and T, so the existing
  `EventKind::Ppu` deadline covers it; `Ppu::sync` to T or later posts
  the bits. The line-0 pulse makes its request inside the walk
  (`Ppu::walk_to`); the LYC write's is made by the bus
  (`Ppu::request_irq`).
- **IF holds bits 0-4 only.** `IE & IF` is what is pending without a
  mask; `INT_MASK` remains only where IF is written.
- **Rules.** Until T the request is not in IF: an IF write,
  `Bus::set_interrupt_flags` and the dispatch's acknowledge neither see
  nor drop it. From T on it is an ordinary IF bit. Turning the LCD off
  drops waiting requests (gambatte `LCD::lcdcChange` disables every LCD
  event; behaviour only). Nothing else cancels one.
- **Sync independence.** Every IF observer already syncs the PPU to its
  own clock when the PPU deadline has passed (the boundary's event before
  an interrupt check or IF read, an IF write at its landing dot, the
  acknowledge at the middle of its M-cycle), so IF at a clock is a
  function of the clock and the writes. `KNOWN_EAGER` is empty.
- One caller syncs without posting: palette data writes use
  `Ppu::walk_to`. They look at the PPU's position, not at IF, and the
  deadline (at or before T) stays due, so the next real sync posts.

Why not the scheduler slot: the pulse's request is made inside the PPU
walk, which any PPU register access can run, so a slot in `Sys` would need
a check after every sync to pick the request up (the same per-sync cost),
plus a second deadline test at each mid-M-cycle IF observer (IF write,
acknowledge) that today tests only the PPU's. PPU-owned state costs no
more than the slot at any site and needs no new dispatch-order entry
(`EventKind` order is emulated behaviour, ADR 0011). The type itself
(`irq::PendingIrq`) knows nothing of the PPU; another lazily synced
component with the same need can own one the same way.

Line 153's LYC=0 point (also a dot before gambatte's event) could use the
same request and drop `Ppu::early_point`'s IF-write case. It is a
behaviour change for an IF read or interrupt check on 153:12 with no ROM
(review L5); not done here.

## Consequences
- The three A2-04 hazards and the LCD-off one can't happen; each has a
  unit test, seen failing on the old code
  (`bus::ppu_io::tests::lyc_write_request_*`, `lcd_off_cancels_a_lyc_write_request`).
  Three are behaviour fixes outside the corpus (the fingerprint of all
  4,020 runs is unchanged, with and without `--audio-buffer 7
  --hash-samples`); the fourth (the raw accessor exposing bits 5-6) had
  no observable effect.
- Removed: `LYC_IRQ_PENDING`/`LYC_IRQ_PHASE`, `lyc_irq_delay`,
  `lyc_irq_due`, the masks at the interrupt checks, the late pulse
  deadline in `next_event`, the pulse's case in `write_if`.
  Still there (review A2-02): `MODE_HOLD`, `MODE_1_STICKY`, `m0_latched`,
  `early_point` for line 153's IF writes and for a STAT write's phase 2
  on the pulse (with its IF undo).
- `write_if` reschedules the PPU after its sync (review L10).
- Cost (vs main, measured the same day): Ir cpu_instrs 0.00%, cgb_sound
  0.00%, cgb-acid2 -0.02%, event-storm 0.00%, dma-storm +0.02%, halt-irq
  -0.47%, worst-case -0.33%, cgb-raster +0.04%, apu-writes -0.17%; `.text`
  97,136 -> 97,510 B; `size_of::<Emulator>()` 848 -> 872 B (thumbv7em).
  With the post inside the palette writes' inlined sync, cgb-raster was
  +0.75%, hence `walk_to`.

## Update (2026-11-24)
Line 153's LYC=0 point uses the same request now (review L5): walked at
12, its request reaches IF at 13 (`edges::L153_ZERO_IRQ`;
`docs/reference/stat-irq.md`). `Ppu::early_point`'s IF-write case and
its branch in `Ctx::write_if` are gone; what is left of it is
`Ppu::on_ly0_pulse`, for a STAT write's phase 2 on the pulse. Unit tests
`bus::ppu_io::tests::line_153_lyc_0_request_reaches_if_a_dot_after_its_point_however_synced`
and `ppu::tests::single_speed_line_153_lyc_0_request_is_a_dot_after_its_point`,
seen failing without the delay. The fingerprint of all 4,020 runs is
unchanged (no ROM has an M-cycle boundary on 153:12 that looks at IF).
The decision above is unchanged.
