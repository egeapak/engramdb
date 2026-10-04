#!/bin/bash
# Stage 1 + 2 of the burn-onnx import test for every ONNX model EngramDB uses.
#
#   stage 1  onnx2burn: ONNX graph -> Rust source + .bpk weights
#   stage 2  cargo check of the generated code on Burn's Flex backend
#
# Results go to results/import.tsv (model, stage reached, seconds, first error
# line). Stage 3 (run and compare against ONNX Runtime) is `verify/`.
set -uo pipefail
cd "$(dirname "$0")"
M=../inference-engines/models/onnx-import
ONNX2BURN="${ONNX2BURN:-$HOME/.cargo/bin/onnx2burn}"
mkdir -p gen results logs
: > results/import.tsv

MODELS=(
  "minilm-fp32:$M/minilm-model.onnx"
  "minilm-uint8:$M/minilm-model_uint8.onnx"
  "reranker-fp32:$M/reranker/model.onnx"
  "reranker-uint8:$M/reranker/model_uint8.onnx"
  "nli-fp32:$M/nli/model.onnx"
  "nli-int8:$M/nli/model_quantized.onnx"
  "t5-encoder-fp32:$M/t5/encoder_model.onnx"
  "t5-encoder-int8:$M/t5/encoder_model_quantized.onnx"
  "t5-decoder-fp32:$M/t5/decoder_model.onnx"
  "t5-decoder-int8:$M/t5/decoder_model_quantized.onnx"
  "t5-decoder-past-fp32:$M/t5/decoder_with_past_model.onnx"
)

first_error() { grep -m1 -E "error(\[E[0-9]+\])?:|panicked|Error|Unsupported|unsupported" "$1" | cut -c1-240; }

for entry in "${MODELS[@]}"; do
  name=${entry%%:*}; file=${entry#*:}
  echo "== $name" >&2
  rm -rf "gen/$name"
  start=$(date +%s.%N)
  if ! "$ONNX2BURN" "$file" "gen/$name" > "logs/$name.codegen.log" 2>&1; then
    secs=$(echo "$(date +%s.%N) - $start" | bc)
    printf '%s\tcodegen-failed\t%.1f\t%s\n' "$name" "$secs" "$(first_error "logs/$name.codegen.log")" >> results/import.tsv
    continue
  fi
  rs=$(find "gen/$name" -maxdepth 1 -name '*.rs' | head -1)
  # One crate per model, sharing a target dir so Burn compiles once.
  mkdir -p "gen/$name/crate/src"
  cat > "gen/$name/crate/Cargo.toml" <<TOML
[package]
name = "gen-${name}"
version = "0.0.0"
edition = "2021"
publish = false
[workspace]
[dependencies]
burn = { version = "=0.21.0", default-features = false, features = ["std", "flex"] }
burn-flex = "=0.21.0"
burn-store = { version = "=0.21.0", features = ["std", "burnpack"] }
TOML
  # Generated code is no_std-style and names `alloc::` directly.
  printf 'extern crate alloc;\n\npub mod model {\n    include!("%s");\n}\n' "$(realpath "$rs")" > "gen/$name/crate/src/lib.rs"
  if (cd "gen/$name/crate" && cargo check -q --release --target-dir ../../../target) > "logs/$name.check.log" 2>&1; then
    stage=compiles; err=""
  else
    stage=compile-failed; err=$(first_error "logs/$name.check.log")
  fi
  secs=$(echo "$(date +%s.%N) - $start" | bc)
  printf '%s\t%s\t%.1f\t%s\n' "$name" "$stage" "$secs" "$err" >> results/import.tsv
done
column -t -s $'\t' results/import.tsv
