//! Shared corpus + scenarios for the FTS relevance evaluation
//! (`examples/fts_quality.rs`).
//!
//! Kept in its own module so the corpus can grow without burying the harness,
//! and so a future regression test can import the same fixtures rather than
//! inventing a second, divergent set.
//!
//! ## Corpus design
//!
//! ~50 EngramDB-flavoured memories with *deliberately varied* vocabulary. This
//! matters: IDF is only meaningful when term frequencies actually differ across
//! documents, and the `benches/helpers.rs` generator cycles through 10 fixed
//! strings (every 10th memory is textually identical), which would make any
//! IDF-based scorer look artificially good or bad. These are hand-written so
//! that some terms are genuinely rare ("flock", "MemWAL") and some genuinely
//! common ("memory", "store", "config").
//!
//! ## Scenario design
//!
//! Each scenario names the retrieval property it probes, so a win or loss can
//! be attributed rather than just tallied. Scenarios are written as questions a
//! coding agent would actually ask, NOT reverse-engineered from either scorer's
//! strengths — several are cases the current keyword scorer should handle fine,
//! which is the point: a harness that only contains FTS-favourable cases proves
//! nothing.

/// `(id, summary, tags, content)`
pub const CORPUS: &[(&str, &str, &[&str], &str)] = &[
    // --- locking / concurrency -------------------------------------------
    ("m01", "Mutating operations take a per-project advisory flock", &["storage", "concurrency"],
     "Every mutating operation acquires an advisory file lock via flock(2) before touching the store. Reads are lock-free and rely on LanceDB MVCC. The guard releases on drop so early returns and panics cannot strand the lock."),
    ("m02", "Registry writes serialize on a separate lock file", &["storage", "concurrency"],
     "The global registry has its own lock file next to registry.json. Holding it while doing slow directory deletion would block other processes, so the lock is released before cleanup begins."),
    ("m03", "Never re-acquire a lock the current task already holds", &["concurrency", "hazard"],
     "Advisory locks are per open file description. Each acquisition opens a fresh descriptor, so a second acquire from the same task blocks forever against itself. Call only load and save while holding the registry lock."),

    // --- testing ----------------------------------------------------------
    ("m04", "Use cargo nextest, never cargo test", &["testing", "convention"],
     "The suite requires nextest because process-per-test isolation is load-bearing for the environment variable redirection the harness installs. Plain cargo test shares one process and the globals leak between cases."),
    ("m05", "Model-loading tests are serialized into one group", &["testing", "onnx"],
     "Tests that load ONNX models share heavyweight global state, so they run in a group capped at a single thread. Parallel loads transiently fail and the provider resolves to none, which downstream reads as embeddings being unavailable."),
    ("m06", "Two doctor tests are flaky under full parallelism", &["testing", "hazard"],
     "Both pass in isolation and fail identically on a clean base, so they are not a regression signal. Do not chase them when triaging an unrelated failure."),

    // --- embeddings / models ---------------------------------------------
    ("m07", "The default embedding model is int8 quantized MiniLM", &["embeddings", "decision"],
     "Benchmarking showed the quantized variant is roughly 1.4 to 1.9 times faster than fp32 with negligible ranking drift. The model identifier is persisted so a quantization swap is detected and forces a reindex."),
    ("m08", "Embedding fingerprints guard against silent vector corruption", &["embeddings", "storage"],
     "Each store records the model identifier and dimension count. On open the expected fingerprint is compared to the live provider and a mismatch surfaces as a warning telling the operator to reindex with the embeddings-only flag."),
    ("m09", "A pure Rust tract backend covers Intel Mac", &["embeddings", "portability"],
     "No prebuilt ONNX Runtime exists for x86_64 Apple targets past a certain release, so the tract engine provides an fp32 fallback. It is slower but needs no native library at all."),
    ("m10", "The cross encoder reranker is off by default", &["reranking", "config"],
     "Reranking loads a separate BGE model and adds material latency per query, so it stays disabled until explicitly enabled in configuration. When on, it rescores the shortlist and blends with the composite score."),

    // --- daemon -----------------------------------------------------------
    ("m11", "A shared daemon loads each model once per machine", &["daemon", "performance"],
     "Stdio transport gives one process per agent session, so without coordination every concurrent session would load its own copy of the embedding model. The daemon serves inference over a Unix domain socket instead."),
    ("m12", "The daemon reaps only after the last session disconnects", &["daemon"],
     "An idle watchdog exits the process after the configured timeout with no activity and no in-flight connections. Heartbeat pings from live sessions refresh the activity timestamp, keeping it resident while anyone is attached."),
    ("m13", "Daemon failures must never break an operation", &["daemon", "convention"],
     "If the daemon is disabled or unreachable, both front ends load models in process exactly as before. Graceful fallback is the contract; a broken socket degrades speed, never correctness."),
    ("m14", "Socket resolution has a fixed precedence order", &["daemon", "config"],
     "Command line flag beats environment variable, which beats the configuration file, which beats the default per-user path. Every client and server site must call the same helper so they agree on where the socket lives."),

    // --- storage / schema -------------------------------------------------
    ("m15", "Memories live as markdown files with TOML frontmatter", &["storage", "decision"],
     "The files on disk are the source of truth. The vector database holds metadata for filtering plus optional embedding vectors, and can always be rebuilt from the markdown without re-embedding."),
    ("m16", "Schema version bumps trigger an automatic reindex on open", &["storage", "migration"],
     "The column set is versioned in the manifest. When the recorded version lags, the table is rebuilt from the markdown files in seconds, vectors preserved, and the current version is stamped."),
    ("m17", "File writes are atomic temp-then-rename", &["storage", "convention"],
     "A partially written memory file must never be observable. Content is written to a temporary path in the same directory and renamed over the target, which is atomic on every supported filesystem."),
    ("m18", "Every project gets a hashed sixteen character identifier", &["storage"],
     "Identifiers derive from a hash of the canonical project path. The well known global store identifier starts with underscores so it cannot collide with a real project."),

    // --- retrieval / scoring ---------------------------------------------
    ("m19", "Retrieval runs in filter mode or rank mode", &["retrieval", "decision"],
     "Filter mode narrows by query text, path, and tags and requires a query signal. Rank mode orders everything by relevance to the supplied context. The composite formula differs between them."),
    ("m20", "Scope proximity combines physical and logical distance", &["retrieval", "scope"],
     "Physical scope matches file paths with depth decay. Logical scope walks a dotted hierarchy where exact, parent, and sibling relationships score differently. The combined multiplier is capped."),
    ("m21", "Decay strategies are none, linear, exponential, and step", &["scoring"],
     "Exponential decay uses a half life. Step decay holds full relevance until the time to live expires and then drops to a floor. The chosen strategy multiplies into effective relevance."),
    ("m22", "Trust weight varies by provenance source", &["scoring", "config"],
     "Human authored memories carry full trust, agent authored slightly less, and inferred least. The weight is a post multiplier on the base score and is configurable per deployment."),

    // --- scope ------------------------------------------------------------
    ("m23", "Physical scope patterns compile to a glob set", &["scope", "performance"],
     "Compiling every pattern into one matcher lets a single pass test all of them. The root pattern matches everything with a reduced base score so a catch-all memory does not outrank a targeted one."),
    ("m24", "Logical scopes use dot notation with a bounded bonus", &["scope"],
     "Namespaces look like api.auth.oauth. The lowest common ancestor determines closeness and the resulting bonus is clamped so deep hierarchies cannot dominate the ranking."),

    // --- auth / security --------------------------------------------------
    ("m25", "The MCP server authenticates nothing on stdio", &["security", "mcp"],
     "Standard input transport inherits the trust of the spawning process, so no additional authentication is performed. The streamable HTTP transport is the surface that would need it."),
    ("m26", "Socket permissions are restricted before the rename", &["security", "daemon"],
     "The socket is chmoded at its temporary path and only then renamed over the target, so the published path never exposes a group or world accessible endpoint even momentarily."),
    ("m27", "Peer credentials are checked on every connection", &["security", "daemon"],
     "The server reads the connecting peer's effective user id and refuses anyone who is not the owner. This closes the multi-user shared machine case."),

    // --- hooks / integration ---------------------------------------------
    ("m28", "Hook handlers read event JSON on standard input", &["hooks", "integration"],
     "The pre-tool-use and session-start subcommands consume the event payload and emit additional context as JSON on standard output. The session start injection is capped at two thousand characters."),
    ("m29", "Setup writes hook and server entries into settings", &["hooks", "cli"],
     "The setup command edits the settings file, or the project scoped variant, to register both the hook handlers and the memory server. The marketplace plugin performs the same wiring automatically."),

    // --- CLI --------------------------------------------------------------
    ("m30", "Output formatting supports pretty, JSON, and plain", &["cli", "convention"],
     "Pretty mode colours and aligns for humans. JSON mode emits structured records for scripting. Plain mode is minimal and pipe friendly. The operations layer never formats anything itself."),
    ("m31", "Worktree invocations route to the main checkout", &["cli", "worktree"],
     "When run inside a linked worktree, commands transparently resolve to the primary project and register the worktree as a sub project. Initialization and server commands are exempt because they own their own behaviour."),

    // --- gc / compression -------------------------------------------------
    ("m32", "Garbage collection targets low relevance memories", &["gc", "maintenance"],
     "The sweep considers effective relevance after decay, not raw criticality, so a once important but long stale memory becomes eligible. Deletion is staged and reversible until confirmed."),
    ("m33", "Compression merges near duplicate memories", &["compression", "maintenance"],
     "Candidates are surfaced by high pairwise similarity. The actual merge requires a language model to synthesize the combined text, so the command only lists candidates."),

    // --- contradiction ----------------------------------------------------
    ("m34", "Contradiction detection uses a natural language inference model", &["nli", "quality"],
     "A new memory is compared against semantically close existing ones and a strong entailment of the negation flags the pair for review. This is what powers the challenge workflow."),
    ("m35", "Challenged memories carry a scoring penalty", &["nli", "scoring"],
     "An open challenge subtracts from the final score so disputed knowledge sinks without disappearing. Resolving the challenge restores the original ranking."),

    // --- telemetry --------------------------------------------------------
    ("m36", "Request metrics persist across daemon restarts", &["telemetry", "daemon"],
     "Counters are written into the global store so cumulative figures survive a restart and can be reported even when no daemon is currently running. Heartbeat ping counts are in memory only."),
    ("m37", "Stage timings are recorded per query", &["telemetry", "performance"],
     "Embedding, vector search, scoring, and reranking each record elapsed milliseconds. The breakdown is what makes a slow query diagnosable without a profiler."),

    // --- config -----------------------------------------------------------
    ("m38", "Provider caches key on model affecting configuration only", &["config", "hazard"],
     "The cache key covers backend, provider, dimensions, and the model selections. Routing only fields such as idle timeout deliberately do not participate. Adding a model affecting field without extending the key serves stale bundles."),
    ("m39", "Summary length is validated before a memory is stored", &["config", "convention"],
     "The limit is enforced at the operations layer so an oversized summary never reaches storage. The bound is configurable and exists to keep the metadata table compact."),

    // --- groups / sharing -------------------------------------------------
    ("m40", "Group stores fan into a subscribed project's queries", &["groups", "sharing"],
     "Knowledge shared by a set of related repositories is written once to a named group. Any project subscribed to that group sees those memories in its own results without duplicating them."),
    ("m41", "Audience lists scope a single memory to named projects", &["groups", "sharing"],
     "Setting an audience on a global or group write surfaces it only for the listed projects. This is advisory fan-in scoping and explicitly not a confidentiality boundary."),

    // --- build / release --------------------------------------------------
    ("m42", "Release builds optimize aggressively for size", &["build", "release"],
     "Link time optimization is on, the optimization level targets size, symbols are stripped, panics abort, and code generation uses a single unit. The result is a materially smaller shipped binary."),
    ("m43", "Dependency debug info is dropped in development builds", &["build", "performance"],
     "Full debug information across the whole tree produced an enormous target directory. Workspace crates keep line tables so backtraces still resolve, while dependencies build with none at all."),
    ("m44", "The linker is switched to lld on Linux", &["build", "performance"],
     "Linking pulls in hundreds of crates worth of objects and the release profile makes it a heavy serial phase. The alternative linker is substantially faster with no behavioural change."),

    // --- fuzzing ----------------------------------------------------------
    ("m45", "Fuzz targets only call already public pure functions", &["fuzzing", "convention"],
     "No API was widened for fuzzing. If a surface sits behind a private module, drive it through an existing public entry point transitively rather than exposing it."),
    ("m46", "Score math asserts finiteness, not bounds", &["fuzzing", "scoring"],
     "The fuzzer ignores configuration validation, so only the weakest invariant that must always hold can be asserted. Tighter ranges are checked only after inputs are clamped into their valid domain."),

    // --- MemWAL / rare vocabulary ----------------------------------------
    ("m47", "MemWAL sharded writes are not enabled", &["storage", "decision"],
     "The log structured write path targets high rate sharded ingest and requires an explicit specification to be installed. Our write pattern is a single upsert per memory on a local store, so it stays off."),
    ("m48", "Blob columns are unused because content lives in files", &["storage"],
     "The large object storage path would matter if bodies were kept in the table. They are not; the table holds metadata and vectors, and the markdown file remains authoritative."),

    // --- BM25 stress cases -------------------------------------------------
    //
    // The entries below exist to exercise BM25's *scoring* rather than its
    // tokenizer: length normalisation and term-frequency saturation. Without
    // them the corpus has no document where "matched a query word" and "is
    // actually about that word" come apart, and a scorer with no notion of
    // document length or term density can never be shown to be wrong.

    // Deliberately long and unfocused: touches many topics in passing, so it
    // collides with lots of queries while being the right answer to none of
    // them. A scorer without length normalisation over-rewards it because it
    // simply contains more words.
    ("m49", "Onboarding notes covering the whole system", &["onboarding", "overview"],
     "This is a broad tour for new contributors. It mentions the store and the memory files and the index and the daemon and the socket and the config and the schema and the migration and the reranker and the embedding model and the scoring formula and the decay strategies and the scope matching and the retrieval modes and the hooks and the CLI output and the telemetry counters and the garbage collection sweep and the compression candidates and the group stores and the audience lists and the build profiles and the linker and the fuzz targets and the release process. None of these are explained in any depth here; each has its own dedicated memory which should be preferred whenever a question is actually about that topic, because this page only names them in passing without describing behaviour, rationale, configuration, or failure modes."),

    // Repeats a common term many times without being the best answer about
    // it. Rewards raw term frequency; BM25 saturates, a linear count does not.
    ("m50", "Meeting notes where the daemon came up repeatedly", &["notes"],
     "We talked about the daemon. Someone asked whether the daemon should start automatically and whether the daemon should stop on idle and whether the daemon logs enough. The daemon came up again later when discussing the daemon socket and the daemon lifetime. No decisions were recorded about the daemon in this meeting; see the dedicated daemon memories for the actual behaviour."),

    // Short and precise: the correct answer to a question that m49 and m50
    // both partially collide with.
    ("m51", "The idle timeout is configurable per deployment", &["daemon", "config"],
     "Set the idle timeout in configuration to control how long an unused process lingers before exiting."),

    // --- Tie-break pairs ---------------------------------------------------
    //
    // Each pair is constructed so the CURRENT scorer must score both members
    // *identically*: the summaries contain the same query terms, and both
    // contents contain every query term at least once. Because the scorer
    // compares distinct-token sets, "mentions it once in passing" and "is
    // entirely about it" are indistinguishable. The decoy is listed first so
    // an exact tie resolves the wrong way under a stable sort.

    // Pair A — query "retry policy"
    ("m52", "Retry policy came up in an old design review", &["notes"],
     "A wide ranging review covering deployment, packaging, logging, metrics, alerting, the release checklist, the rollback procedure, the on call rotation, and the escalation path. The retry policy was mentioned once and deferred without a decision being recorded anywhere in this document."),
    ("m53", "Retry policy for transient network failures", &["reliability", "decision"],
     "The retry policy applies to transient network failures. Retry three times with exponential backoff, and treat a retry budget exhaustion as a hard failure. This policy is the one to follow."),

    // Pair B — query "cache invalidation"
    ("m54", "Cache invalidation was raised during onboarding", &["notes"],
     "New joiners ask about many subsystems: the build, the test harness, the linter, the formatter, the documentation site, the changelog, the issue triage rota, and the support handover. Cache invalidation came up briefly and was answered verbally."),
    ("m55", "Cache invalidation happens on config change", &["config", "decision"],
     "Cache invalidation is triggered whenever a model affecting field changes. The cache key covers the fields that alter results, so invalidation is precise rather than wholesale."),

    // Pair C — query "backpressure"
    ("m56", "Backpressure listed among future work", &["notes"],
     "The roadmap section enumerates possible directions including sharding, tiering, replication, quotas, auditing, multi tenancy, and cost accounting. Backpressure appears in that list with no further detail."),
    ("m57", "Backpressure is applied by bounding the queue", &["performance", "decision"],
     "Backpressure comes from a bounded queue. When the queue is full the producer blocks, which propagates backpressure upstream instead of growing memory without limit."),
];

