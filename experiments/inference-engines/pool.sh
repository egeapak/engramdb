#!/bin/bash
# Daemon-pool throughput sweep (see src/pool.rs). Two runs:
#
#   results-pool/          each engine's default threads per session — what
#                          the EngramDB daemon does today (fastembed never sets
#                          intra-op threads; pool = cores/2).
#   results-pool-1thread/  one thread per session, so a pool of N uses N cores
#                          without sessions competing for them.
#
# Env: POOL_SIZES (1,2,4), POOL_CALLERS (1,2,4,8), POOL_WINDOW_SECS (4),
# ENGINES. Binaries come from run.sh's per-engine target dirs (built here
# incrementally if missing).
set -euo pipefail
cd "$(dirname "$0")"

ENGINES="${ENGINES:-ort-u8 ort-f32 burn-flex burn-ndarray candle}"
family_of() { case "$1" in ort-*) echo ort ;; *) echo "$1" ;; esac; }
bin_of() { echo "target-$1/release/inference-engines"; }

for fam in baseline ort burn-flex burn-ndarray candle; do
  feat=$([ "$fam" = baseline ] && echo "" || echo "--features $fam")
  # shellcheck disable=SC2086
  cargo build -q --release --locked --target-dir "target-$fam" $feat
done

export ORT_DYLIB_PATH="${ORT_DYLIB_PATH:-$PWD/models/onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so}"

sweep() {
  local out=$1
  mkdir -p "$out"
  for e in $ENGINES; do
    echo "== pool $e -> $out" >&2
    "$(bin_of "$(family_of "$e")")" pool "$e" --out "$out"
  done
  "$(bin_of baseline)" compare --out "$out"
}

sweep results-pool
ENGINE_THREADS=1 RAYON_NUM_THREADS=1 sweep results-pool-1thread
