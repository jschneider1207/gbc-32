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

## Second round (human: "are you going to do the remaining tasks?")
- Human signed off hardware item 2 (audio) and approved gbc-core's
  dev-dependency on tools/hard-rules (Q13, d2cfb51).
- Done: K10 1aace5a, K11 (landed in bea81df via another agent's commit,
  see below), K13 b48b149, Q11 8974629, Q15 df3d99c, Q2 rest 54db549,
  Q9 fold c2aac36, Q17 27421de, M3/M4-bench 6a14794, P2 16f88a3,
  M4-player bd38750, dac-probe e59c536, P3 4df60a8, M2 892efe3,
  M1 9a3cba4 (ADR 0045), P6 rejected (6c974e7), H8 B1 stack line a22f12e.
- Board: final full gate exit 0 on 2838849; K10 Shantae 4 MiB live +
  audio 0 underruns; K13 paced cgb_sound PASS, sleep_gate_te=203,
  sleep_keep_te=7900234 (no timeout); player re-flashed.

## Learned / surprises (second round)
- The git index is shared: bea81df (a docs commit) swept another agent's
  staged K11 hunks. It happened to be complete and built; no rewrite.
  Agents now stage and commit in one command (memory updated).
- An agent's `until ! pgrep -f "x"` wait loop matched its own command
  line and never ended; killed by the orchestrator.
- H8's B1 stack line was empty because grep saw a NUL in the capture and
  treated the file as binary.
- align-loops=1 frees 4.7 KB ITCM but costs up to +5.9% (cgb-raster).