/// What retrieval property a scenario probes. Used to break the results down
/// so a win can be attributed to a mechanism rather than to luck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Query words appear verbatim. Both scorers should succeed; these guard
    /// against the new system regressing the easy cases.
    Exact,
    /// Query uses a different inflection than the document ("authenticating"
    /// vs "authenticates"). Needs stemming.
    Stemming,
    /// Query is mostly stopwords plus one discriminative term. Needs IDF (or
    /// stopword removal) to avoid the common words dominating.
    Idf,
    /// Query term is misspelled. Needs fuzzy matching.
    Typo,
    /// Query terms are individually common but the combination is specific.
    Discrimination,
    /// A long, unfocused document collides with the query while a short,
    /// precise one is the real answer. Needs document-length normalisation —
    /// a mechanism stemming cannot supply.
    LengthNorm,
    /// A document repeats a query term many times without being the best
    /// answer about it. Needs term-frequency saturation.
    TermFreq,
    /// Two documents whose *summaries* match the query identically, differing
    /// only in how strongly the content is about it. The current scorer
    /// compares distinct-token sets per field, so once both contents contain
    /// the query terms at all the scores are exactly equal and the winner is
    /// decided by store order — an arbitrary tiebreak. BM25 can separate them
    /// on term density and length.
    ///
    /// The decoy is placed FIRST in the corpus in each pair, so a genuine tie
    /// surfaces as a miss rather than being masked by stable-sort luck.
    TieBreak,
}

