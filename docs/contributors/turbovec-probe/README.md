# turbovec probe

Measurement harness behind [../turbovec-evaluation.md](../turbovec-evaluation.md).

Standalone by design: the evaluation concluded **don't adopt**, so `turbovec`
must not appear in the workspace lockfile. This crate carries its own
`[workspace]` table (like `fuzz/`), so root-level `cargo build` / `clippy` /
`nextest` never reach it.

```bash
cd docs/contributors/turbovec-probe

cargo run --release --bin probe      # recall@10 + top-1 vs exact cosine, d=384
cargo run --release --bin overhead   # fixed per-index bytes; vs a plain f32 scan
cargo run --release --bin allowlist  # allowlist selectivity; UnknownId semantics
cargo run --release --bin vs_lance   # per-project search cost vs LanceDB
```

`vs_lance` needs `protoc` (`apt-get install -y protobuf-compiler`) for LanceDB's
build script. The other three have no system dependencies — no ONNX runtime and
no staged model, since every corpus is synthetic.

Results, caveats, and why synthetic corpora are the weak point of all four are in
the evaluation document.
