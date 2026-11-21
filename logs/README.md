# Session logs

One file per working session: `YYYY-MM-DD-NN-slug.md` (NN = session number
that day). These are append-only history and **are not read by default**.
`docs/STATUS.md` is the source of truth for current state. Read a log only
when you need the history behind something.

Keep each log short: 10–30 lines.

```markdown
# YYYY-MM-DD-NN: slug

- Milestone: M<N>
- Commits: <first>..<last>

## Done
- ...

## Learned / surprises
- Hardware behavior, tooling gotchas, dead ends (and why they were dead).

## Review (only if a review ran this session)
- Trigger + reviewers run; each finding: fixed (commit) / rejected (one-line reason)

## Left open
- ...
```
