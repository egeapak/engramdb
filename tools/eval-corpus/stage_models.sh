#!/bin/bash
# Pre-stage HF models into the engramdb model cache.
# hf-hub uses rustls and rejects the web sandbox's proxy CA, so downloads must
# come through curl. Idempotent: skips anything already present.
set -u
CACHE="${ENGRAMDB_MODEL_CACHE_DIR:-$HOME/.cache/engramdb/models}"
stage() {
  local repo="$1" file="$2"
  local dir="$CACHE/models--${repo//\//--}"
  mkdir -p "$dir/refs" "$dir/snapshots/main/$(dirname "$file")"
  echo main > "$dir/refs/main"
  for aux in tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
    [ -s "$dir/snapshots/main/$aux" ] || curl -sSL --retry 3 \
      "https://huggingface.co/$repo/resolve/main/$aux" -o "$dir/snapshots/main/$aux"
  done
  if [ -s "$dir/snapshots/main/$file" ]; then
    echo "  have $repo/$file"
  else
    echo "  fetching $repo/$file"
    curl -sSL --retry 3 "https://huggingface.co/$repo/resolve/main/$file" \
      -o "$dir/snapshots/main/$file"
  fi
  du -h "$dir/snapshots/main/$file" 2>/dev/null | sed 's/^/    /'
}
stage "Xenova/all-MiniLM-L12-v2"              "onnx/model_uint8.onnx"
stage "mixedbread-ai/mxbai-embed-large-v1"    "onnx/model_quantized.onnx"
stage "Alibaba-NLP/gte-large-en-v1.5"         "onnx/model_quantized.onnx"
stage "Qdrant/bge-large-en-v1.5-onnx-Q"       "model_optimized.onnx"
echo STAGING_DONE
