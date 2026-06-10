#!/usr/bin/env python3
"""Generate the benchmark dataset: 10 projects x 10 memories + 10 global memories.

The store is isolated under $ENGRAMDB_DATA_DIR / $ENGRAMDB_CONFIG_DIR (set by
run_all.sh) so it never touches a developer's real EngramDB data. Each project
gets a coherent theme so semantic ("rank"/"filter") queries have real signal.

Memories are added via the `engramdb` binary. A daemon is assumed to be running
(run_all.sh starts one) so embeddings load once instead of per-invocation.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

BIN = os.environ["ENGRAM_BIN"]
ROOT = Path(os.environ["BENCH_PROJECTS_DIR"])

TYPES = ["decision", "convention", "hazard", "context", "intent",
         "relationship", "debug", "preference"]

# 10 project themes, each a (slug, domain-blurb, [10 memory specs]).
# A memory spec is (type, title, summary, content, logical, tags).
PROJECTS = {
    "web-api": "REST API service in Rust with axum",
    "auth-service": "OAuth2 / JWT authentication microservice",
    "payment-gateway": "Stripe-backed payment processing service",
    "frontend-spa": "React single-page application",
    "data-pipeline": "Airflow ETL and batch data pipeline",
    "ml-platform": "model training and inference platform",
    "mobile-app": "Flutter cross-platform mobile client",
    "search-engine": "Elasticsearch-backed full-text search service",
    "devops-infra": "Terraform and Kubernetes infrastructure",
    "analytics-db": "ClickHouse analytics warehouse",
}

# Per-theme memory seed phrases — 10 each. Kept varied so vector search ranks
# differently per query.
SEEDS = {
    "web-api": [
        ("decision", "Use axum framework", "Chose axum over actix for the HTTP layer",
         "We standardized on axum 0.7 because of tower middleware compatibility and tokio alignment.", "api.http", "rust,axum,http"),
        ("convention", "Error responses as RFC7807", "All API errors follow problem+json",
         "Every handler returns RFC 7807 problem details with a stable type URI.", "api.errors", "errors,convention"),
        ("hazard", "N+1 query in list endpoint", "GET /users triggers N+1 DB calls",
         "The users list endpoint lazily loads roles per row; batch it with a join.", "api.users", "performance,sql,hazard"),
        ("context", "Rate limiting via tower", "Per-IP rate limit middleware",
         "Rate limiting uses a tower layer backed by a redis token bucket.", "api.middleware", "ratelimit,redis"),
        ("convention", "Versioned routes under /v1", "URL path versioning",
         "All routes are mounted under /v1; breaking changes bump to /v2.", "api.routing", "versioning"),
        ("decision", "Pagination is cursor-based", "Opaque cursor pagination",
         "List endpoints use opaque base64 cursors, never offset pagination.", "api.pagination", "pagination,decision"),
        ("debug", "504 on cold start", "First request after deploy times out",
         "Cold-start 504s came from lazy DB pool init; warm the pool on startup.", "api.startup", "debug,timeout"),
        ("intent", "Add OpenAPI generation", "Auto-generate OpenAPI spec",
         "We plan to derive the OpenAPI 3.1 spec from handler types via utoipa.", "api.docs", "openapi,intent"),
        ("preference", "Prefer thiserror", "Use thiserror for error enums",
         "Library crates use thiserror; the binary uses anyhow at the edges.", "api.errors", "errors,preference"),
        ("relationship", "Auth depends on auth-service", "Token validation is remote",
         "The web-api validates JWTs by calling the auth-service introspection endpoint.", "api.auth", "auth,dependency"),
    ],
    "auth-service": [
        ("decision", "JWT with RS256", "Asymmetric JWT signing",
         "Tokens are signed RS256 so resource servers verify with the public JWKS.", "auth.jwt", "jwt,rs256"),
        ("hazard", "Refresh token reuse", "Replay of rotated refresh tokens",
         "Detect refresh-token reuse and revoke the whole family on replay.", "auth.refresh", "security,hazard"),
        ("convention", "Scopes are colon-namespaced", "scope format service:action",
         "OAuth scopes use the form service:action, e.g. payments:read.", "auth.scopes", "oauth,convention"),
        ("decision", "Sessions in Redis", "Server-side session store",
         "Opaque session ids map to Redis records with a sliding 30m TTL.", "auth.session", "redis,session"),
        ("hazard", "Timing attack on login", "Constant-time compare needed",
         "Password verification must use constant-time compare to avoid timing leaks.", "auth.login", "security,timing"),
        ("context", "JWKS rotation every 90d", "Key rotation schedule",
         "Signing keys rotate every 90 days with a 7-day overlap window.", "auth.keys", "rotation,jwks"),
        ("intent", "Add WebAuthn support", "Passkey login",
         "We intend to add WebAuthn passkeys as a second-factor option.", "auth.mfa", "webauthn,intent"),
        ("debug", "Clock skew rejects tokens", "iat in the future",
         "Tokens were rejected due to 2s clock skew; add a leeway window.", "auth.jwt", "debug,clock"),
        ("preference", "Argon2id for hashing", "Password KDF choice",
         "Use argon2id with 64MB memory cost for password hashing.", "auth.login", "argon2,preference"),
        ("relationship", "Issues tokens for all services", "Central token authority",
         "auth-service is the sole token issuer consumed by web-api and payment-gateway.", "auth.jwt", "dependency"),
    ],
    "payment-gateway": [
        ("decision", "Use Stripe PaymentIntents", "Stripe as PSP",
         "All charges go through Stripe PaymentIntents with 3D Secure.", "pay.stripe", "stripe,decision"),
        ("hazard", "Double-charge on retry", "Idempotency required",
         "Network retries can double-charge; always send a Stripe idempotency key.", "pay.charge", "idempotency,hazard"),
        ("convention", "Money in integer cents", "Never float money",
         "Monetary amounts are stored as integer minor units, never floats.", "pay.money", "money,convention"),
        ("decision", "Webhooks are source of truth", "Async settlement",
         "Charge state is reconciled from Stripe webhooks, not the API response.", "pay.webhook", "webhook,decision"),
        ("hazard", "Unverified webhook signatures", "Forge risk",
         "Reject webhooks whose Stripe-Signature header fails verification.", "pay.webhook", "security,hazard"),
        ("context", "PCI scope is minimized", "No card data stored",
         "We never store PANs; Stripe.js tokenizes cards client-side.", "pay.pci", "pci,compliance"),
        ("intent", "Add refund automation", "Self-serve refunds",
         "Plan a refund workflow with approval thresholds above $500.", "pay.refund", "refund,intent"),
        ("debug", "Currency mismatch error", "EUR charge on USD account",
         "A currency mismatch came from a hardcoded USD; read it from the order.", "pay.charge", "debug,currency"),
        ("preference", "Prefer idempotent ledger writes", "Append-only ledger",
         "Ledger entries are append-only and idempotent on (order_id, kind).", "pay.ledger", "ledger,preference"),
        ("relationship", "Calls auth-service for scopes", "Authz on refunds",
         "Refund endpoints check payments:refund scope via auth-service.", "pay.refund", "auth,dependency"),
    ],
    "frontend-spa": [
        ("decision", "React with TanStack Query", "Data fetching layer",
         "Server state is managed by TanStack Query; no global redux store.", "ui.data", "react,decision"),
        ("convention", "CSS modules only", "No global CSS",
         "Styling uses CSS modules scoped per component; avoid global selectors.", "ui.style", "css,convention"),
        ("hazard", "XSS via dangerouslySetInnerHTML", "Sanitize HTML",
         "Any dangerouslySetInnerHTML must run through DOMPurify first.", "ui.security", "xss,hazard"),
        ("context", "Bundled with Vite", "Build tooling",
         "The app builds with Vite and code-splits routes lazily.", "ui.build", "vite,context"),
        ("decision", "Routing via React Router 6", "Nested routes",
         "We use React Router 6 data routers with loaders per route.", "ui.routing", "router,decision"),
        ("hazard", "Token in localStorage", "Prefer httpOnly cookie",
         "Storing the JWT in localStorage exposes it to XSS; move to httpOnly cookie.", "ui.security", "security,hazard"),
        ("intent", "Adopt server components", "RSC migration",
         "We intend to migrate to a Next.js app router with server components.", "ui.arch", "rsc,intent"),
        ("debug", "Hydration mismatch", "SSR vs client diff",
         "A hydration mismatch came from rendering Date.now during SSR.", "ui.ssr", "debug,hydration"),
        ("preference", "Prefer composition over HOCs", "Hooks over HOCs",
         "Favor custom hooks and composition rather than higher-order components.", "ui.arch", "hooks,preference"),
        ("relationship", "Consumes web-api v1", "Backend contract",
         "The SPA depends on web-api /v1 and breaks if cursors change.", "ui.data", "api,dependency"),
    ],
    "data-pipeline": [
        ("decision", "Airflow for orchestration", "DAG scheduler",
         "Batch jobs are orchestrated as Airflow DAGs on a daily schedule.", "etl.orch", "airflow,decision"),
        ("convention", "Idempotent backfills", "Re-runnable tasks",
         "Every task is idempotent so backfills can replay any partition.", "etl.tasks", "idempotency,convention"),
        ("hazard", "Schema drift breaks load", "Upstream changes",
         "Unannounced source schema drift silently drops columns on load.", "etl.schema", "schema,hazard"),
        ("context", "Parquet on S3", "Storage format",
         "Intermediate data lands as partitioned Parquet in S3 by event date.", "etl.storage", "parquet,context"),
        ("decision", "dbt for transforms", "SQL transform layer",
         "Transformations live in dbt models with tests on primary keys.", "etl.transform", "dbt,decision"),
        ("hazard", "Timezone in partition key", "UTC required",
         "Partitioning on local time caused gaps; always partition in UTC.", "etl.schema", "timezone,hazard"),
        ("intent", "Move to streaming", "Kafka ingestion",
         "We intend to add a Kafka streaming path for near-real-time metrics.", "etl.stream", "kafka,intent"),
        ("debug", "OOM on large join", "Spark executor memory",
         "A large shuffle join OOMed; repartition and bump executor memory.", "etl.spark", "debug,oom"),
        ("preference", "Prefer columnar formats", "Avoid CSV",
         "Prefer Parquet/ORC over CSV for all intermediate datasets.", "etl.storage", "parquet,preference"),
        ("relationship", "Feeds analytics-db", "Downstream warehouse",
         "The pipeline loads curated tables consumed by analytics-db.", "etl.load", "dependency"),
    ],
    "ml-platform": [
        ("decision", "ONNX for inference", "Runtime choice",
         "Models are exported to ONNX and served with onnxruntime for portability.", "ml.serve", "onnx,decision"),
        ("convention", "Experiments tracked in MLflow", "Reproducibility",
         "Every training run logs params and metrics to MLflow.", "ml.train", "mlflow,convention"),
        ("hazard", "Train/serve skew", "Feature parity",
         "Feature transforms must match between training and serving paths.", "ml.features", "skew,hazard"),
        ("context", "GPU autoscaling", "Inference scaling",
         "Inference pods autoscale on GPU utilization with a 60s cooldown.", "ml.serve", "gpu,context"),
        ("decision", "Feature store is Feast", "Central features",
         "Online features are served from Feast backed by Redis.", "ml.features", "feast,decision"),
        ("hazard", "Data leakage in CV", "Target leakage",
         "Cross-validation leaked the target via a pre-computed aggregate.", "ml.train", "leakage,hazard"),
        ("intent", "Add model monitoring", "Drift detection",
         "We intend to add population-stability drift alerts on inputs.", "ml.monitor", "drift,intent"),
        ("debug", "Quantization accuracy drop", "int8 regression",
         "int8 quantization dropped F1 by 4 points; use per-channel scales.", "ml.serve", "debug,quant"),
        ("preference", "Prefer batch inference", "Throughput over latency",
         "For nightly scoring, prefer batched inference over per-row calls.", "ml.serve", "batch,preference"),
        ("relationship", "Pulls features from data-pipeline", "Upstream features",
         "Training data is produced by the data-pipeline curated tables.", "ml.features", "dependency"),
    ],
    "mobile-app": [
        ("decision", "Flutter for cross-platform", "Single codebase",
         "We use Flutter to ship iOS and Android from one Dart codebase.", "mob.core", "flutter,decision"),
        ("convention", "Riverpod for state", "State management",
         "App state uses Riverpod providers; avoid setState for shared state.", "mob.state", "riverpod,convention"),
        ("hazard", "Token refresh on resume", "Stale session",
         "On app resume a stale token causes a 401 storm; refresh proactively.", "mob.auth", "auth,hazard"),
        ("context", "Offline-first cache", "Local DB",
         "Reads come from a local SQLite cache synced in the background.", "mob.cache", "offline,context"),
        ("decision", "Push via FCM", "Notifications",
         "Push notifications are delivered through Firebase Cloud Messaging.", "mob.push", "fcm,decision"),
        ("hazard", "PII in logs", "Privacy leak",
         "Crash logs must scrub email and phone before upload.", "mob.privacy", "privacy,hazard"),
        ("intent", "Add biometric unlock", "FaceID/TouchID",
         "We intend to gate the app with biometric unlock on cold start.", "mob.auth", "biometric,intent"),
        ("debug", "Jank on list scroll", "Rebuild storm",
         "List jank came from rebuilding the whole tree; add const widgets.", "mob.perf", "debug,jank"),
        ("preference", "Prefer go_router", "Declarative routing",
         "Use go_router for declarative, deep-link-friendly navigation.", "mob.nav", "router,preference"),
        ("relationship", "Talks to web-api", "Backend client",
         "The mobile client consumes web-api /v1 and the auth-service.", "mob.core", "api,dependency"),
    ],
    "search-engine": [
        ("decision", "Elasticsearch backend", "Search store",
         "Full-text search is served by Elasticsearch with custom analyzers.", "se.core", "elasticsearch,decision"),
        ("convention", "Index aliases for zero-downtime", "Reindex pattern",
         "Writes target an alias so reindexing swaps without downtime.", "se.index", "alias,convention"),
        ("hazard", "Mapping explosion", "Dynamic fields",
         "Dynamic mapping on user fields exploded the index; disable it.", "se.index", "mapping,hazard"),
        ("context", "BM25 plus reranker", "Hybrid ranking",
         "Results from BM25 are reranked by a cross-encoder for the top 50.", "se.rank", "bm25,context"),
        ("decision", "Synonyms at query time", "Synonym strategy",
         "Synonyms expand at query time, not index time, for easy updates.", "se.analysis", "synonyms,decision"),
        ("hazard", "Deep pagination cost", "from+size blowup",
         "Deep from+size pagination is O(n); use search_after instead.", "se.query", "pagination,hazard"),
        ("intent", "Add vector search", "kNN hybrid",
         "We intend to add dense-vector kNN alongside BM25 for semantic recall.", "se.rank", "vector,intent"),
        ("debug", "Relevance regression", "Analyzer change",
         "A relevance drop traced to an analyzer change dropping stopwords.", "se.analysis", "debug,relevance"),
        ("preference", "Prefer keyword for facets", "Exact-match fields",
         "Use keyword type for facet/aggregation fields, text for search.", "se.index", "facets,preference"),
        ("relationship", "Indexes analytics-db rows", "Source of documents",
         "Documents are sourced from analytics-db product tables.", "se.index", "dependency"),
    ],
    "devops-infra": [
        ("decision", "Terraform for IaC", "Infra as code",
         "All cloud infra is declared in Terraform with remote state in S3.", "infra.iac", "terraform,decision"),
        ("convention", "One module per service", "Module layout",
         "Each service gets its own Terraform module with a stable interface.", "infra.modules", "modules,convention"),
        ("hazard", "State file lock contention", "Concurrent applies",
         "Concurrent applies corrupt state without a DynamoDB lock table.", "infra.state", "state,hazard"),
        ("context", "EKS for workloads", "Kubernetes",
         "Workloads run on EKS with cluster-autoscaler and Karpenter.", "infra.k8s", "eks,context"),
        ("decision", "GitOps with ArgoCD", "Deploy model",
         "Deployments are GitOps via ArgoCD syncing from the env repo.", "infra.deploy", "argocd,decision"),
        ("hazard", "Secrets in plan output", "Leak risk",
         "Terraform plan can print secrets; mark variables sensitive.", "infra.state", "secrets,hazard"),
        ("intent", "Adopt OpenTofu", "Terraform fork",
         "We intend to evaluate OpenTofu to avoid licensing concerns.", "infra.iac", "opentofu,intent"),
        ("debug", "Pod evicted OOM", "Memory limits",
         "Pods were OOM-evicted; set requests=limits for the JVM services.", "infra.k8s", "debug,oom"),
        ("preference", "Prefer immutable images", "No in-place patch",
         "Prefer rebuilding immutable images over patching running pods.", "infra.deploy", "immutable,preference"),
        ("relationship", "Hosts all services", "Platform substrate",
         "devops-infra provisions clusters running web-api and friends.", "infra.k8s", "dependency"),
    ],
    "analytics-db": [
        ("decision", "ClickHouse warehouse", "OLAP store",
         "Analytics queries run on ClickHouse for sub-second aggregations.", "adb.core", "clickhouse,decision"),
        ("convention", "Tables are MergeTree", "Engine choice",
         "Fact tables use MergeTree partitioned by month, ordered by event time.", "adb.schema", "mergetree,convention"),
        ("hazard", "Mutations are expensive", "Avoid UPDATE",
         "ALTER UPDATE rewrites parts; model as append + ReplacingMergeTree.", "adb.schema", "mutation,hazard"),
        ("context", "Materialized views for rollups", "Pre-aggregation",
         "Hourly rollups are maintained by materialized views on insert.", "adb.rollup", "mv,context"),
        ("decision", "TTL-based retention", "Data lifecycle",
         "Raw events expire after 90 days via column TTL to cold storage.", "adb.retention", "ttl,decision"),
        ("hazard", "Cardinality blowup in GROUP BY", "Memory limit",
         "High-cardinality GROUP BY blew the memory limit; use sampling.", "adb.query", "cardinality,hazard"),
        ("intent", "Add tiered storage", "Hot/cold tiers",
         "We intend to move cold partitions to S3-backed disks.", "adb.retention", "tiering,intent"),
        ("debug", "Slow query from missing index", "Skip index",
         "A slow scan was fixed by adding a bloom_filter skip index.", "adb.query", "debug,index"),
        ("preference", "Prefer LowCardinality", "String optimization",
         "Use LowCardinality(String) for enum-like columns to shrink storage.", "adb.schema", "lowcardinality,preference"),
        ("relationship", "Fed by data-pipeline", "Upstream loader",
         "Curated tables are loaded nightly by the data-pipeline.", "adb.core", "dependency"),
    ],
}

# 10 cross-project (global) memories — broad org-wide conventions/hazards.
GLOBAL = [
    ("convention", "Conventional Commits", "Commit message format",
     "All repos use Conventional Commits; CI rejects non-conforming subjects.", "org.git", "git,convention"),
    ("decision", "Rust 2021 edition baseline", "Language baseline",
     "Backend services target Rust 2021 edition with MSRV 1.75.", "org.lang", "rust,decision"),
    ("hazard", "Never log secrets", "Secret hygiene",
     "Credentials, tokens, and PII must never be written to logs.", "org.security", "security,hazard"),
    ("convention", "Trunk-based development", "Branching model",
     "Teams use short-lived branches merged to main behind feature flags.", "org.git", "branching,convention"),
    ("decision", "Observability via OpenTelemetry", "Tracing standard",
     "All services emit OpenTelemetry traces to a central collector.", "org.observability", "otel,decision"),
    ("hazard", "Untrusted input is tainted", "Validate at the edge",
     "Treat all external input as tainted; validate and sanitize at the boundary.", "org.security", "validation,hazard"),
    ("preference", "Prefer SemVer for libraries", "Versioning policy",
     "Internal libraries follow SemVer; breaking changes bump major.", "org.release", "semver,preference"),
    ("convention", "12-factor config", "Config via env",
     "Configuration comes from environment variables, never committed files.", "org.config", "config,convention"),
    ("context", "On-call rotation weekly", "Incident process",
     "On-call rotates weekly; SEV1s page immediately via PagerDuty.", "org.oncall", "oncall,context"),
    ("intent", "Adopt SLSA provenance", "Supply chain",
     "We intend to publish SLSA build provenance for all release artifacts.", "org.security", "supplychain,intent"),
]


def run(args, **kw):
    r = subprocess.run([BIN] + args, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        sys.stderr.write(f"FAILED: {' '.join(args)}\n{r.stdout}\n{r.stderr}\n")
        raise SystemExit(1)
    return r


def add_memory(spec, project_dir=None, glob=False):
    typ, title, summary, content, logical, tags = spec
    args = ["--quiet"]
    if project_dir:
        args += ["--dir", str(project_dir)]
    args += ["add", "-t", typ, "-T", title, "-s", summary, "-c", content,
             "-l", logical, "--tags", tags]
    if glob:
        args += ["--global"]
    run(args)


# Dataset size is configurable so the same generator drives both the default
# (10x10) and a larger run. Extra memories are distinct variants of the curated
# seeds (numbered title/summary/content + a variant tag) so vector search still
# has real, non-duplicate signal; extra projects cycle the base themes.
N_PROJECTS = int(os.environ.get("BENCH_N_PROJECTS", "10"))
MEM_PER_PROJECT = int(os.environ.get("BENCH_MEM_PER_PROJECT", "10"))
N_GLOBAL = int(os.environ.get("BENCH_N_GLOBAL", "10"))


def expand(seeds, count):
    """Yield `count` specs by cycling `seeds`, making repeats distinct."""
    out = []
    for i in range(count):
        typ, title, summary, content, logical, tags = seeds[i % len(seeds)]
        rep = i // len(seeds)
        if rep:
            title = f"{title} v{rep + 1}"
            summary = f"{summary} (variant {rep + 1})"
            content = f"{content} (case {rep + 1})"
            tags = f"{tags},v{rep + 1}"
        out.append((typ, title, summary, content, logical, tags))
    return out


def project_specs():
    """Return [(slug, theme, seeds)] for N_PROJECTS, cycling base themes."""
    base = list(PROJECTS.keys())
    specs = []
    for i in range(N_PROJECTS):
        theme_slug = base[i % len(base)]
        rep = i // len(base)
        slug = theme_slug if rep == 0 else f"{theme_slug}-{rep + 1}"
        specs.append((slug, PROJECTS[theme_slug], SEEDS[theme_slug]))
    return specs


def main():
    t0 = time.time()
    ROOT.mkdir(parents=True, exist_ok=True)
    project_ids = {}
    projects_meta = {}
    for slug, theme, seeds in project_specs():
        pdir = ROOT / slug
        pdir.mkdir(exist_ok=True)
        run(["--quiet", "--dir", str(pdir), "init", "--no-embeddings"])
        for spec in expand(seeds, MEM_PER_PROJECT):
            add_memory(spec, project_dir=pdir)
        r = run(["--dir", str(pdir), "--json", "projects", "info"])
        try:
            pid = json.loads(r.stdout).get("project_id")
        except Exception:
            pid = None
        project_ids[slug] = pid
        projects_meta[slug] = {"path": str(pdir), "id": pid, "theme": theme}
        print(f"  {slug}: {MEM_PER_PROJECT} memories  (id={pid})")
    for spec in expand(GLOBAL, N_GLOBAL):
        add_memory(spec, glob=True)
    print(f"  global: {N_GLOBAL} memories")

    manifest = {
        "projects": projects_meta,
        "n_projects": N_PROJECTS,
        "memories_per_project": MEM_PER_PROJECT,
        "global_memories": N_GLOBAL,
    }
    (ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2))
    total = N_PROJECTS * MEM_PER_PROJECT + N_GLOBAL
    print(f"Generated {total} memories "
          f"({N_PROJECTS} projects x {MEM_PER_PROJECT} + {N_GLOBAL} global) "
          f"in {time.time()-t0:.1f}s -> {ROOT}")


if __name__ == "__main__":
    main()