/// `(query, relevant ids, probe, note)`
///
/// Relevance is judged by "would an agent asking this be satisfied by this
/// memory", and several scenarios list more than one acceptable answer.
pub const SCENARIOS: &[(&str, &[&str], Probe, &str)] = &[
    // -- Exact: both systems should get these. Regression guards. ----------
    (
        "advisory flock",
        &["m01"],
        Probe::Exact,
        "rare term, verbatim",
    ),
    (
        "cargo nextest",
        &["m04"],
        Probe::Exact,
        "verbatim tool name",
    ),
    (
        "unix domain socket daemon",
        &["m11"],
        Probe::Exact,
        "verbatim phrase",
    ),
    (
        "glob set physical scope",
        &["m23"],
        Probe::Exact,
        "verbatim",
    ),
    ("MemWAL", &["m47"], Probe::Exact, "very rare term"),
    (
        "temp-then-rename atomic writes",
        &["m17"],
        Probe::Exact,
        "verbatim",
    ),
    // -- Stemming: the inflected term is the ONLY discriminative token ------
    //
    // These are deliberately terse. An earlier version phrased them as natural
    // questions ("compressing duplicate memories") — but the extra words
    // ("duplicate", "memories") already identified the document on their own,
    // so the scenario passed without any stemming and proved nothing. Each
    // query below reduces to one term whose exact form appears nowhere in the
    // corpus; only a stemmer can bridge it.
    (
        "authenticating",
        &["m25"],
        Probe::Stemming,
        "corpus has 'authenticates'",
    ),
    (
        "compressing",
        &["m33"],
        Probe::Stemming,
        "corpus has 'Compression'",
    ),
    (
        "serializing",
        &["m02"],
        Probe::Stemming,
        "corpus has 'serialize'",
    ),
    (
        "validating",
        &["m39"],
        Probe::Stemming,
        "corpus has 'validated'",
    ),
    (
        "quantizing",
        &["m07"],
        Probe::Stemming,
        "corpus has 'quantized'/'quantization'",
    ),
    (
        "expiring",
        &["m21"],
        Probe::Stemming,
        "corpus has 'expires'",
    ),
    // -- IDF: mostly stopwords plus one rare term --------------------------
    (
        "what is the flock for",
        &["m01"],
        Probe::Idf,
        "4 stopwords + 1 rare term",
    ),
    (
        "how does the daemon know when to exit",
        &["m12"],
        Probe::Idf,
        "mostly stopwords",
    ),
    (
        "why is it that the reranker is off",
        &["m10"],
        Probe::Idf,
        "stopword heavy",
    ),
    (
        "what do we do about the worktree",
        &["m31"],
        Probe::Idf,
        "stopword heavy",
    ),
    (
        "is there a limit on the summary",
        &["m39"],
        Probe::Idf,
        "stopword heavy",
    ),
    // -- Typo: fuzzy only. Single term so nothing else can rescue the query.
    // Mixed edit distances on purpose: a dropped letter is distance 1, a
    // transposition is distance 2 under plain Levenshtein, and AUTO fuzziness
    // only allows 2 for tokens of 6+ characters — so `flcok` (5 chars,
    // transposed) is expected to stay out of reach even for the fuzzy arm.
    (
        "rerankng",
        &["m10"],
        Probe::Typo,
        "dropped letter, distance 1",
    ),
    (
        "telemtry",
        &["m36", "m37"],
        Probe::Typo,
        "dropped letter, distance 1",
    ),
    (
        "quantizd",
        &["m07"],
        Probe::Typo,
        "dropped letter, distance 1",
    ),
    (
        "nextset",
        &["m04"],
        Probe::Typo,
        "transposition, distance 2",
    ),
    (
        "flcok",
        &["m01"],
        Probe::Typo,
        "transposition, 5 chars -> auto allows only 1",
    ),
    // -- Discrimination: common words, specific combination -----------------
    (
        "model loading tests",
        &["m05"],
        Probe::Discrimination,
        "'model'/'tests' both common",
    ),
    (
        "storage schema version",
        &["m16"],
        Probe::Discrimination,
        "all three terms common",
    ),
    (
        "memory scoring penalty",
        &["m35"],
        Probe::Discrimination,
        "'memory'/'scoring' common",
    ),
    (
        "build performance linker",
        &["m44"],
        Probe::Discrimination,
        "'build'/'performance' common",
    ),
    (
        "configuration cache key",
        &["m38"],
        Probe::Discrimination,
        "'config'/'cache' common",
    ),
    (
        "socket permissions",
        &["m26"],
        Probe::Discrimination,
        "'socket' appears in several",
    ),
    // -- Length normalisation: the sprawling m49 collides with all of these,
    // -- naming each topic verbatim while explaining none of them.
    (
        "garbage collection sweep",
        &["m32"],
        Probe::LengthNorm,
        "m49 name-drops it verbatim",
    ),
    (
        "compression candidates",
        &["m33"],
        Probe::LengthNorm,
        "m49 name-drops it verbatim",
    ),
    (
        "decay strategies",
        &["m21"],
        Probe::LengthNorm,
        "m49 name-drops it verbatim",
    ),
    (
        "audience lists",
        &["m41"],
        Probe::LengthNorm,
        "m49 name-drops it verbatim",
    ),
    (
        "scope matching",
        &["m20", "m23", "m24"],
        Probe::LengthNorm,
        "m49 name-drops it verbatim",
    ),
    // -- Term frequency: m50 says "daemon" six times but answers nothing ----
    (
        "daemon idle timeout",
        &["m51", "m12"],
        Probe::TermFreq,
        "m50 repeats 'daemon'",
    ),
    (
        "daemon socket",
        &["m14", "m26", "m11"],
        Probe::TermFreq,
        "m50 uses this exact pair",
    ),
    (
        "daemon lifetime",
        &["m12"],
        Probe::TermFreq,
        "m50 uses this exact phrase",
    ),
    // -- Tie-break: summaries match equally, only content density differs ---
    (
        "retry policy",
        &["m53"],
        Probe::TieBreak,
        "m52 mentions it once; both summaries carry both terms",
    ),
    (
        "cache invalidation",
        &["m55"],
        Probe::TieBreak,
        "m54 mentions it once; both summaries carry both terms",
    ),
    (
        "backpressure",
        &["m57"],
        Probe::TieBreak,
        "m56 lists it; both summaries carry the term",
    ),
];

#[allow(dead_code)]
fn main() {}
