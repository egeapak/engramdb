# Hard cases for the memory-tool-use eval

Files: `cases_hard.jsonl` (47 cases; 45 in the climb, 2 costly cases added after it), `seed_memories_hard.json` (43 memories: the 10
original seeds unchanged, then 33 new ones in seeding order), and `fixture_hard/` (a copy
of `fixture/` plus the files listed at the end). The original eval is at its ceiling
because every fact sits in a short memory whose body the hooks inject whole, one memory
answers each case, and nothing in the repository disagrees with memory except where the
user says so. Each category below removes one of those crutches.

Hook behaviour these cases rely on was checked against the current release binary by
seeding the store and calling `engramdb hook pre-tool-use` / `user-prompt-submit` by hand:
body previews are cut at 160 characters (400 for the top entry); superseded memories are
not shown by the hooks or by `query`; and a memory scoped to a service directory
(`services/notifier/`, `services/reporting/`) is **not** injected for files two levels
below it (`services/notifier/notifier/email.py`, `services/reporting/reports/monthly.py`),
while `services/notifier/migrations/` does match its own files.

## Categories

**multi_memory (mm-, 6).** The task needs two or three memories at once, graded with
`facts_all` (every group must match). The hooks rank by file proximity, so a memory for
the endpoint's directory (`src/api/`: idempotency header, pagination, money shape) competes
with one for a different module (`src/billing/refunds.py`: approval threshold;
`src/jobs/`: `dedupe_key`) and with the 400/160-character preview budget. Today's build
may surface one memory, act on it, and never query for the rest; `mm-refund-question`
has no edit at all, and the prompt hook surfaces the approval rule but not the audit-event
convention.

**buried_fact (bf-, 6).** The deciding fact is 565 to 757 characters into a long body
whose title is deliberately generic ("Database connections in deployed environments", not
"pgbouncer"). The hook shows the first 160 to 400 characters and a "truncated; full text:
get the id above" marker. Because the hook already delivered the title and the start of
the body, `consulted` passes without any call; only `fact_used` shows whether Claude
followed the marker with `get`.

**stale_superseded (st-, 6).** Four pairs where a later memory supersedes an earlier one
with a different answer (CI rerun, FX provider, notifier owner, retention period), with
`forbidden` set to the stale value on the two code-editing cases. The product already
hides superseded memories, so these mostly test that the replacement still surfaces:
`retention_new` has no path (as a memory saved from a conversation often does), so the
file hook shows nothing and Claude must query. Two cases test a memory whose premise the
repository has made false: `pdf_render` (wkhtmltopdf while `invoice-pdf-v2` is off;
the fixture now has the flag on and a WeasyPrint renderer) and `flask_routes` (use
`@bp.route(methods=...)` because production pins Flask 1.1; `pyproject.toml` requires
flask>=3.0 and the blueprints use `@bp.get`). Both expect a challenge. Earlier results
show Claude follows memory or code without saying they disagree unless the user points it
out.

**distractor (ds-, 6).** Two near-identical memories with opposite answers, scoped to
different places: structlog everywhere vs. stdlib `logging` in the notifier; integer cents
in billing vs. Decimal with 4 places in reporting; never edit an applied migration vs.
edit notifier migrations in place before launch; `flags.is_on` vs. `NOTIFIER_FF_*` env
vars; `make test-fast` vs. the notifier's own pytest command; ROUND_HALF_EVEN for tax vs.
ROUND_FLOOR for FX. The wrong memory is the more general and often the better-scored one.
For the two deep notifier/reporting files the right memory is not injected at all (see
above), so Claude has to query instead of trusting the root memory it already knows.

