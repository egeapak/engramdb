#!/bin/sh
# Downloads the model files with curl (hf-hub's rustls rejects the web
# sandbox's proxy CA, see CLAUDE.md) and the official ONNX Runtime 1.24.2
# shared library (the pyke prebuilt mis-executes quantized models on
# AVX-512/AMX hosts, so it must not be used for timing or agreement runs).
set -eu
cd "$(dirname "$0")"
HF=https://huggingface.co

mkdir -p models/xenova/onnx models/st
for f in onnx/model_uint8.onnx onnx/model.onnx tokenizer.json config.json; do
  [ -s "models/xenova/$f" ] || curl -sSfL -o "models/xenova/$f" "$HF/Xenova/all-MiniLM-L12-v2/resolve/main/$f"
done
for f in model.safetensors tokenizer.json config.json; do
  [ -s "models/st/$f" ] || curl -sSfL -o "models/st/$f" "$HF/sentence-transformers/all-MiniLM-L12-v2/resolve/main/$f"
done

ORT=onnxruntime-linux-x64-1.24.2
if [ "$(uname -s)" = Linux ] && [ ! -e "models/$ORT/lib/libonnxruntime.so" ]; then
  curl -sSfL "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/$ORT.tgz" | tar -xz -C models
fi
ls -l models/xenova/onnx models/st
