# Extra-hard cases for the memory-tool-use eval

Files: `cases_xhard.jsonl` (40 cases), `seed_memories_xhard.json` (114 memories: the 43
hard seeds unchanged, then 71 new ones), `fixture_xhard/` (a copy of `fixture_hard/`
grown by three modules), and `split_xhard.json` (20 train / 20 test, stratified by
`tags[0]`, seed 20261005). Run with `python3 run.py --fixture xhard ...`; `regrade.py`
knows the fixture too.

## Why a third set

The hard set saturated at 96-99% on both models. Its cases are hard for one reason
each, and with 43 memories the right one usually is the only memory on its topic, or one
of two. This set is meant to be hard for the **memory behaviour**, not for the coding:
every task is a stub of a few lines. What it makes hard:

- **Crowding.** The store has 7 rounding rules, 7 retry policies, 7 ownership memories,
  6 idempotency/dedupe rules, 6 time-zone rules, 6 size limits, 7 money representations,
  each scoped to a different module. The file hook shows 5 memories. On
  `services/ingest/` files it shows `ingest_precision, ingest_retry, ingest_dedupe,
  ingest_owner, ingest_tests` and never the other four ingest memories (logging, flags,
  chunk size, row statuses); on `services/notifier/` files it never shows
  `notifier_retry` or `notifier_entry`. The prompt hook on "we always round down toward
  negative infinity" returns only `sql_params`.
- **The rule lives elsewhere.** Cross-module tasks edit module A, and the deciding memory
  is scoped to module B or C that the file imports or calls. No hook on A shows it.
- **Time and compaction.** Long cases run 4 turns, most with a `/compact` turn, and need
  a fact after it that no hook shows in the last turn's files.
- **Three-step chains** where the current memory has no path, so only `query` finds it,
  and the repo still holds the middle (superseded) value.
- **One contradicted memory among look-alikes**, and **negatives** that look like a new
  fact or a contradiction in a store where the fact already exists under other words.

## /compact as a turn

Probed with `claude -p --resume <id> "/compact"` (Claude Code 2.1.289, Haiku 4.5, a
scratch dir): it works. The turn emits `system/status compacting`, a
`system/compact_boundary` event (`trigger: manual`, 29k -> 2k tokens) and a normal
`result` event (`num_turns: 0`, about $0.02), so `run.py` needs no change: a `"/compact"`
entry in `turns` is just another follow-up turn. A later `--resume` turn continues from
the summary. The plugin's PreCompact hook clears the per-session "already injected" list,
so after compaction the file hook re-injects memories in full when a scoped file is
touched; the long cases put their last turn where no hook shows the fact (`scripts/`,
another service) or test that re-injection itself (`lc-corrected-notice`).

## Fixture (fixture_xhard/)

Copied from `fixture_hard/`, then:

- `src/payouts/` (new): `fees.py` (`platform_fee` stub), `schedule.py` (`payout_date`
  stub; `SAME_DAY_CUTOFF = time(14, 0)`, the superseded v2 cutoff), `bankco.py` (BankCo
  client stubs; `BASE_URL` ends in `/v1`, the superseded API; `MAX_TRANSFERS_PER_FILE =
  250` with a dated comment that contradicts `payout_batch_limit`), `batch.py`
  (`Payout`, `eligible_balance`, `should_pay_out`, `submit_batch`, `mark_paid` stubs).
- `src/subscriptions/` (new): `plans.py`, `proration.py` (`prorate_amount` stub, and a
  working `prorated_tax` that uses `ROUND_HALF_EVEN`, the tax rule, which looks like a
  contradiction of the proration rule and is not), `renewals.py` (`Subscription` with
  `customer_tz`; six stubs; `RENEWAL_NOTICE_DAYS = 7`).
- `services/ingest/` (new, separately deployed): `ingest/parse.py`, `loader.py`,
  `source.py`, `client.py`, `ingest.toml` (`[source]` bucket `psp-settlements-eu`, the
  superseded v2 bucket), `tests/test_parse.py`.
- `src/billing/psp.py` (new; `PSP_VERSION = "2026-03-01"`, the superseded v2 value),
  `src/api/internal.py` (new; `POST /internal/settlements`).
