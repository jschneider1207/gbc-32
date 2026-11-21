# 2026-11-21-01: m0-workspace-runner

- Milestone: M0 (closed, tagged `stage1-m0`)
- Commits: 99b7b5a..HEAD (d2b9658 was a concurrent human-side commit that swept in half-written workspace files; 99b7b5a restored a building main)

## Done
- Toolchain pinned (1.100.0), workspace (`gbc-core`, `gbc-desktop` stub, `romtest`).
- `gbc-core` skeleton + public API (ADR 0006, amended twice the same day):
  `Emulator<'a>` with in-place `init`, `load`/`eject` that always hand buffers back,
  per-call framebuffer/audio, `Events` bitset, `screen_blank`, debug accessors.
- `romtest` implementing the full contract except `gambatte` (ADR 0007, amended):
  `Machine` trait + fake, integrity list `acceptance/test-roms.sha256`, strict
  baseline rules, narrow panic boundary, `--roms-dir`, `--allow-removed`.
- `crates/gbc-core/tests/hard_rules.rs` enforces no_std/no-alloc/no-deps beyond the gate's grep.
- Initial baseline (everything failing). Gate fails at the ROM step with romtest exit 1.

## Review
- Core API review (1 reviewer): 13 findings. Fixed 1,2,3,7,9,10,12. Deferred to plan
  tasks 4 (pixel format, M3), 5 (M-cycle inputs, M2), 6 (bus context, M1), 8 (RTC advance, M4),
  11 (ROM mask, M1). 13 (d2b9658 doesn't build) can't be fixed without rewriting history.
- romtest pass/fail review (1 reviewer): 12 findings. Fixed 1 (integrity list, missing pass
  entries), 2 (gate needs baseline), 3 (panic boundary), 4 (final frame after LCD off),
  5 (no-reference precedence), 6, 7, 11, 12. Rejected: 8 (score formula undefined for an
  empty tier, documented), 9 (an exclude typo only makes runs stricter), 10 (covered by M2 task).
- M0 close, test integrity: 8 findings. Fixed 1 (any vanished baseline key fails a full run),
  2 (hard-rules test; gate-script change → Needs human), 4, 6, 7, 8. Rejected 3 (milestone
  commands rely on partial gating; documented in ADR 0007) and 5 (documented).
  Integrity-list ownership → Needs human.
- M0 close, hardware: 8 findings. Fixed 1 (first frame after LCD on is blank), 2 (FRAME_READY at
  LY=144), 3 (RTC masking), 5 (ROMs < 32 KiB). Deferred to plan: power-on PPU phase (M3),
  STOP blanking (M5), cart table gaps (M4), DMG-compat boot regs and FEA0 reads (M1).
- No perf review: required from M3 on.

## Learned / surprises
- `cargo` is not on PATH in non-interactive shells: `source ~/.cargo/env`.
- The stub core runs the full manifest (~2.2 G idle steps) in seconds; the full run went
  from ~3 s to ~10 s after the review fixes (hashing adds ~0.4 s wall; the rest is not
  investigated, irrelevant until there's a real CPU).

## Left open
- Needs human: integrity-list ownership, gate-script hardening, contract text in
  acceptance/README.md (see STATUS).
