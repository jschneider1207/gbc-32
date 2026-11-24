# shellcheck shell=bash
# Sourced (from the repository root) by every script that builds with its
# own RUSTFLAGS. The workspace's rustc flags live in ONE place, the
# `rustflags = [...]` line of .cargo/config.toml (docs/perf.md "Build
# flags"); this reads them from there, because a RUSTFLAGS variable
# replaces the config's flags instead of adding to them.
#
# Defines:
#   GBC_RUSTFLAGS      the config's flags, space-separated
#   GBC_MCU_RUSTFLAGS  those plus `-C target-cpu=cortex-m7` (the MCU build)
#   with_mcu_rustflags CMD...  runs CMD with RUSTFLAGS=$GBC_MCU_RUSTFLAGS
#                      (and CARGO_ENCODED_RUSTFLAGS unset: it would win)
# Exits 1 if the line is missing, not a one-line array, or lacks the
# LLVM option of ADR 0008, so a build without it cannot pass a check.

_gbc_cfg=.cargo/config.toml
_gbc_lines=$(grep -E '^[[:space:]]*rustflags[[:space:]]*=' "$_gbc_cfg" || true)
if [ "$(printf '%s\n' "$_gbc_lines" | grep -c .)" -ne 1 ]; then
  echo "FAIL: expected exactly one 'rustflags = [...]' line in $_gbc_cfg (docs/perf.md, Build flags)" >&2
  exit 1
fi
GBC_RUSTFLAGS=$(printf '%s\n' "$_gbc_lines" \
  | sed -n 's/^[[:space:]]*rustflags[[:space:]]*=[[:space:]]*\[\(.*\)\][[:space:]]*$/\1/p' \
  | tr -d '"' | tr ',' ' ' | tr -s ' ' | sed 's/^ //; s/ $//')
case " $GBC_RUSTFLAGS " in
  *" llvm-args=-cgp-max-address-users-to-scan="*) ;;
  *)
    echo "FAIL: the rustflags of $_gbc_cfg lack llvm-args=-cgp-max-address-users-to-scan=... (ADR 0008; docs/perf.md, Build flags)" >&2
    exit 1
    ;;
esac
GBC_MCU_RUSTFLAGS="$GBC_RUSTFLAGS -C target-cpu=cortex-m7"
unset _gbc_cfg _gbc_lines

with_mcu_rustflags() {
  env -u CARGO_ENCODED_RUSTFLAGS RUSTFLAGS="$GBC_MCU_RUSTFLAGS" "$@"
}
