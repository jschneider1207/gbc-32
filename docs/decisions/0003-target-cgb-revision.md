# 0003: Emulate CPU CGB revision C

- Status: Accepted (confirmed by human 2026-11-21)
- Date: 2026-11-21

## Context
CGB chip revisions (0, A–E) differ in small ways, and the test suites
publish expected results for specific revisions. We need one target.

| Suite | Revisions with references |
|---|---|
| Blargg | CGB B, C, E (cgb_sound fails on B) |
| Mealybug | CGB C (27 screenshots), CGB D (20) |
| Gambatte | CGB C (`cgb04c`) |
| AGE | CGB B/C and E variants |
| SameSuite APU | some tests are CGB E only |

## Decision
Target CPU CGB C. It has the widest reference coverage (Mealybug, Gambatte,
AGE `cgbBC*`, Blargg). The few CGB-E-only SameSuite APU tests are tier `stretch`.

Alternatives considered: B (fails Blargg `cgb_sound` on real hardware),
D (only Mealybug references), E (gains the E-only SameSuite APU tests but
loses Mealybug and Gambatte references), 0/A (rare, almost no references).
Revisions are not configurable: one fixed target avoids per-revision code
paths that games never need.

## Consequences
Games don't depend on the differences between C and E, so no player-visible
impact. The manifest uses `model = "cgb-c"` and CGB-C references throughout.
