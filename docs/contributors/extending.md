# Extending EngramDB

Each recipe lists which files to touch. Use an existing implementation as the template.

> When you change anything user-facing, also update the relevant pages in `docs/users/`, `docs/agents/`, or `docs/contributors/`.

## Add a new embedding provider

`OnnxProvider` (`src/embeddings/onnx.rs`) is the template.

1. Implement `EmbeddingProvider` in `src/embeddings/<name>.rs`. **`model_id()` is persisted to the manifest** — distinct quantization (fp32 vs int8) MUST produce distinct IDs.
2. Add a `ModelSpec` constant and export it from `src/embeddings/mod.rs`.
3. Wire the provider string into `provider_specs` in `src/ops/mod.rs`. This is the **single source of truth** the fingerprint check and the resolver both use — don't shortcut it.
4. Add a test in `src/embeddings/<name>.rs::tests` and add it to the `ml-models` nextest group.

## Add a new MCP tool (or CLI subcommand)

Existing tools (`create_memory`, `query`) are the template — implement once in `src/ops/`, expose twice.

1. Implement `MyOpParams` / `MyOpResult` and the async function in `src/ops/<name>.rs`. Re-export from `src/ops/mod.rs`.
2. CLI surface: add a `Command::MyOp` variant in `src/cli/app.rs`, a `run_my_op` handler in `src/cli/commands/<name>.rs`, re-export from `commands/mod.rs`, dispatch in `cli/mod.rs`.
3. MCP surface: add a `#[tool(...)]` method on `EngramDbServer` in `src/mcp/server.rs` with a `MyOpRequest` input struct. Resolve the target store (handle the `project` parameter), call `ops::my_op`, serialize the result.
4. Test the op in `src/ops/<name>.rs::tests` and the CLI in `tests/cli/<name>.rs`.

## Add a new memory type variant

1. **`src/types/memory.rs`**:
   - Add the variant to `MemoryType` enum with the `#[serde(rename = "...")]` lowercase form.
   - Update `default_decay` to return the right `Decay` for the new variant.
   - Update `Display`, `FromStr` impls.

2. **`src/ops/parsing.rs`**: extend `parse_memory_type` to accept the new variant string.

3. **`src/cli/output.rs`**: if the variant needs a unique color/glyph for pretty output, wire it in.

4. **Documentation.**
   - `docs/agents/memory-model.md` — add to the types table.
   - `docs/users/cli-reference.md` — update the `add --type` list.

5. **Test** that the round-trip (create → list → get) preserves the new type.

## Add a new config field

1. **`src/types/config.rs`**: add the field with `#[serde(default = "default_my_field")]` and a `fn default_my_field() -> T` returning the default. **Don't use `#[serde(default)]`** with the type's `Default` impl unless the type has the right default — explicit `default_*` functions document the value next to the field.

2. **Validate at deserialization.** If the field has a valid range (e.g. `0..=1.0`), add a custom deserializer or a `validate_*` call in a post-deser hook. Bad configs should fail loading, not at first use.

3. **If the field affects model loading**, add it to `provider_cache_key` in `src/ops/mod.rs`. There's a test (`cache_key_is_deterministic_and_signature_sensitive`) that fails if you forget.

4. **Document in `docs/users/configuration.md`** — every section there should match `EngramConfig`.

5. **Default behavior must be backwards-compatible.** Existing `config.toml` files in users' projects don't have your new field. They must continue to work with the default.

## Add a new scoring weight or signal

1. **`src/scoring/composite.rs`**: extend `ScoringContext` with the new input. Extend the formula in `composite_score`.

2. **Decide which scoring mode it affects** — `with_query` / `with_keyword` / `scope_only` / `degraded`, or all of them.

3. **If it's a multiplier**, follow the existing pattern: `multiplier = floor + (1 - floor) * signal`, configurable floor in `[retrieval.scoring]`.

4. **`src/types/config.rs`**: add the weight to `ScoringWeights` or as a top-level field in `ScoringConfig`.

5. **Test the formula in `composite.rs::tests`** with at least: signal=0 (baseline), signal=1 (max), and a midpoint.

## Add a new daemon RPC

1. **`src/daemon/protocol.rs`**:
   - Add the request/response enum variants. Bump `PROTOCOL_VERSION` if the protocol is changing in a backwards-incompatible way.
   - The daemon-mod uses `bincode`-or-similar; check the existing serializer.

2. **`src/daemon/server.rs`**: add the handler. It should call into `src/ops/` or the in-process model providers — don't reinvent.

3. **`src/daemon/remote.rs`**: add the client-side wiring. The remote impl must produce the same trait surface as the local impl (`EmbeddingProvider`, `NliProvider`, `Reranker`).

4. **Test in `src/daemon/tests.rs`.** Pattern: spawn an in-process daemon, connect a client, call the new RPC, assert.

5. **Compatibility.** If you bump `PROTOCOL_VERSION`, the daemon and client must check it on handshake and fall back / error helpfully. **Daemon failures must never break operations** — if the new RPC isn't available on an older daemon, the client should fall back to local-provider behavior.

## Add a new CLI output format

1. **`src/cli/output.rs`**: extend `OutputFormat` enum with the new variant.

2. **Implement the formatting paths** — search the file for places that match on `OutputFormat` and add the new arm.

3. **`src/cli/app.rs`**: extend `--format` to accept the new value (Clap's `value_parser` will pick it up automatically).

4. **Document in `docs/users/cli-reference.md`** under the global flags table.

## Add support for a new hook event

1. **`src/cli/app.rs`**: extend `HookCommand` with the new variant.

2. **`src/cli/commands/hook.rs`**: add the handler. Read event JSON from stdin (use `serde_json::Value` and `.get("...")` for forward-compat), emit `additionalContext` JSON.

3. **`src/cli/commands/setup.rs`**: update the writer that adds hooks to `settings.json` so the new event is registered when users run `engramdb setup`.

4. **`.claude-plugin/plugin.json`**: add the new hook to the plugin manifest.

5. **Test.** Pipe a fixture event JSON into the new subcommand and assert the output.

