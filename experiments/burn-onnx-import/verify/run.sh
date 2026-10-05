#!/bin/bash
# Stage 3: one process per model, so a panic in one does not lose the others.
set -uo pipefail
cd "$(dirname "$0")"
export ORT_DYLIB_PATH="${ORT_DYLIB_PATH:-$PWD/../../inference-engines/models/onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so}"
cargo build -q --release || exit 1
out=../results/verify.md
: > "$out"
for m in minilm reranker nli t5; do
  echo "== $m" >&2
  if ! ./target/release/burn-onnx-verify "$m" > "../results/verify-$m.md" 2> "../results/verify-$m.log"; then
    echo "| $m | – | FAILED: $(grep -m1 -E 'panicked|Error' -A1 "../results/verify-$m.log" | tail -1 | cut -c1-200) |" >> "$out"
  fi
done
