# 2026-11-27-30: stage2-comprehensive-review

- Milestone: Stage 2 (post-M7, awaiting sign-off)
- Commits: 1ce5986..(this commit)

## Done
- Human asked for a comprehensive Stage 2 review (correctness, performance,
  optimizations, Rust quality); four fresh read-only reviewers, recorded in
  `docs/reviews/stage2-comprehensive.md` (1ce5986).
- Human: "go ahead with all of them, use your judgment". Fix agents, one
  theme each: protocol (K1, K2, K12, Q14), scaler (P1), gate lints (Q3,
  Q10), host lows (K3, K5-K7, K14, Q12, Q15, Q16), PC client (K4, K8),
  gbc-link-board crate (Q2, ADR 0043), PLL2 to clock.rs (Q7), neutral
  audio frame (Q1, ADR 0044, protocol 2), take_once!/rcc::modify (Q4, Q6),
  qspi/nor split (Q8), hard-rules helper (Q13), fused DAC pop (P5), bin
  dedup (Q9). Every new test seen failing.
- Host-only gate caught Q1 renaming two H7 named tests; restored both
  names as end-to-end tests (bbbee09, 3/3 mutants). Fixed an rustfmt
  module-order slip (8c56c54).
- Full gate with the board exit 0 on e8b8efd. Board measurements
  (983e9ae, docs/perf.md "Session 30 review fixes"): scaler 196.7k ->
  118.7k; live fe mean -28..-32%; audio_isr_max 8.4k -> 2.8k (tee 10.5k ->
  5.3k); K2 verified (board self-timeout, then a second play --audio got
  audio).

## Learned / surprises
- The reviewer's 85-100k scaler estimate was optimistic (118.7k measured).
- Fusing the DAC conversion into the pop mattered: the first Q1 version put
  a 1 KiB zeroed scratch half on the ISR stack (1,088 B frame vs 80 B now).
- Renaming a named test silently loosens H7 unless the name returns; the
  gate caught it.

## Review
- Trigger: human request (whole Stage 2). Outcomes per row in
  docs/reviews/stage2-comprehensive.md. No high correctness findings; two
  mediums (K1, K2) fixed. Rejected: none. Deferred: plan "Stage 3
  carry-over tasks", comprehensive-review block (K10, K11, K13, P2, P3, P6-P8,
  M1-M5, Q5, Q9/Q11/Q15/Q17/Q2/Q13 rests).
- Not re-reviewed: Q2 and Q1 change the core<->platform boundary only in
  the firmware crates (not gbc-core's API); runner pass/fail logic untouched.

## Left open
- Needs human: K6 on Windows, Q13 gbc-core dev-dep, sign-off item 2 call.
- Pokemon Red core_max +1.7% (one frame), accepted as layout noise.