- `.github/CODEOWNERS` (new): agrees with every ownership memory except `subs_owner`.
- Changed: `src/billing/invoices.py` (`create_invoice(..., origin=None)`, `Invoice.origin`,
  `Invoice.finalized_at`), `src/billing/refunds.py` (`REFUND_WINDOW_DAYS = 180`, v2's
  value), `services/reporting/reports/monthly.py` (a docstring that contradicts
  `reporting_tz`; `load_payout_rows`, `month_bounds` stubs),
  `services/notifier/notifier/provider.py` (a dated comment that contradicts
  `notifier_retry`), `README.md` (new modules, CODEOWNERS pointer).
- The generated FX JSON fixtures are committed and `.gitignore` no longer ignores them,
  so the root tests run without `runtime_state()`. The xhard set has no costly
  (`attempt_re`) cases, and `run.py` does not create `var/` for it.

Everything is stdlib-only except the existing Flask/requests/structlog imports. The ingest
tests run with `python -m unittest discover -s tests -t .` from `services/ingest` (2
tests, OK). Every `.py` file compiles.

## Seeds

The 71 new memories are listed in `seed_memories_xhard.json` after the hard ones. Six
3-step chains (`*_v1 -> *_v2 -> *_v3`, each `supersedes` the previous): payout cutoff
16:00 -> 14:00 -> 15:30 Amsterdam; refund window 90 -> 180 -> 395 days; settlement
source SFTP -> `psp-settlements-eu` -> `acme-psp-inbound`; notifier sender `billing@` ->
`noreply@` -> `receipts@mail.acme.example`; BankCo API SFTP -> REST v1 with key -> v2
OAuth; PSP version path -> `2026-03-01` -> `2026-09-15`. v1 and v2 carry the file's path;
v3 has none. Four new memories are `"stale": true` because the fixture contradicts them on
purpose (`payout_batch_limit`, `notifier_retry`, `subs_owner`, `reporting_tz`); with the
superseded v1/v2 seeds, revising them is never counted as spurious.

Seeding 114 memories with `engramdb add` takes about 165 s per case (1.2-1.5 s each, the
embedding model loads per call), against about 50 s for the hard set. Budget for it: a
full 40 x 3 run at concurrency 3 spends about 2 h in setup alone.

## Categories

