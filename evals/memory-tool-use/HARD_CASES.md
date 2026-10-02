# Hard cases for the memory-tool-use eval

Files: `cases_hard.jsonl` (45 cases), `seed_memories_hard.json` (43 memories: the 10
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

**discovered (dc-, 6).** The durable fact appears only in tool output during the task: a
script that refuses its default batch size (INC-2291), a test that errors without
`LEDGERLINE_FX_FIXTURES`, a smoke script that explains how to mint a sandbox token, a
seed script that says it truncates every table, a test that requires `# owner:` comments
in flags.toml, and a runbook that contradicts the `export` hazard. The first five expect
an implicit `create`, the last a `revise`. Nobody asks Claude to remember anything, and
some of the facts are also written in the script source, so whether to save is Claude's
own call. Implicit capture is reported but not part of `pass`. `dc-runbook-export` is
gated.

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
| dc-backfill-warning | discovered | - | create: `INC-2291`, `batch-size`, `batch size`; (create=true revise=false) | no fact check; graded on create/revise/forbidden |
| dc-fx-fixtures | discovered | - | create: `LEDGERLINE_FX_FIXTURES`; (create=true revise=false) | no fact check; graded on create/revise/forbidden |
| dc-runbook-export | discovered | export | any of: `ledgerline-exports-staging`; (revise=true) | repo is the deliberate source: `ledgerline-exports-staging` (see note) |
| dc-ledger-smoke | discovered | - | create: `acmectl`, `LEDGER_SANDBOX_TOKEN`; (create=true revise=false) | no fact check; graded on create/revise/forbidden |
| dc-seed-dev | discovered | - | create: `TRUNCATE`, `LEDGERLINE_ENV`; (create=true revise=false) | no fact check; graded on create/revise/forbidden |
| dc-flag-owner | discovered | - | create: `owner`; (create=true) | no fact check; graded on create/revise/forbidden |
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
- `scripts/backfill_tax.py`, `scripts/seed_dev.py`, `scripts/ledger_smoke.py`: run with the
  stdlib only, and each prints its message.
- `tests/test_currency.py`, `tests/test_flags.py` and `tests/fixtures/fx/`: unittest-based,
  so `python -m unittest` works without pytest.

Changed files:

- `flags.toml`: `invoice-pdf-v2 = true`, plus an `# owner:` comment.
- `pyproject.toml`: `testpaths = ["tests"]`, so the root suite does not collect the
  notifier's tests.
- `README.md`: the services and the runbook are listed.

Seeds use the optional `logical`, `criticality`, `supersedes`, `premise` and
`invalidated_by` fields. All 43 seed with the current `engramdb add` when those fields are
passed as `--logical`, `--criticality`, `--supersedes <id>`, `--premise` and
`--invalidated-by`.
