#!/usr/bin/env bash
# Tests scripts/lib/board-lock.sh with a temp lock file and dummy commands.
# No board, no serial port. Exit 0 = all pass.
set -u
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export GBC_BOARD_LOCK=$tmp/lock
unset GBC_BOARD_LOCK_HELD
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }
lib=$here/lib/board-lock.sh

# (a) two concurrent holders serialize: the second starts only after the first ends.
cat > "$tmp/hold.sh" <<EOS
. "$lib"
with_board_lock bash -c 'echo "start \$1" >> "$tmp/log"; sleep 1; echo "end \$1" >> "$tmp/log"' _ "\$1"
EOS
bash "$tmp/hold.sh" A & pa=$!
for _ in $(seq 100); do [ -s "$tmp/log" ] && break; sleep 0.05; done
bash "$tmp/hold.sh" B & pb=$!
wait $pa $pb
if [ "$(tr '\n' ' ' < "$tmp/log")" = "start A end A start B end B " ]; then ok "concurrent holders serialize"
else bad "holders interleaved: $(tr '\n' ' ' < "$tmp/log")"; fi

# (b) re-entrancy: nested calls do not deadlock (short wait so a deadlock fails fast).
out=$(GBC_BOARD_LOCK_WAIT=3 bash -c ". '$lib'; with_board_lock bash -c \". '$lib'; with_board_lock echo nested-ran\"" 2>&1); rc=$?
if [ $rc -eq 0 ] && [ "$out" = nested-ran ]; then ok "nested with_board_lock re-enters"
else bad "nested call rc=$rc out=$out"; fi

# (b2) CMD's own exit status is passed through.
GBC_BOARD_LOCK_WAIT=3 bash -c ". '$lib'; with_board_lock bash -c 'exit 7'"; rc=$?
[ $rc -eq 7 ] && ok "exit status passed through" || bad "expected rc 7, got $rc"

# (c) timeout: exit 2, message names the lock file.
bash -c ". '$lib'; with_board_lock sleep 3" & ph=$!
sleep 0.5
out=$(GBC_BOARD_LOCK_WAIT=1 bash -c ". '$lib'; with_board_lock echo should-not-run" 2>&1); rc=$?
wait $ph
if [ $rc -eq 2 ] && [[ $out == *"$GBC_BOARD_LOCK"* ]] && [[ $out != *should-not-run* ]]; then ok "timeout exits 2 naming the lock file"
else bad "timeout rc=$rc out=$out"; fi

[ $fails -eq 0 ] && echo "all board-lock tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
