# embed_eval.json — dataset notes

Evaluation dataset for embedding-retrieval experiments in EngramDB. Two fictional projects mixed:
**orderflow** (Rust: axum, sqlx/Postgres, Redis, Kafka/MSK, JWT auth, Stripe, k8s) and
**dashly** (TypeScript: React, Vite, tRPC, Playwright, LaunchDarkly flags, Segment/BigQuery analytics).

## Counts

- Memories: 60 — short (<60 words): 24, medium (~100-180 words): 19, long (350-500 words): 17
- Queries: 48
  - keyword: 6 (q01-q06)
  - natural: 6 (q07-q12)
  - title_echo: 6 (q13-q18)
  - tag_only: 6 (q19-q24)
  - buried_fact: 12 (q25-q30, q43-q48)
  - paraphrase: 6 (q31-q36)
  - distractor_trap: 6 (q37-q42)
- Every query has >= 1 grade-2 memory; all qrels ids verified to exist.
- 43/60 memories are relevant to at least one query; the rest are realistic corpus noise.

## Planted facts in long memories (`fact_pos` field)

Chunker assumption: 192-word blocks. Buckets: `start` = fact within first 50 words;
`straddle` = fact's sentence crosses the word-192 boundary (verified programmatically);
`end` = fact within final 50 words. 8 facts also sit past word 250 (original buried-fact
requirement): m07, m12, m18, m26, m38, m49, m54, m60.

| id  | fact_pos | fact word/total | sentence span | planted fact | query |
|-----|----------|-----------------|---------------|--------------|-------|
| m15 | start    | 37/423  | 25-56   | SQLSTATE 40P01 deadlock code | q27 |
| m27 | start    | 14/445  | 0-37    | OF_ORDER_QUEUE_DEPTH_ALARM=5000 | — |
| m56 | start    | 11/366  | 0-45    | SaslAuthenticationException: Access denied | q43 |
| m57 | start    | 22/356  | 0-39    | VITE_DEV_PROXY_TARGET (default http://localhost:8787) | q44 |
| m09 | straddle | 199/356 | 185-222 | OF_CONSUMER_MAX_POLL_RECORDS=50 | q26 |
| m32 | straddle | 192/374 | 172-215 | STORAGE_STATE_TTL_MIN=90 | q28 |
| m45 | straddle | 196/389 | 186-223 | /t/ingest first-party proxy path | — |
| m58 | straddle | 197/368 | 176-219 | OTEL_EXPORTER_OTLP_ENDPOINT=...svc.cluster.local:4317 | q45 |
| m59 | straddle | 186/371 | 165-219 | ANALYTICS_BACKFILL_BATCH=20000 | q46 |
| m07 | end      | 394/409 | 363-409 | client-output-buffer-limit pubsub 64mb 16mb 60 | — |
| m12 | end      | 395/413 | 361-413 | OF_STRIPE_SANDBOX_DISABLED=1 | q25 |
| m18 | end      | 382/414 | 368-414 | memory limit 512Mi -> 768Mi (plus MALLOC_ARENA_MAX=2) | q29 |
| m26 | end      | 403/429 | 387-429 | OF_OUTBOX_POLL_MS=200, batches of 500 | — |
| m38 | end      | 342/372 | 316-352 | NODE_OPTIONS=--max-old-space-size=6144 | q30 |
| m49 | end      | 327/372 | 312-355 | dash-checkout-v2-kill (plus LD_SDK_TIMEOUT_MS=1500) | q48 |
| m54 | end      | 353/380 | 340-380 | build.target es2019 in vite.config.ts | — |
| m60 | end      | 311/358 | 304-358 | CARGO_INCREMENTAL=0 | q47 |

Bucket totals: start 4, straddle 5, end 8. Straddle facts sit at words 186-199 with their
sentences spanning the 192 boundary, so a 192-word chunker bisects the fact's sentence.

## Code-dense stratum (`code_dense: true`, 15 memories)

Heavy in identifiers, env vars, file paths, CLI flags, error strings (wordpiece-explosion probes):
- Long (9): m09, m12, m15, m38, m56, m57, m58, m59, m60
- Short/medium (6): m04, m05, m20, m30, m33, m41

9 buried_fact queries target facts inside code-dense long memories:
q25, q26, q27, q30, q43, q44, q45, q46, q47 (requirement was >= 4).

## Near-duplicate distractor pairs (6)

| pair | A | B | trap queries |
|------|---|---|--------------|
| 1 | m02 access-token TTL decision | m03 refresh-rotation reuse hazard | q37 -> m02 (m03=1); q01 -> m03 (m02=1) |
| 2 | m06 Redis eviction config | m07 Redis pubsub outage postmortem | q38 -> m06 (m07 omitted) |
| 3 | m10 partition count decision | m09 consumer-lag postmortem | q39 -> m10 (m09=1); q02 -> m09 (m10=1) |
| 4 | m11 Stripe webhook signature hazard | m12 payment idempotency design | q40 -> m11 (m12 omitted) |
| 5 | m31 Playwright-over-Cypress decision | m32 flaky checkout spec hazard | q41 -> m31 (m32 omitted); q13 -> m32 (m31=1) |
| 6 | m34 flag rollout ladder | m35 stale flag removal checklist | q42 -> m35 (m34=1); q10 -> m34 (m35=1) |

## Tag/title-only signal (11 memories)

Critical term lives ONLY in tags or title, not in content/summary:
- m06: "redis" only in tags (content says "the cache tier")
- m14: "NOT NULL" only in title; "access-exclusive-lock"/"ddl" only in tags (content: "required column", "heavyweight table lock", "schema change") — enables q15 (title_echo) and q24 (tag_only)
- m20: "structured-logging" only in tags
- m21: "thundering-herd"/"singleflight" only in tags (content: "dogpile", "request coalescing"); "stampede" only in title — enables q17, q19
- m25: "jwt"/"leeway" only in tags (content: "signed auth tokens", "tolerance") — enables q23
- m32: "Playwright"/"retry"/"flakiness" only in title/tags (content: "e2e suite", "re-runs", "least trusted") — enables q13
- m37: "segment"/"tracking-plan" only in tags (content: "analytics vendor SDK", "event catalog sheet") — enables q22
- m41: "dotenv"/"import-meta-env" only in tags
- m47: "cloudflare"/"414" only in tags (content: "our CDN", "rejected at the edge") — enables q21
- m50: "xss"/"localstorage" only in tags (content: "injected scripts", "browser storage") — enables q20
- m51: "StrictMode" only in title (content: "mounts, unmounts, and remounts") — enables q14

## Known benign lexical overlaps (validator warnings, accepted)

- title_echo queries share only generic words with target content (q14 "react/double/effects", q15 "postgres/not", q16 "checkout/pods", q17 "cache"); the discriminating title terms (strictmode, "not null", oomkilled, stampede) never appear in content or summary.
- q33 paraphrase shares only "old" with m54's content ("older-but-supported"); all topical words differ.

## Validation

Generated and checked by `queries_gen.py` (same directory): JSON round-trip parse, unique
sequential ids, qrels referential integrity, grade-2 present per query, >= 5 queries per
archetype, long-memory word counts 350-500, summary <= 100 chars, fact-position buckets
(including sentence-straddles-192 verification), and no markdown in content. Final run: ALL CHECKS PASSED.
