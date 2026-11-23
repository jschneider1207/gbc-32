#!/usr/bin/env bash
# Prove a change is behavior-identical to an earlier revision (docs/perf.md,
# ADR 0011 "Verification").
#
#   scripts/diff-fingerprint.sh <rev> [bench --fingerprint args, e.g. --seconds 5]
#
# Builds tools/bench at <rev> in a temporary git worktree and the current
# working tree, runs `bench --fingerprint` on both with the same arguments,
# and diffs the per-ROM hashes. Exit 0 iff identical.
#
# A ROM run on only one side (the corpus changed between <rev> and now) is
# reported as CORPUS lines and fails by default; --allow-corpus-change
# (a script flag, not passed to bench) compares only the common ROMs and
# still reports the one-sided ones.
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

[ $# -ge 1 ] || { echo "usage: $0 <rev> [--seconds N] [--root DIR]" >&2; exit 2; }
rev=$1
shift
allow_corpus=0
rest=()
for a in "$@"; do
  if [ "$a" = --allow-corpus-change ]; then allow_corpus=1; else rest+=("$a"); fi
done
set -- "${rest[@]+${rest[@]}}"
git rev-parse --verify --quiet "$rev^{commit}" >/dev/null || { echo "diff-fingerprint: unknown rev '$rev'" >&2; exit 2; }

# The new side is the working tree, not HEAD: say so loudly if they differ.
dirty=$(git status --porcelain -- crates tools Cargo.toml Cargo.lock)
if [ -n "$dirty" ]; then
  echo "diff-fingerprint: WARNING: uncommitted changes under crates/, tools/ or Cargo.*:" >&2
  echo "$dirty" >&2
  echo "diff-fingerprint: WARNING: the new side is the WORKING TREE, not HEAD" >&2
fi

# test-roms/ is gitignored, so the worktree's run reads the main tree's.
args=("$@")
case " ${args[*]} " in
  *" --root "*) ;;
  *) args+=(--root "$PWD/test-roms") ;;
esac

tmp=$(mktemp -d /tmp/diff-fingerprint.XXXXXX)
cleanup() {
  git worktree remove --force "$tmp/old" >/dev/null 2>&1 || true
  rm -rf "$tmp"
  git worktree prune
}
trap cleanup EXIT

git worktree add --detach "$tmp/old" "$rev" >/dev/null 2>&1

echo "diff-fingerprint: building $rev and the working tree" >&2
(cd "$tmp/old" && cargo build --release -p bench --quiet --target-dir "$tmp/target-old")
cargo build --release -p bench --quiet --target-dir "$tmp/target-new"

"$tmp/target-old/release/bench" --fingerprint "${args[@]}" > "$tmp/old.txt"
"$tmp/target-new/release/bench" --fingerprint "${args[@]}" > "$tmp/new.txt"

old_total=$(wc -l < "$tmp/old.txt")
total=$(wc -l < "$tmp/new.txt")
if [ "$old_total" != "$total" ]; then
  echo "diff-fingerprint: WARNING: ROM count differs: $old_total at $rev, $total in the working tree" >&2
fi
if errs=$(grep -c '^LOAD-ERROR' "$tmp/new.txt"); then
  echo "diff-fingerprint: WARNING: $errs ROMs failed to load in the working tree" >&2
fi
# One-sided ROMs (corpus changed between <rev> and now) are never "identical".
# The path is everything after the first space (some paths contain spaces).
strip() { sed 's/^[^ ]* //' "$1" | sort; }
strip "$tmp/old.txt" > "$tmp/old.paths"
strip "$tmp/new.txt" > "$tmp/new.paths"
comm -23 "$tmp/old.paths" "$tmp/new.paths" | sed 's/^/CORPUS only at rev: /' > "$tmp/corpus.txt"
comm -13 "$tmp/old.paths" "$tmp/new.paths" | sed 's/^/CORPUS only in working tree: /' >> "$tmp/corpus.txt"
ncorpus=$(wc -l < "$tmp/corpus.txt")
if [ "$ncorpus" -gt 0 ]; then
  head -n 5 "$tmp/corpus.txt"
  [ "$ncorpus" -le 5 ] || echo "... ($((ncorpus - 5)) more)"
  if [ "$allow_corpus" = 1 ]; then
    echo "diff-fingerprint: WARNING: $ncorpus runs exist on only one side (--allow-corpus-change): comparing the common ROMs only" >&2
  else
    echo "diff-fingerprint: FAIL: $ncorpus runs exist on only one side; $rev has a different corpus, so \"identical\" would mean less. Pass --allow-corpus-change to compare the common ROMs only." >&2
    exit 1
  fi
fi

# Compare by path, over the ROMs both sides ran.
cmp="$tmp/cmp.txt"
awk '
  { h = $1; p = $0; sub(/^[^ ]* /, "", p) }
  NR == FNR { old[p] = h; next }
  { if ((p in old) && old[p] != h) print "DIFF " p "  " old[p] " -> " h }
' "$tmp/old.txt" "$tmp/new.txt" | sort > "$cmp"
ncmp=$(wc -l < "$cmp")
if [ "$ncmp" -eq 0 ]; then
  echo "diff-fingerprint: identical ($((total - ncorpus)) common ROMs compared, $ncorpus one-sided) vs $rev"
  exit 0
fi
cat "$cmp"
echo "diff-fingerprint: $ncmp of $total ROMs differ vs $rev" >&2
exit 1