**multi_turn (mt-, 7).** Turn 1 is ordinary work. Turn 2 either corrects a memory that a
hook showed in turn 1 (reviewer, webhook header, export format, which is a fact Claude may
only have seen truncated), states a durable fact in passing ("keep that in mind", "for
the record"), or asks something only memory answers after a turn of unrelated edits.
Turn 2 arrives with `--resume`, where Claude tends to answer from context already in the
session instead of going back to the store.

**discovered (dc-, 8).** A fact that one script warning reveals at once does not need
saving, so this category separates two kinds of case. Six are **costly** (tag `costly`,
`create: true`). In each, the cause is runtime state or a deep code path that no doc
names, so reading first does not reveal it, and the fix takes several calls after the
task command fails:
- `dc-backfill-tz`: the runner sets `TZ=Europe/Berlin` (the ops host). After the batch
  size is refused, `src/billing/periods.py` raises "period ... does not line up with the
  ledger period table". Backfills need `TZ=UTC`.
- `dc-fx-fixtures`: the test fails with `FileNotFoundError` for a per-day JSON file. The
  fixtures are generated by `scripts/gen_fx_fixtures.py`.
- `dc-seed-dev`: the runner leaves `var/dev.sqlite3` from an older checkout (migrations
  0001-0004). The database exists, so nothing says to create it; the seed fails with
  the real error swallowed. Run `scripts/migrate_dev.py` first.
- `dc-golden-statements`: the expected golden diff, then "golden integrity check failed"
  after the hand edit. Goldens are regenerated with `scripts/regen_goldens.py`.
- `dc-reconcile-format` (added after the climb): the runner leaves the ledger's export in
  `var/ledger/` in the provider's v1 column order. The script reads v2 by default and
  reports every invoice missing. Run it with `--format v1`.
- `dc-search-index-cache` (added after the climb): the runner leaves token shards from an
  older tokenizer in `var/cache/search/`. The build fails with "unexpected token". Run it
  with `--clear-cache`.

The save is graded only when finding the cause was costly: the **discovery cost** is the
number of tool calls from the first failed run of the task command (`attempt_re`) to its
first success, and it must be 3 or more. An attempt failed when the tool reported an
error or its output matches `fail_re`, so a chained `; ls` cannot hide a failure. Probes
before the first attempt cost nothing. The save is also graded only when the agent found
the fact, meaning a tool call used one of the `create_terms`. An agent that worked around
the failure without finding the cause has nothing to save. The row records
`discovery_cost` for every costly case.

Two cases are **cheap** (tag `cheap`, `create: null`, so saving is optional and not
graded). In `dc-ledger-smoke` the first run prints the fix. `dc-runbook-export` keeps its
gated `revise`: one file read shows that the runbook contradicts the `export` hazard.

Each costly case was checked end to end without Claude, on a fresh copy of
`fixture_hard/` prepared by `run.stale_dev_db` and with `TZ=Europe/Berlin`:

| case | 1st attempt | obvious fix | real fix |
|---|---|---|---|
| dc-fx-fixtures (convert() implemented) | `python -m pytest tests/test_currency.py` → `FileNotFoundError: …/fixtures/fx/2026-09-30.json` | - | `python scripts/gen_fx_fixtures.py` → `2 passed` |
| dc-backfill-tz | `python scripts/backfill_tax.py --month 2026-09` → exit 3 "batch size 2000 is above the safe limit" | `--batch-size 500` → `RuntimeError: period 2026-09 does not line up with the ledger period table` | `TZ=UTC … --batch-size 500` → exit 0 "would backfill 2026-09-01..2026-10-01" |
| dc-seed-dev | `python scripts/seed_dev.py` → exit 1 "refusing to run … Set LEDGERLINE_ENV=dev" | `LEDGERLINE_ENV=dev …` → exit 1 "seed failed: demo data did not load" | `python scripts/migrate_dev.py` (applied 3 migrations) then seed → "loaded 12 demo invoices and 3 refunds" |
| dc-golden-statements (separator added) | `python -m pytest tests/test_statements.py` → golden diff `1,234.56` vs `1234.56` | hand-edit the golden → `AssertionError: golden integrity check failed (statement_basic.txt)` | `python scripts/regen_goldens.py` → `1 passed` |

The runner generates the FX fixtures for every case except `dc-fx-fixtures`, as a
checkout where the generator ran once would have them. Without that, every task that runs
the tests (`ds-fx-rounding`, `mt-sandbox-region`) hit the same discovery, and the capture
round (v6) saved it there as a false create.

**negative_hard (nh-, 8).** Prompts that look like they should change memory but must
not: a one-off print() instead of structlog, a hypothetical rounding change, curiosity
about loguru, a session-only constraint, a user who guesses the rounding mode wrongly
without asking for a change, a remark about a previous job's migration practice, a
restatement of a convention that is already stored (a new memory would duplicate it),
and an undecided CI idea. All expect `create: false, revise: false`. The step-6 challenge
wording made Claude quick to challenge, so these check that it does not over-correct.

## Cases

| case id | category | target(s) | facts | verification |
|---|---|---|---|---|
| mm-refund-endpoint | multi_memory | idem_header, refund_limits, audit_events | all: `X-Acme-Idempotency`; all: `pending_approval`; all: `refund_issued`; (query=true create=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mm-dunning-job | multi_memory | dunning, jobs_queue | all: `1, 3, 7, 14` / `1,3,7,14`; all: `uncollectible`; all: `dedupe_key`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mm-webhook-failed | multi_memory | webhook_sig, jobs_queue | all: `PSP_WEBHOOK_SECRET`; all: `Psp-Timestamp`; all: `dedupe_key`; (query=true create=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mm-list-invoices | multi_memory | api_pagination, api_money | all: `next_cursor`; all: `page_size`; all: `ccy`; (query=true create=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mm-refund-question | multi_memory | refund_limits, audit_events | all: `pending_approval` / `second approver` / `50,000` / `50000` / `$500`; all: `refund_issued` / `actor_id`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mm-receipt-email | multi_memory | notifier_templates, jobs_queue, finalize | all: `.mjml`; all: `dedupe_key`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| bf-ledger-amount | buried_fact | bf_ledger_api | any of: `amount_minor`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| bf-export-csv | buried_fact | bf_export_csv | any of: `%d.%m.%Y`, `DD.MM.YYYY`; (query=true create=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| bf-tax-quebec | buried_fact | bf_tax_quebec | any of: `CA-QC`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| bf-db-pool | buried_fact | bf_db_pool | any of: `prepare_threshold`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| bf-release-freeze | buried_fact | bf_release_freeze | any of: `fin-eng`, `Dec 15`, `December 15`, `15 Dec`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| bf-notifier-sandbox | buried_fact | bf_notifier_sandbox | any of: `sink.acme.test`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| st-ci-rerun | stale_superseded | ci_new | any of: `bk build retry`, `buildkite`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| st-fx-provider | stale_superseded | fx_provider_new | any of: `ECB_FEED_URL`; forbidden: `OXR_APP_ID`, `openexchangerates`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| st-notifier-owner | stale_superseded | notifier_owner_new | any of: `growth-eng`, `Growth team`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| st-retention | stale_superseded | retention_new | any of: `3650`; forbidden: `2555`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| st-pdf-premise | stale_superseded | pdf_render | any of: `weasyprint`; (query=true create=false revise=true) | repo is the deliberate source: `weasyprint` (see note) |
| st-flask-premise | stale_superseded | flask_routes | any of: `bp.delete`; forbidden: `methods=[`; (query=true create=false revise=true) | `grep -rli` over fixture_hard/: 0 hits for every term |
| ds-notifier-log | distractor | notifier_logging | any of: `getLogger`; forbidden: `structlog`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| ds-reporting-money | distractor | reporting_money | any of: `0.0001`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| ds-notifier-migration | distractor | notifier_migrations | forbidden: `RENAME COLUMN`, `0008_`; (query=true create=false revise=false) | no fact check; graded on create/revise/forbidden |
| ds-notifier-flag | distractor | notifier_flags | any of: `NOTIFIER_FF_`; forbidden: `is_on(`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| ds-notifier-tests | distractor | notifier_tests | any of: `import-mode=importlib`; (query=true create=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| ds-fx-rounding | distractor | fx_rounding | any of: `ROUND_FLOOR`; forbidden: `ROUND_HALF_EVEN`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mt-reviewer | multi_turn | owner_ledger | any of: `Marco`; (revise=true) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mt-warnings-as-errors | multi_turn | - | create: `-W error`, `DeprecationWarning`; (create=true revise=false) | no fact check; graded on create/revise/forbidden |
| mt-drift-dunning | multi_turn | dunning | any of: `1, 3, 7, 14`, `1,3,7,14`, `uncollectible`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mt-webhook-spec | multi_turn | webhook_sig | all: `PSP_WEBHOOK_SECRET`; all: `Psp-Signature-Timestamp`; (revise=true) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mt-sandbox-region | multi_turn | - | create: `PSP_SANDBOX_REGION`; (create=true) | no fact check; graded on create/revise/forbidden |
| mt-refund-after-drift | multi_turn | idem_header, refund_limits | all: `X-Acme-Idempotency`; all: `pending_approval`; (query=true create=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| mt-export-format | multi_turn | bf_export_csv | (revise=true) | no fact check; graded on create/revise/forbidden |
| dc-backfill-tz | discovered | - | create: `TZ=UTC`, `TZ is UTC`, `TZ to UTC`, `TZ='UTC'`, `TZ="UTC"`; (create=true revise=false) | costly: cause is runtime state or a deep code path, not in the docs; verified end to end (table above) |
| dc-fx-fixtures | discovered | - | create: `gen_fx_fixtures`; (create=true revise=false) | costly: cause is runtime state or a deep code path, not in the docs; verified end to end (table above) |
| dc-runbook-export | discovered | export | any of: `ledgerline-exports-staging`; (revise=true) | repo is the deliberate source: `ledgerline-exports-staging` (see note) |
| dc-ledger-smoke | discovered | - | (revise=false) | no fact check; graded on create/revise/forbidden |
| dc-seed-dev | discovered | - | create: `migrate_dev`; (create=true revise=false) | costly: cause is runtime state or a deep code path, not in the docs; verified end to end (table above) |
| dc-golden-statements | discovered | - | create: `regen_goldens`; (create=true revise=false) | costly: cause is runtime state or a deep code path, not in the docs; verified end to end (table above) |
| nh-print-oneoff | negative_hard | - | (create=false revise=false) | no fact check; graded on create/revise/forbidden |
| nh-hypothetical-rounding | negative_hard | - | (create=false revise=false) | no fact check; graded on create/revise/forbidden |
| nh-curious-loguru | negative_hard | - | (create=false revise=false) | no fact check; graded on create/revise/forbidden |
| nh-offline-today | negative_hard | - | (create=false revise=false) | no fact check; graded on create/revise/forbidden |
| nh-user-unsure-rounding | negative_hard | rounding | any of: `HALF_EVEN`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| nh-other-project | negative_hard | migrations | any of: `0008`; (query=true create=false revise=false) | `grep -rli` over fixture_hard/: 0 hits for every term |
| nh-already-known | negative_hard | - | (create=false revise=false) | no fact check; graded on create/revise/forbidden |
| nh-ci-idea | negative_hard | - | (create=false revise=false) | no fact check; graded on create/revise/forbidden |

`(query=… create=… revise=…)` gives the graded expectations (absent = not graded). The
absence check ran every `facts`/`facts_all` term, case-insensitively, against every
file under `fixture_hard/`, and it was repeated with `grep -rliF`. The two terms that do
occur in the repository are there on purpose: `weasyprint` (`src/billing/pdf.py`, which is
what makes `pdf_render`'s premise false) and `ledgerline-exports-staging`
(`docs/runbook.md`, which is what contradicts `export`). `forbidden` terms are checked
only against the diff, so they are used only on cases that edit code. None of them occur
in the case's own facts.

## Files added to fixture_hard/

- `src/api/webhooks.py`: a PSP webhook stub with no verification.
- `src/billing/currency.py`: `fetch_rates` and `convert` stubs.
- `src/billing/dunning.py`: a `schedule_retries` stub.
- `src/billing/pdf.py`: the WeasyPrint renderer.
- `src/jobs/queue.py`: `enqueue(name, payload, **options)`.
- `services/notifier/`: a second service with its own code, migrations (including the
  `recipent` typo), tests and pyproject.
- `services/reporting/`: a third service with a `load_rows` stub.
- `docs/runbook.md`: the ops runbook.
- Scripts, all stdlib-only. `scripts/backfill_tax.py` needs a batch size of 500 or less
  and `TZ=UTC` (via `src/billing/periods.py`). `scripts/seed_dev.py` and `scripts/migrate_dev.py` work on
  `var/dev.sqlite3`. `scripts/ledger_smoke.py` prints how to get a sandbox token.
  `scripts/gen_fx_fixtures.py` and `scripts/regen_goldens.py` generate test data.
- Docs: `docs/testing.md`, `docs/dev-setup.md` and `docs/ops/backfills.md`. They do
  not name the causes behind the costly discovered cases.
- `src/billing/statements.py`: the statement renderer.
- Tests: `tests/test_currency.py` with `tests/fxdata.py` and the snapshot
  `tests/fixtures/fx/usd-2026-09.csv`; `tests/test_statements.py` with `tests/goldens.py`
  and `tests/golden/` (an input, a golden file and the MANIFEST). All are unittest-based.
- `.gitignore`: ignores `var/` and the generated FX JSON files.

Changed files:

- `flags.toml`: `invoice-pdf-v2 = true`.
- `pyproject.toml`: `testpaths = ["tests"]`, so the root suite does not collect the
  notifier's tests.
- `README.md`: the services and the docs are listed.

Seeds use the optional `logical`, `criticality`, `supersedes`, `premise` and
`invalidated_by` fields. All 43 seed with the current `engramdb add` when those fields are
passed as `--logical`, `--criticality`, `--supersedes <id>`, `--premise` and
`--invalidated-by`.
