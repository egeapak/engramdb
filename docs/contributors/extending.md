# Extending EngramDB

Recipes for common extensions: adding embedding providers, MCP tools, CLI commands, memory types, and scoring tweaks. Each recipe lists every file you need to touch.

## Add a new embedding provider

**Critical invariant.** The fingerprint table and the provider resolver derive from **one map**: `provider_specs` in `src/ops/mod.rs`. Adding a provider in one place but not the other is the silent-vector-corruption footgun that the unification fixed. Always update `provider_specs` — don't shortcut.

Steps:

1. **Implement the `EmbeddingProvider` trait.** Add `src/embeddings/<name>.rs`:
   ```rust
   pub struct MyProvider { /* ... */ }

   #[async_trait::async_trait]
   impl EmbeddingProvider for MyProvider {
       async fn embed(&self, text: &str) -> Result<Vec<f32>> { ... }
       async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> { ... }
       fn dimensions(&self) -> usize { ... }
       fn max_tokens(&self) -> usize { ... }
       fn model_id(&self) -> String { /* e.g. "myprovider/some-model" */ ... }
   }
   ```
   Pay attention to `model_id()` — it's what gets persisted to the manifest. Distinct quantization (fp32 vs int8) **must** produce distinct ids so swaps are detected.

2. **Add a `ModelSpec` constant.** In the same file:
   ```rust
   pub const MY_MODEL: MyModelSpec = MyModelSpec {
       name: "my-model",
       dimensions: 768,
       /* ... */
   };
   ```

3. **Export it.** In `src/embeddings/mod.rs`:
   ```rust
   mod my_provider;
   pub use my_provider::{MyProvider, MY_MODEL};
   ```

4. **Wire it into `provider_specs`** (`src/ops/mod.rs`). Add a match arm for the config provider string:
   ```rust
   fn provider_specs(provider: &str) -> Option<ProviderSpecs> {
       Some(match provider {
           ...,
           "my-model" => ProviderSpecs {
               onnx: MY_ONNX_SPEC,        // ALSO add an ONNX variant if you want ONNX backend
               #[cfg(feature = "ollama")]
               ollama: MY_OLLAMA_SPEC,    // if Ollama supports it
           },
           _ => return None,
       })
   }
   ```

5. **Add to `try_onnx_then_ollama`** if backend selection needs anything custom (usually not).

6. **Update config validation** in `src/types/config.rs` if you want the new provider name to validate against an enum (currently the provider string is freeform; unknown strings disable embeddings).

7. **Test.** Add a unit test in `src/embeddings/<name>.rs::tests`. Add it to the `ml-models` nextest group (`.config/nextest.toml`) so it doesn't race with other model tests.

8. **Document.** Mention the new provider in `docs/users/embeddings.md` and the provider table in `docs/users/configuration.md`.

## Add a new MCP tool (or CLI subcommand)

The ops layer is shared — implement once, expose twice.

1. **Implement the operation in `src/ops/<name>.rs`.** Typed `Params` struct in, typed `Result` struct out. No formatting, no MCP serialization.
   ```rust
   pub struct MyOpParams { /* ... */ }
   pub struct MyOpResult { /* ... */ }

   pub async fn my_op(store: &MemoryStore, params: MyOpParams) -> Result<MyOpResult> { ... }
   ```

2. **Re-export from `src/ops/mod.rs`.**

3. **Add the CLI subcommand:**
   - `src/cli/app.rs` — add a `Command::MyOp { ... }` variant with Clap derive.
   - `src/cli/commands/<name>.rs` — `pub async fn run_my_op(dir, params, formatter) -> Result<()>` that parses CLI args into `MyOpParams`, calls `ops::my_op`, and formats via `OutputFormatter`.
   - `src/cli/commands/mod.rs` — re-export `run_my_op`.
   - `src/cli/mod.rs` — add a match arm in the dispatch.

4. **Add the MCP tool:**
   - In `src/mcp/server.rs`, add a `#[tool(name = "...", description = "...")]` method on `EngramDbServer`.
   - Define a `MyOpRequest` struct with `#[derive(Deserialize, JsonSchema)]` for the MCP input parameters.
   - Inside the tool method: resolve the target store (handle the `project` parameter), build `MyOpParams`, call `ops::my_op`, serialize the result to `CallToolResult`.
   - The existing tools (`create_memory`, `query`, etc.) are the template — copy structure from one of them.

5. **If the tool mutates state**, document that in its description so agents know.

6. **Test it.**
   - Unit test the op in `src/ops/<name>.rs::tests`.
   - Integration test the CLI in `tests/cli/<name>.rs`.
   - Document in `docs/agents/mcp-tools.md` (parameter table) and `docs/users/cli-reference.md`.

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

## When the answer doesn't fit a recipe

If your change spans many files (e.g. "rework the scoring pipeline", "switch storage backend"), open a design doc on the PR before writing code. The architecture invariants (one-binary, ops-layer, provider_specs, model fingerprint, daemon-as-optimization-not-dependency) are the constraints — work with them, not against them.

When in doubt, search `CLAUDE.md` and `docs/contributors/architecture.md` for the invariant; the code comment near the affected place usually explains why it is the way it is.
