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

text() { size -A "$1" | awk '$1==".text" {print $2}'; }

printf '%-14s %10s %10s %10s   %12s %12s\n' \
  profile scalar intrinsics fearless 'intrinsics' 'fearless'
for prof in syms o2 o3; do
  case $prof in
    syms) label='"z" (ships)' ;;
    o2)   label='opt-level 2' ;;
    o3)   label='opt-level 3' ;;
  esac
  cargo build --profile "$prof" --bins >/dev/null 2>&1
  s=$(text "target/$prof/size_scalar")
  i=$(text "target/$prof/size_intrinsics")
  f=$(text "target/$prof/size_fearless")
  printf '%-14s %10s %10s %10s   %+12s %+12s\n' \
    "$label" "$s" "$i" "$f" "$((i - s))" "$((f - s))"
done
echo
echo '.text bytes; last two columns are what the SIMD adds over a scalar loop.'