**crowded (cr-, 10).** The target competes with 5-7 memories on the same topic; facts are
the target's value and `forbidden` lists the neighbours' values (rounding keyword forms,
other queues' delays, `dedupe_key`, structlog/logging, `Europe/Amsterdam`). Five of them
are on files where the hook does show the target among its 5 (proration, payout fee,
renewal retry/time zone, ingest amount/retry/dedupe), so they test whether Claude picks the
right one of several shown; `cr-ingest-logging`, `cr-ingest-tests` and `cr-payouts-owner`
need a query that discriminates.

**cross_module (xm-, 8).** `facts_all` across 2 memories from different modules: the
edited file imports or calls the module the rule is scoped to (`create_invoice`'s origin
value, the job queue's `dedupe_key`, audit event names from `src/billing/`, the
notifier's job entry point and template format, the internal API's caller header plus the
idempotency header, payout row columns plus reporting precision, the BankCo request-id
hazard, settlement row statuses plus the reserve).

**long_session (lc-, 6).** Multi-turn via `turns`; five include `"/compact"`.
`lc-reserve-after-compact` and `lc-buried-remittance` surface a memory through turn 1's
file hook, use only part of it, then need the rest in `scripts/` after compaction.
`lc-ingest-chunks` needs an ingest memory the hook never shows. `lc-corrected-notice`
corrects a memory in turn 1 and implements against it after compaction, while the repo
constant still says 7. `lc-sandbox-iban` states a fact (graded create) and needs it in a
test after compaction; without it Claude writes a textbook IBAN (forbidden).
`lc-review-sweep` has four turns without compaction and ends with a question only memory
answers. Facts in long cases are code forms or turn-4-only values, because the grader
joins every turn's answer and an echo in turn 1 would satisfy them.

**superseded_chain (st-, 6).** One per chain. The repo holds v2's value and v3 says so;
the hook shows neither v1 nor v2 (checked) and cannot show v3 (no path). An agent that
does not query uses the repo's value, and the fact check fails.

**contradiction (ct-, 4).** A dated repo source (a code constant, a code comment,
CODEOWNERS, a module docstring) contradicts exactly one memory of a cluster. Pass needs a
revise of that memory. **Not graded:** that the cluster's other memories were left alone.
`grade.py` computes `no_spurious_revise` only when `revise` is false, so a collateral
challenge does not fail these cases; read it from `meta.gaps.memory_calls`, or extend the
grader to check `revise_calls` against the other seeded ids for `contradiction` cases.

**negative_hard (nh-, 6).** `create: false, revise: false`: a fact restated in other words
(`fx_rounding`; the no-path `cutoff_v3`), code that looks like a contradiction and is not
(`prorated_tax`), a session-only constraint, a hypothetical about a hazard, and a user's
wrong guess with a task attached.

## How each case was checked

- `check_xhard.py` (no API calls; prints every exception for review): for every
  `facts`/`facts_all` term, every seed whose title or body contains it
  (case-insensitive) must be one of the case's targets, and no fixture file may contain
  it. Exceptions, all reviewed: derived terms that no memory holds verbatim
  (`ledger.payout_` from the `ledger.<noun>_<verb>` rule; `'cleared'` and `0.075` as other
  spellings; `(6, 0`, `hour=6`, `15, 30` as code forms); `06:00` also occurs in
  `bf_export_csv` (cron time, not surfaced on these files); `35` and `1000` occur only in
  FX data files and `seed_dev.py`/`test_currency.py`, which these tasks do not edit, and
  the grader reads only answers and the diff; `lc-corrected-notice`'s value comes from the
  user. `forbidden` terms are absent from the target memories, except the old value of
  the memory `lc-corrected-notice` corrects.
- Every file a prompt names exists (or the prompt asks for a new one). All fixture Python
  compiles; the ingest tests pass with the command `ingest_tests` stores.
- Hooks and queries were probed on a workspace seeded by `run.setup_workspace` with the
  `target/eval` binary (`engramdb hook pre-tool-use` / `user-prompt-submit`, `engramdb
  query --mode filter`). Results quoted above; superseded v1/v2 never appear in a file
  hook; every target is returned by a plain keyword query (`"insert chunk"` ->
  `ingest_chunk`, `"reserve"` -> `payout_reserve`, `"sender address"` -> `sender_v3`, ...).
- Harness smoke run: see the end of this file.

## Cases

| case id | category | target(s) | graded |
|---|---|---|---|
| cr-payout-fee | crowded | payout_fee_rounding | any of: `ROUND_CEILING`; forbidden: `rounding=ROUND_HALF_EVEN`, `rounding = ROUND_HALF_EVEN`, `rounding=decimal.ROUND_HALF_EVEN`, `rounding=ROUND_FLOOR` …; (query=true create=false revise=false) |
| cr-proration | crowded | proration_rounding | any of: `ROUND_HALF_DOWN`; forbidden: `rounding=ROUND_HALF_EVEN`, `rounding = ROUND_HALF_EVEN`, `rounding=decimal.ROUND_HALF_EVEN`, `rounding=ROUND_FLOOR` …; (query=true create=false revise=false) |
| cr-ingest-amount | crowded | ingest_precision | any of: `SettlementPrecisionError`; forbidden: `ROUND_`, `quantize(`; (query=true create=false revise=false) |
| cr-renewal-retry | crowded | renewal_retry | all: `21600`; all: `past_due`; forbidden: `backoff="exp"`, `backoff='exp'`, `max_attempts=5`, `1, 3, 7, 14`; (query=true create=false revise=false) |
| cr-ingest-retry | crowded | ingest_retry | all: `900`; all: `quarantine`; forbidden: `3600`, `21600`, `2 **`, `2**` …; (query=true create=false revise=false) |
| cr-ingest-dedupe | crowded | ingest_dedupe | any of: `content_sha256`; forbidden: `dedupe_key`; (query=true create=false revise=false) |
| cr-payouts-owner | crowded | payouts_owner | any of: `Duarte`; (query=true create=false revise=false) |
| cr-ingest-tests | crowded | ingest_tests | any of: `unittest discover`; (query=true create=false) |
| cr-ingest-logging | crowded | ingest_logging | any of: `"evt"`, `'evt'`; forbidden: `structlog`, `getLogger`, `import logging`; (query=true create=false revise=false) |
| cr-renewal-tz | crowded | renewal_tz | all: `ZoneInfo` / `zoneinfo`; all: `06:00` / `hour=6` / `time(6` / `(6, 0`; forbidden: `Europe/Amsterdam`; (query=true create=false revise=false) |
| xm-renewal-invoice | cross_module | invoice_origin, jobs_queue | all: `RENEWAL_AUTO`; all: `dedupe_key`; (query=true create=false revise=false) |
| xm-payout-paid | cross_module | audit_events, notifier_entry | all: `ledger.payout_`; all: `actor_id`; all: `notifier.send_email`; (query=true create=false revise=false) |
| xm-ingest-client | cross_module | internal_api, idem_header | all: `X-Acme-Caller`; all: `X-Acme-Idempotency`; (query=true create=false revise=false) |
| xm-reporting-payouts | cross_module | payout_rows, reporting_money | all: `fee_minor`; all: `0.0001`; (query=true create=false revise=false) |
| xm-renewal-notice-email | cross_module | notifier_entry, notifier_templates | all: `notifier.send_email`; all: `.mjml`; (query=true create=false revise=false) |
| xm-payout-retry-endpoint | cross_module | bankco_transfers, idem_header | all: `bank_request_id`; all: `X-Acme-Idempotency`; (query=true create=false revise=false) |
| xm-cancel-refund | cross_module | refund_limits, proration_rounding | all: `pending_approval`; all: `ROUND_HALF_DOWN`; (query=true create=false revise=false) |
| xm-payout-eligible | cross_module | ingest_status, payout_reserve | all: `"cleared"` / `'cleared'`; all: `750` / `0.075` / `7.5%`; (query=true create=false revise=false) |
| lc-reserve-after-compact | long_session +3 turns | payout_reserve | any of: `750`, `0.075`, `7.5%`; (query=true create=false revise=false) |
| lc-corrected-notice | long_session +3 turns | renewal_notice | any of: `RENEWAL_NOTICE_DAYS = 14`, `RENEWAL_NOTICE_DAYS=14`, `days=14`, `timedelta(14`; forbidden: `days=7`, `DAYS = 7`, `DAYS=7`; (revise=true) |
| lc-buried-remittance | long_session +3 turns | bankco_transfers | any of: `35`; (query=true create=false revise=false) |
| lc-ingest-chunks | long_session +3 turns | ingest_chunk | any of: `1000`, `1_000`, `1,000`; forbidden: `= 500`, `=500`; (query=true create=false revise=false) |
| lc-review-sweep | long_session +3 turns | payouts_owner | any of: `Duarte`; (query=true create=false) |
| lc-sandbox-iban | long_session +3 turns | - | forbidden: `DE89`, `GB82`, `GB29`, `FR14` …; create: `NL91`; (create=true revise=false) |
| st-payout-cutoff | superseded_chain | cutoff_v3 | any of: `15:30`, `15, 30`; forbidden: `time(14, 0)`, `time(14,0)`, `time(16, 0)`, `time(16,0)`; (query=true create=false revise=false) |
| st-refund-window | superseded_chain | refund_window_v3 | any of: `395`; forbidden: `= 180`, `=180`, `days=90`, `= 90`; (query=true create=false revise=false) |
| st-ingest-source | superseded_chain | ingest_source_v3 | any of: `acme-psp-inbound`; forbidden: `sftp`; (query=true create=false revise=false) |
| st-notifier-sender | superseded_chain | sender_v3 | any of: `mail.acme.example`; forbidden: `noreply@acme.example`, `billing@acme.example`; (query=true create=false revise=false) |
| st-bankco-api | superseded_chain | bankco_api_v3 | any of: `BANKCO_CLIENT_ID`, `/v2/payments`; forbidden: `X-BankCo-Key`, `BANKCO_API_KEY`, `/v1/transfers`; (query=true create=false revise=false) |
| st-psp-version | superseded_chain | psp_version_v3 | any of: `2026-09-15`; forbidden: `/v2/`; (query=true create=false revise=false) |
| ct-batch-limit | contradiction | payout_batch_limit | (create=false revise=true) |
| ct-notifier-retry | contradiction | notifier_retry | (create=false revise=true) |
| ct-subs-owner | contradiction | subs_owner | (create=false revise=true) |
| ct-reporting-tz | contradiction | reporting_tz | (create=false revise=true) |
| nh-fx-restated | negative_hard | - | (create=false revise=false) |
| nh-cutoff-restated | negative_hard | - | (create=false revise=false) |
| nh-prorated-tax | negative_hard | - | (create=false revise=false) |
| nh-bankco-down | negative_hard | - | (create=false revise=false) |
| nh-payout-retry-hypothetical | negative_hard | - | (query=true create=false revise=false) |
| nh-min-payout-unsure | negative_hard | payout_min | any of: `2500`; (query=true create=false revise=false) |


## Which hooks surface the targets (probe, before any run)

Probed on a freshly seeded workspace (`target/eval` build `3983b19c`), calling `engramdb
hook user-prompt-submit` with each turn's prompt and `pre-tool-use` (Edit) on each file a
prompt names, each with a new session id. `*` marks a target the prompt hook returned.

- **High-criticality memories take prompt-hook slots everywhere.** `bankco_transfers`
  (0.9), `kyc_gate`, `payouts_pii`, `webhook_sig`, `bf_db_pool` appear in most of the 40
  prompt-hook results, whatever the topic. In a real session SessionStart injects them
  first and the seen-list then lets lower-ranked memories into the prompt hook: in the
  smoke run, `cr-ingest-logging`'s prompt hook did return `ingest_logging`, which the
  session-less probe did not. So the probe below *understates* what the hooks deliver.
- **The prompt hook finds most no-path v3 memories** when the prompt names the function
  (`st-payout-cutoff`, `st-refund-window`, `st-bankco-api`, `st-psp-version`): the
  "only `query` finds it" premise holds for the file hook, not for the prompt hook. It
  does not find `ingest_source_v3` (`st-ingest-source`) or `sender_v3`
  (`st-notifier-sender`).
- **No hook surfaced a target** (query, or reading the other module, required):
  `cr-ingest-logging` (see above), `xm-payout-paid` (both), `xm-ingest-client` (unless
  `src/api/internal.py` is opened), `xm-reporting-payouts` (`payout_rows`),
  `xm-renewal-notice-email` (`notifier_templates`), `xm-cancel-refund`
  (`proration_rounding`, unless proration.py is opened), `lc-review-sweep` (turn 4),
  `st-ingest-source`, `st-notifier-sender`, `nh-fx-restated` (the duplicate check).
- **The file hook shows the target** on its own file for 9 of 10 crowded cases (not
  `cr-ingest-logging`, `cr-payouts-owner` on batch.py); there the test is picking the
  right one of five shown and not acting on a neighbour.

## Smoke run

`run.py --fixture xhard --engram-md --model claude-sonnet-5-5 --variant smoke --reps 1`
on `cr-ingest-logging`, `st-payout-cutoff`, `lc-corrected-notice`
(`.claude/hillclimb/memory-tool-use-xhard/smoke/smoke/`): 3 graded rows, 0 harness
errors, 3/3 pass, $0.85 in total. The `/compact` turn ran (one `compact_boundary` event)
and the next `--resume` turn continued. In `lc-corrected-notice` Sonnet called `get`,
`update` (content now says 14 days) and `verify` in turn 1 and wrote
`RENEWAL_NOTICE_DAYS = 14` in turn 4 (19 turns, $0.69). The two single-turn cases passed
with no memory call at all: the prompt hook delivered `ingest_logging` and `cutoff_v3`.
