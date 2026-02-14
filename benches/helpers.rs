use engramdb::storage::{InMemoryRegistry, MemoryStore};
use engramdb::types::{
    Decay, DecayStrategy, EngramConfig, Memory, MemoryType, Provenance, Visibility,
};
use tempfile::TempDir;

/// Physical scope patterns distributed across common project paths.
const PHYSICAL_PATTERNS: &[&str] = &[
    "src/main.rs",
    "src/lib.rs",
    "src/types/memory.rs",
    "src/types/config.rs",
    "src/storage/store.rs",
    "src/storage/lance_index.rs",
    "src/scoring/composite.rs",
    "src/scoring/decay.rs",
    "src/retrieval/engine.rs",
    "src/retrieval/filters.rs",
    "src/scope/physical.rs",
    "src/scope/logical.rs",
    "src/cli/commands/add.rs",
    "src/cli/commands/retrieve.rs",
    "src/ops/mod.rs",
    "src/ops/retrieve.rs",
    "src/mcp/server.rs",
    "tests/integration.rs",
    "docs/architecture.md",
    "src/**/*.rs",
];

/// Logical scope patterns representing common domain areas.
const LOGICAL_SCOPES: &[&str] = &[
    "api.auth",
    "api.auth.oauth",
    "api.auth.jwt",
    "storage.lance",
    "storage.files",
    "cli.commands",
    "cli.output",
    "scoring.composite",
    "scoring.decay",
    "retrieval.engine",
    "retrieval.filters",
    "scope.physical",
    "scope.logical",
    "types.memory",
    "types.config",
];

/// Memory types to distribute across generated memories.
const MEMORY_TYPES: &[MemoryType] = &[
    MemoryType::Decision,
    MemoryType::Convention,
    MemoryType::Hazard,
    MemoryType::Context,
    MemoryType::Intent,
    MemoryType::Relationship,
    MemoryType::Debug,
    MemoryType::Preference,
];

/// Summaries for generated memories — short, realistic strings.
const SUMMARIES: &[&str] = &[
    "Use LanceDB for unified index storage",
    "Always validate summary length before create",
    "Scoring uses three modes: semantic, degraded, scope_only",
    "Physical scope matches via globset patterns",
    "Logical scope uses dot-notation hierarchy",
    "Decay strategies: none, linear, exponential, step",
    "Trust weights vary by provenance source",
    "Reranking blends cross-encoder scores with composite",
    "CLI output supports pretty, json, and plain formats",
    "Hook runs on every Read/Write/Edit operation",
];

/// Content strings for generated memories — roughly 50-100 words.
const CONTENTS: &[&str] = &[
    "The LanceDB integration stores both metadata and embedding vectors. Metadata lives in a \
     memories table while embedding chunks are in a separate chunks table. Vector search queries \
     the chunks table and aggregates results by memory_id using max-score.",
    "Summary validation ensures the summary field does not exceed 100 characters. This is \
     enforced at the ops layer before the memory reaches storage, preventing index bloat.",
    "Composite scoring combines relevance (criticality * decay), scope proximity, and optional \
     semantic similarity. The weights depend on the scoring mode selected at retrieval time.",
    "Physical scope matching uses GlobSet for efficient multi-pattern matching. The root pattern \
     '/' matches everything with a base score of 0.4. Exact file matches score 1.0.",
    "Logical scopes follow a dot-notation hierarchy where parent-child and sibling relationships \
     are scored differently. Exact matches get 0.3, parent/child 0.2, siblings 0.15.",
    "Four decay strategies are supported. None keeps relevance constant. Linear decreases \
     linearly over TTL. Exponential uses half-life. Step drops to floor after TTL.",
    "Trust weights adjust the final score based on who created the memory. Human sources get 1.0, \
     agents 0.85, imported 0.7, and inferred 0.6. These are configurable.",
    "Cross-encoder reranking takes the top N candidates, scores them with a BERT-based model, \
     normalizes the scores, and blends them with the original composite scores.",
    "The CLI output formatter supports three modes. Pretty mode uses colored output with tables. \
     JSON mode outputs structured data. Plain mode is minimal for piping.",
    "The PreToolUse hook spawns a subprocess that opens the store, builds a retrieval engine \
     with scope-only scoring, retrieves relevant memories, and returns JSON.",
];

/// Generate a realistic test memory with deterministic data based on index.
pub fn generate_memory(index: usize) -> Memory {
    let type_ = MEMORY_TYPES[index % MEMORY_TYPES.len()];
    let summary = SUMMARIES[index % SUMMARIES.len()];
    let content = CONTENTS[index % CONTENTS.len()];

    let mut memory = Memory::new(type_, summary, content, provenance_for_index(index));

    // Assign 1-3 physical scopes based on index
    let phys_start = index % PHYSICAL_PATTERNS.len();
    let phys_count = 1 + (index % 3);
    memory.physical = (0..phys_count)
        .map(|i| PHYSICAL_PATTERNS[(phys_start + i) % PHYSICAL_PATTERNS.len()].to_string())
        .collect();

    // Assign 1-2 logical scopes
    let log_start = index % LOGICAL_SCOPES.len();
    let log_count = 1 + (index % 2);
    memory.logical = (0..log_count)
        .map(|i| LOGICAL_SCOPES[(log_start + i) % LOGICAL_SCOPES.len()].to_string())
        .collect();

    // Vary criticality between 0.3 and 1.0
    memory.criticality = 0.3 + (((index * 7) % 8) as f64 * 0.1);

    // Add tags
    memory.tags = vec![format!("tag-{}", index % 5), format!("group-{}", index % 3)];

    // Vary decay strategy
    memory.decay = match index % 4 {
        0 => Some(Decay::none()),
        1 => Some(Decay::exponential(chrono::Duration::days(7))),
        2 => Some(Decay::linear(chrono::Duration::days(30))),
        3 => Some(Decay {
            strategy: DecayStrategy::Step,
            half_life: None,
            ttl: Some(chrono::Duration::days(14)),
            floor: 0.2,
        }),
        _ => unreachable!(),
    };

    memory.visibility = Visibility::Shared;
    memory
}

/// Return a provenance that varies by index.
fn provenance_for_index(index: usize) -> Provenance {
    match index % 4 {
        0 => Provenance::human(),
        1 => Provenance::agent("claude-opus"),
        2 => Provenance::inferred(),
        3 => Provenance::imported(),
        _ => unreachable!(),
    }
}

/// Create a temporary store pre-populated with `count` memories.
///
/// Returns the TempDir (must be kept alive) and an open MemoryStore.
pub async fn setup_store(count: usize) -> (TempDir, MemoryStore) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .expect("failed to init store");

    for i in 0..count {
        let memory = generate_memory(i);
        store
            .create(&memory)
            .await
            .expect("failed to create memory");
    }

    (temp_dir, store)
}

/// Return a default config with embeddings and reranking disabled (scope_only benchmarks).
pub fn default_config() -> EngramConfig {
    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;
    config
}

/// Generate the JSON that Claude Code sends to the PreToolUse hook.
pub fn sample_hook_json(project_dir: &std::path::Path, relative_path: &str) -> String {
    serde_json::json!({
        "tool_name": "Read",
        "tool_input": {
            "file_path": project_dir.join(relative_path).to_string_lossy()
        }
    })
    .to_string()
}
