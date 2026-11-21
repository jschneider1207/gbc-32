# 0004: Acceptance by deterministic scripts and human-owned manifests

- Status: Accepted
- Date: 2026-11-21

## Context
The project is executed largely autonomously by an AI agent, with the human
as PM/architect/final verifier. "Done" therefore has to be objective,
reproducible and impossible to quietly redefine.

## Decision
- Acceptance criteria are data (`acceptance/*.toml`) encoding the upstream
  pass/fail rules of each test suite. The human owns these files.
- The agent-built runner (`tools/romtest`) implements the contract in
  `acceptance/README.md`. Results are deterministic.
- Per-stage gate scripts (`scripts/gate-stage<N>.sh`) combine lint, tests,
  the embedded build check and ROM acceptance. Exit 0 means pass.
- A committed baseline makes regressions fail the gate.
- Stages also need human sign-off on real games, which can't be scripted.

## Consequences
- Milestones can close without the human. Stages can't.
- Any wish to "exclude a flaky test" goes through the human.
