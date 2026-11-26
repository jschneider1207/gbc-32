# shellcheck shell=bash
# Sourced by every script that flashes or talks to the board (ADR 0036,
# "Board lock"). There is one board per machine, so one exclusive flock.
#
#   with_board_lock CMD...   runs CMD (a command or a shell function) while
#                            holding the lock; returns CMD's exit status.
#
#   GBC_BOARD_LOCK       lock file (default /tmp/gbc-32-board.lock; not under
#                        target/, which cargo clean or a scratch worktree
#                        would remove or split)
#   GBC_BOARD_LOCK_WAIT  seconds to wait for the lock (default 1800)
#   GBC_BOARD_LOCK_HELD  set to 1 (exported) while the lock is held; nested
#                        calls then just run CMD, so a flash-then-read
#                        sequence can hold the lock across both scripts.
#
# On timeout: message naming the lock file, exit 2 (tool error). The build
# lock (H7_BENCH_LOCK) is separate.
#
# A script that must hold the lock for its whole body starts with:
#   . "$(dirname "$0")/<path>/scripts/lib/board-lock.sh"
#   board_lock_reexec "$@"    # re-runs the script under the lock, if not held

with_board_lock() {
  if [ "${GBC_BOARD_LOCK_HELD:-}" = 1 ]; then
    "$@"
    return
  fi
  local lock=${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock}
  local wait=${GBC_BOARD_LOCK_WAIT:-1800}
  (
    # The fd is held by this subshell (and inherited by CMD); the lock is
    # released when they exit.
    exec 9>>"$lock" || { echo "board-lock: cannot open $lock" >&2; exit 2; }
    if ! flock -x -w "$wait" 9; then
      echo "board-lock: timed out after ${wait}s waiting for the board lock $lock (another agent is using the board)" >&2
      exit 2
    fi
    export GBC_BOARD_LOCK_HELD=1
    "$@"
  )
}

# board_lock_reexec ARGS...: call as the first thing in a script, with the
# script's own arguments. If the lock is not held, re-runs the script under it.
board_lock_reexec() {
  [ "${GBC_BOARD_LOCK_HELD:-}" = 1 ] && return 0
  with_board_lock "${BASH_SOURCE[1]}" "$@"
  exit $?
}

# board_lock_hold: takes the lock in the calling shell itself (on fd 9) and
# keeps it until that shell exits; exports GBC_BOARD_LOCK_HELD=1 so every
# board script it then runs skips taking it again. For a long sequence of
# board steps in one script (scripts/gate-stage2.sh). Returns 2 on
# timeout or an unopenable lock file (message as with_board_lock's), 0 if
# the lock is already held.
board_lock_hold() {
  [ "${GBC_BOARD_LOCK_HELD:-}" = 1 ] && return 0
  local lock=${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock}
  local wait=${GBC_BOARD_LOCK_WAIT:-1800}
  exec 9>>"$lock" || { echo "board-lock: cannot open $lock" >&2; return 2; }
  if ! flock -x -w "$wait" 9; then
    echo "board-lock: timed out after ${wait}s waiting for the board lock $lock (another agent is using the board)" >&2
    exec 9>&-
    return 2
  fi
  export GBC_BOARD_LOCK_HELD=1
}
