#!/usr/bin/env bash
# Fail when a committed insta snapshot was asserted by no test.
#
#   check_unreferenced_snapshots.sh <references-file>
#
# A snapshot whose test was renamed or deleted stays in the repository, still
# looks like a pinned contract in review, and pins nothing. `cargo insta test
# --unreferenced reject` finds those, but it needs the whole suite to run in
# one place and CI's test jobs are sharded. So this uses the mechanism
# cargo-insta itself is built on: when `INSTA_SNAPSHOT_REFERENCES_FILE` is set,
# insta appends the path of every snapshot file an assertion touches. The
# `coverage` job (the one unsharded full run) sets it; this script compares the
# result with `git ls-files '*.snap'`.
#
# The variable is internal to insta, not a documented interface. If a future
# insta stops writing the file, this script must not pass on an empty list:
# a missing or empty references file is an error.
#
# A snapshot only a non-Linux test asserts belongs in ALLOW below, with the
# `cfg` that explains it. There are none today.
set -euo pipefail

refs=${1:?usage: check_unreferenced_snapshots.sh <references-file>}
root=$(git rev-parse --show-toplevel)
cd "$root"

# One path per line, relative to the repository root. Each entry needs the
# reason (the cfg of the test that owns it).
ALLOW=$(cat <<'LIST'
LIST
)

if [ ! -s "$refs" ]; then
  echo "::error::no snapshot references were recorded in $refs. Either no snapshot test ran, or insta no longer honours INSTA_SNAPSHOT_REFERENCES_FILE; this check cannot tell which, so it fails."
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# insta writes the path as the test saw it: absolute, sometimes with `..`.
while IFS= read -r line; do
  [ -n "$line" ] || continue
  abs=$(realpath -m -- "$line")
  case "$abs" in
    "$root"/*) printf '%s\n' "${abs#"$root"/}" ;;
  esac
done < "$refs" | sort -u > "$work/referenced"

git ls-files '*.snap' | sort -u > "$work/committed"
printf '%s\n' "$ALLOW" | sed '/^[[:space:]]*$/d' | sort -u > "$work/allowed"

committed=$(wc -l < "$work/committed")
referenced=$(comm -12 "$work/committed" "$work/referenced" | wc -l)
echo "committed snapshots: $committed; asserted in this run: $referenced"

# Guard the guard: a references file that matches almost nothing means the
# paths are in a shape this script does not understand, not that every
# snapshot is dead.
if [ "$committed" -gt 0 ] && [ "$referenced" -eq 0 ]; then
  echo "::error::none of the $(wc -l < "$work/referenced") recorded paths is a committed snapshot; the references file has an unexpected format. First lines:"
  head -n 5 "$refs"
  exit 1
fi

comm -23 "$work/committed" "$work/referenced" | comm -23 - "$work/allowed" > "$work/unreferenced"
if [ -s "$work/unreferenced" ]; then
  while IFS= read -r f; do
    echo "::error file=$f::no test asserted this snapshot. Delete it (git rm), or restore the test that owned it."
  done < "$work/unreferenced"
  echo "$(wc -l < "$work/unreferenced") unreferenced snapshot(s)."
  exit 1
fi

# An allow-list entry that is asserted after all, or no longer exists, is stale.
stale=$(comm -12 "$work/allowed" "$work/referenced"; comm -23 "$work/allowed" "$work/committed")
if [ -n "$stale" ]; then
  echo "::error::stale ALLOW entries in check_unreferenced_snapshots.sh (asserted on this platform, or not committed any more):"
  printf '%s\n' "$stale"
  exit 1
fi
echo "OK: every committed snapshot was asserted by a test."
