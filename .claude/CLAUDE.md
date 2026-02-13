This project implements EngramDB — see docs/engramdb-spec.md for the full design specification.
Tech stack: Rust, LanceDB, ONNX Runtime (all-MiniLM-L6-v2), MCP protocol.

## Code Quality (mandatory)

Before marking ANY task as complete, you MUST run and pass both:

1. **`cargo fmt --all`** — format all code. Run this first.
2. **`cargo clippy --all-targets --all-features -- -D warnings`** — all clippy warnings are treated as errors. Fix every warning before proceeding.

No task is done until both commands succeed with zero warnings and zero errors. This applies to all agents and subagents.

## Model Downloads

All ML model downloads (embeddings, reranker, NLI) MUST cache to the same directory:
`dirs::cache_dir() / "engramdb" / "models"`.

- Fastembed models: use `InitOptions::new(model).with_cache_dir(cache_dir)`
- HuggingFace Hub models: use `ApiBuilder::new().with_cache_dir(cache_dir).build()`

Never use default cache locations (e.g., `~/.cache/huggingface/hub/`).
