#!/bin/bash
# Phase 1 driver: clean release build per engine family (timed, sized), then
# one bench process per engine, then the comparison tables.
#
#   ./run.sh                 # everything
#   SKIP_BUILD=1 ./run.sh    # reuse binaries from a previous run
#   BUILD_ONLY=1 ./run.sh    # timed builds only
#   ENGINES="ort-u8 candle" ./run.sh
#
# Env passed through to the bench: ITERS (default 3), PROBE_TRIALS (30),
# PROBE_LOAD_THREADS (4), ENGINE_THREADS (ORT intra-op threads; unset = ORT
# default). Burn and Candle size their pools with rayon (RAYON_NUM_THREADS).
set -euo pipefail
cd "$(dirname "$0")"

FAMILIES="baseline ort burn-flex burn-ndarray candle"
ENGINES="${ENGINES:-ort-u8 ort-f32 burn-flex burn-ndarray candle}"
mkdir -p results

family_of() { case "$1" in ort-*) echo ort ;; *) echo "$1" ;; esac; }
bin_of() { echo "target-$1/release/inference-engines"; }

if [ -z "${SKIP_BUILD:-}" ]; then
  : > results/build.tsv
  for fam in $FAMILIES; do
    feat=$([ "$fam" = baseline ] && echo "" || echo "--features $fam")
    rm -rf "target-$fam"
    echo "== building $fam (clean, release)" >&2
    start=$(date +%s.%N)
    # shellcheck disable=SC2086
    cargo build --release --locked --target-dir "target-$fam" $feat
    end=$(date +%s.%N)
    printf '%s\t%.1f\t%s\n' "$fam" "$(echo "$end - $start" | bc)" \
      "$(stat -c %s "$(bin_of "$fam")")" >> results/build.tsv
  done
fi
[ -n "${BUILD_ONLY:-}" ] && exit 0

ORT_SO="models/onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so"
export ORT_DYLIB_PATH="${ORT_DYLIB_PATH:-$PWD/$ORT_SO}"

for e in $ENGINES; do
  echo "== bench $e" >&2
  "$(bin_of "$(family_of "$e")")" bench "$e"
done

"$(bin_of baseline)" compare
