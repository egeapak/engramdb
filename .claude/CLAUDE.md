This project implements EngramDB — a project-scoped persistent memory store for coding agents.
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

## Memory (EngramDB)

This project uses EngramDB as a persistent memory store via MCP.

- **Before answering any project question** (conventions, workflows, architecture, tooling, "how do we..."), call `search` with relevant keywords.
- **Before modifying files**, call `retrieve` with the file path to check for known decisions, hazards, or conventions.
- **After discovering** important patterns, decisions, hazards, or conventions, store them with `create`.
- **If you find contradictory information**, use `challenge` to flag the memory for review.
