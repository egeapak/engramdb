#!/usr/bin/env bash
# What each dot-product approach costs in bytes, per opt-level.
#
# Measured as a delta between three minimal binaries that differ only in the
# kernel (`src/bin/size_{scalar,intrinsics,fearless}.rs`) — subtracting the
# scalar one cancels std, argv parsing and formatting, leaving the SIMD.
#
# A delta rather than a symbol sum because fearless_simd's cost is not one
# function: `dispatch!` monomorphises the kernel once per SIMD level and pulls
# in that level's `vectorize` wrapper and `splat` kernels, so the bytes live in
# symbols the source never names. Each size_* binary has exactly one dispatch
# site, matching what an adoption inside `dot_unit` would really cost.
#
# `.text` rather than file size: the syms/o2/o3 profiles keep symbols so their
# file sizes carry a mangled-name tax that has nothing to do with code, and
# fearless_simd's generated names are very long. `.text` is what executes.
set -euo pipefail
cd "$(dirname "$0")"

# GNU binutils `size -A`. macOS ships a `size` with different output, and the
# awk below would silently yield an empty string that `$((...))` then chokes on
# *after* the header has printed. Fail up front and say why.
if ! size --version 2>/dev/null | grep -qi "gnu"; then
  echo "error: needs GNU binutils \`size\` (this looks like a different one)." >&2
  echo "       on macOS: brew install binutils, then use gsize / put it on PATH." >&2
  exit 1
fi

# The intrinsics case reproduces the deleted x86-64 backends only; elsewhere it
# is a stub that would make the comparison meaningless.
case "$(uname -m)" in
  x86_64 | amd64) ;;
  *)
    echo "note: the intrinsics column is x86-64 only; skipping it on $(uname -m)." >&2
    INTRINSICS_NA=1
    ;;
esac

text() { size -A "$1" | awk '$1==".text" {print $2}'; }

printf '%-14s %10s %10s %10s   %12s %12s\n' \
  profile scalar intrinsics fearless 'intrinsics' 'fearless'
for prof in syms oz o2; do
  case $prof in
    syms) label='3 (ships)' ;;
    oz)   label='"z" (old)' ;;
    o2)   label='opt-level 2' ;;
  esac
  # Errors are shown, not swallowed: a build failure here used to abort inside
  # the command substitution below with no output at all.
  if ! err=$(cargo build --profile "$prof" --bins 2>&1 >/dev/null); then
    echo "error: build failed for profile $prof" >&2
    echo "$err" >&2
    exit 1
  fi
  s=$(text "target/$prof/size_scalar")
  f=$(text "target/$prof/size_fearless")
  if [ -n "${INTRINSICS_NA:-}" ]; then
    printf '%-14s %10s %10s %10s   %12s %+12s\n' \
      "$label" "$s" "n/a" "$f" "n/a" "$((f - s))"
  else
    i=$(text "target/$prof/size_intrinsics")
    printf '%-14s %10s %10s %10s   %+12s %+12s\n' \
      "$label" "$s" "$i" "$f" "$((i - s))" "$((f - s))"
  fi
done
echo
echo '.text bytes; last two columns are what the SIMD adds over a scalar loop.'
