# Architecture Decision Records

One file per decision: `NNNN-short-slug.md`, numbered sequentially and
append-only. To change a decision, write a new ADR that supersedes the old
one, and mark the old one `Superseded by NNNN`. Never rewrite history.

Write an ADR when a decision is non-obvious, would be expensive to reverse,
or is something a future session might reasonably question.

Statuses: `Proposed` (waiting on the human; work may proceed on this
assumption), `Accepted`, `Superseded by NNNN`.
Changing an Accepted ADR's decision requires human approval (AGENTS.md).

## Template

```markdown
# NNNN: Title

- Status: Accepted
- Date: YYYY-MM-DD

## Context
What forces are at play; what problem needs deciding.

## Decision
What we're doing.

## Consequences
What becomes easier/harder; what we're giving up; follow-ups.
```
