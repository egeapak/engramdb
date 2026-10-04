#!/bin/sh
# Downloads every ONNX file EngramDB uses (fp32 and shipped quantized), plus
# tokenizers, into ../inference-engines/models/onnx-import/. curl, not hf-hub:
# rustls rejects the web sandbox's proxy CA (see CLAUDE.md).
set -eu
cd "$(dirname "$0")"
../inference-engines/stage_models.sh >/dev/null
D=../inference-engines/models/onnx-import
mkdir -p "$D/reranker" "$D/nli" "$D/t5"
get() { [ -s "$D/$2" ] || curl -sSfL -o "$D/$2" "https://huggingface.co/$1/resolve/main/$3"; }
cp -n ../inference-engines/models/xenova/onnx/model.onnx "$D/minilm-model.onnx"
cp -n ../inference-engines/models/xenova/onnx/model_uint8.onnx "$D/minilm-model_uint8.onnx"
for f in model.onnx model_uint8.onnx; do get jinaai/jina-reranker-v1-turbo-en "reranker/$f" "onnx/$f"; done
get jinaai/jina-reranker-v1-turbo-en reranker/tokenizer.json tokenizer.json
for f in model.onnx model_quantized.onnx; do get Xenova/nli-deberta-v3-xsmall "nli/$f" "onnx/$f"; done
get Xenova/nli-deberta-v3-xsmall nli/tokenizer.json tokenizer.json
for f in encoder_model.onnx encoder_model_quantized.onnx decoder_model.onnx \
         decoder_model_quantized.onnx decoder_with_past_model.onnx; do
  get Xenova/t5-small "t5/$f" "onnx/$f"
done
get Xenova/t5-small t5/tokenizer.json tokenizer.json
find "$D" -name '*.onnx' | sort
