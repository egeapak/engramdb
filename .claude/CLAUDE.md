This project implements EngramDB — see docs/engramdb-spec.md for the full design specification.
Tech stack: Rust, LanceDB, ONNX Runtime (all-MiniLM-L6-v2), MCP protocol.

## Code Quality (mandatory)

Before marking ANY task as complete, you MUST run and pass both:

1. **`cargo fmt --all`** — format all code. Run this first.
2. **`cargo clippy --all-targets --all-features -- -D warnings`** — all clippy warnings are treated as errors. Fix every warning before proceeding.

No task is done until both commands succeed with zero warnings and zero errors. This applies to all agents and subagents.
