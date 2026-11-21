#!/usr/bin/env bash
# Prove a change is behavior-identical to an earlier revision (docs/perf.md,
# ADR 0011 "Verification").
#
#   scripts/diff-fingerprint.sh <rev> [bench --fingerprint args, e.g. --seconds 5]
#
# Builds tools/bench at <rev> in a temporary git worktree and the current
# working tree, runs `bench --fingerprint` on both with the same arguments,
# and diffs the per-ROM hashes. Exit 0 iff identical.
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

[ $# -ge 1 ] || { echo "usage: $0 <rev> [--seconds N] [--root DIR]" >&2; exit 2; }
rev=$1
shift
git rev-parse --verify --quiet "$rev^{commit}" >/dev/null || { echo "diff-fingerprint: unknown rev '$rev'" >&2; exit 2; }

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

total=$(wc -l < "$tmp/new.txt")
if diff -q "$tmp/old.txt" "$tmp/new.txt" >/dev/null; then
  echo "diff-fingerprint: identical ($total ROMs) vs $rev"
  exit 0
fi

# Paths are the second column; compare by path so added/removed ROMs show too.
cmp="$tmp/cmp.txt"
join -1 2 -2 2 -a1 -a2 -e MISSING -o 0,1.1,2.1 \
  <(sort -k2 "$tmp/old.txt") <(sort -k2 "$tmp/new.txt") | awk '$2 != $3' > "$cmp"
awk '{ printf "DIFF %s  %s -> %s\n", $1, $2, $3 }' "$cmp"
echo "diff-fingerprint: $(wc -l < "$cmp") of $total ROMs differ vs $rev" >&2
exit 1
