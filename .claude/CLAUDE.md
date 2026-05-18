This project implements EngramDB — a project-scoped persistent memory store for coding agents.
Tech stack: Rust, LanceDB, ONNX Runtime (all-MiniLM-L6-v2), MCP protocol.

## Code Quality (mandatory)

Before marking ANY task as complete, you MUST run and pass both:

1. **`cargo fmt --all`** — format all code. Run this first.
2. **`cargo clippy --all-targets --all-features -- -D warnings`** — all clippy warnings are treated as errors. Fix every warning before proceeding.

No task is done until both commands succeed with zero warnings and zero errors. This applies to all agents and subagents.

## Building & testing in Claude Code on the web (restricted egress)

The web sandbox's egress gateway uses a custom CA that rustls/webpki-based
downloaders (the `ort` build script, `hf-hub`) reject, even though `curl`
works (it trusts `/etc/ssl/certs/ca-certificates.crt`). Cold builds/tests
fail without these one-time workarounds:

1. **protoc** (LanceDB build dep): `apt-get install -y protobuf-compiler`.
2. **ONNX Runtime binary** (`ort-sys` download fails with `UnknownIssuer`):
   fetch + decode the prebuilt static lib via curl, then build with
   `ORT_STRATEGY=system ORT_LIB_LOCATION=/tmp/ort-lib`:
   ```
   curl -sS -o /tmp/ort.tar.lzma2 "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.23.2/x86_64-unknown-linux-gnu.tar.lzma2"
   python3 -c "import lzma; open('/tmp/ort.tar','wb').write(lzma.decompress(open('/tmp/ort.tar.lzma2','rb').read(), format=lzma.FORMAT_RAW, filters=[{'id':lzma.FILTER_LZMA2,'dict_size':1<<26}]))"
   mkdir -p /tmp/ort-lib && tar -xf /tmp/ort.tar -C /tmp/ort-lib
   ```
   Export `ORT_STRATEGY=system` and `ORT_LIB_LOCATION=/tmp/ort-lib` for all
   `cargo build/clippy/test` commands.
3. **Embedding model** (fastembed download fails the same way): pre-stage
   `Qdrant/all-MiniLM-L6-v2-onnx` into the hf-hub cache layout under
   `~/.cache/engramdb/models/models--Qdrant--all-MiniLM-L6-v2-onnx/` with
   `refs/main` containing `main` and `snapshots/main/<file>` for
   `model.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`,
   `tokenizer_config.json` (curl from `https://huggingface.co/<repo>/resolve/main/<file>`).
   `hf-hub` serves cached files without any network call, so embedding
   tests then pass offline.

Note: `cargo test --lib` has two pre-existing flaky failures under full
parallelism (`ops::doctor::tests::test_doctor_many_memories_healthy`,
`ops::projects::tests::test_get_project_info_with_memories`) — they pass in
isolation and fail identically on a clean base, so they are not a
regression signal.

## Model Downloads

All ML model downloads (embeddings, reranker, NLI) MUST cache to the same directory:
`dirs::cache_dir() / "engramdb" / "models"`.

- Fastembed models: use `InitOptions::new(model).with_cache_dir(cache_dir)`
- HuggingFace Hub models: use `ApiBuilder::new().with_cache_dir(cache_dir).build()`

Never use default cache locations (e.g., `~/.cache/huggingface/hub/`).

## Memory (EngramDB)

This project uses EngramDB as a persistent memory store via MCP.

- **Before answering any project question** (conventions, workflows, architecture, tooling, "how do we..."), call `query` with `mode: "filter"` and a `query` of relevant keywords.
- **Before modifying files**, call `query` with `mode: "rank"` and the file `path` to surface known decisions, hazards, or conventions.
- **After discovering** important patterns, decisions, hazards, or conventions, store them with `create`.
- **If you find contradictory information**, use `challenge` to flag the memory for review.
