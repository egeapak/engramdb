# Memory tool use eval: cases for review

Flow: Claude Code with the `engram` plugin (MCP server + hooks) in a small fixture Python billing project. Each case runs in a fresh copy of the fixture with the 10 memories below seeded into its store.

## Labeling policy

- **query**: `yes` = Claude must consult memory (an EngramDB `query`/`get`/`list` call) before it answers, or before its first edit. `any` = not graded. A query is never a failure: Claude cannot know a check is unneeded without checking.
- **create**: `yes` = the store must hold a new memory after the run that contains one of the *create terms*. `no` = no new memory. Checked on the store files (end state), not on the transcript.
- **revise**: `yes` = Claude must `challenge` or `update` the memory that the user's message contradicts.
- **facts**: the final answer or the diff must contain at least one of these strings (case-insensitive). This checks that the stored fact was actually used. A hook may have injected it, so it can pass without a `query` call.

## Seeded memories

| key | type | title |
|---|---|---|
| money | convention | Money is integer cents, never float |
| finalize | hazard | invoices.finalize() is not idempotent |
| logging | decision | Use structlog, not stdlib logging |
| tests | workflow | Run tests with make test-fast |
| migrations | hazard | Never edit an applied migration |
| flags | convention | Feature flags go through flags.is_on() |
| rounding | decision | Tax uses ROUND_HALF_EVEN |
| export | hazard | Staging EXPORT_BUCKET points at the prod bucket |
| errors | convention | API errors use problem+json |
| release | workflow | Release by tagging vYYYY.MM.N |

## Cases

| id | category | query | create | revise | facts / create terms | prompt |
|---|---|---|---|---|---|---|
| pq-tests | project_question | yes | no | no | `test-fast` | How do I run the tests in this repo? |
| pq-logging | project_question | yes | no | no | `structlog` | Which logging library should I use when I add log lines here? |
| pq-money | project_question | yes | no | no | `cents` | How should I represent a currency amount in new billing code? |
| pq-flags | project_question | yes | no | no | `is_on`, `flags.toml` | I want to put a new checkout flow behind a feature flag. How do we do flags in this project? |
| pq-release | project_question | yes | no | no | `v2026`, `vYYYY`, `tag` | What's the process for cutting a release? |
| pq-rounding | project_question | yes | no | no | `HALF_EVEN`, `banker` | What rounding mode do we use for tax? |
| pq-export | project_question | yes | no | no | `dry-run`, `prod` | Can I run scripts/export.py against staging to test it? |
| pq-errors | project_question | yes | no | no | `problem` | How should an endpoint here return an error to the client? |
| pq-migration | project_question | yes | no | no | `0008`, `new migration`, `new file` | I need to add a `phone` column to customers. Where does that change go? |
| pq-finalize | project_question | yes | no | no | `idempot`, `twice`, `finalize_once` | Anything I should know before I touch invoice finalization? |
| ed-discount | before_edit | yes | no | no | `discount_cents`, `_cents` | Add a discount field to the Invoice class in src/billing/invoices.py. |
| ed-refund-log | before_edit | yes | no | no | `structlog` | Add a log line to refund() in src/billing/refunds.py that records the invoice id. |
| ed-flag | before_edit | yes | no | no | `is_on(` | Gate the checkout() function in src/api/checkout.py behind a new feature flag for the new checkout. |
| ed-migration-typo | before_edit | yes | no | no | `0008` | There's a typo in a column name in migrations/0007_add_customers.sql (`emial`). Fix it. |
| ed-finalize-retry | before_edit | yes | no | no | `finalize_once`, `idempot`, `ledger_ref` | finalize() in src/billing/invoices.py sometimes fails on network errors. Make it retry up to 3 times. |
| ed-404 | before_edit | yes | no | no | `problem(` | Make GET /invoices/<id> in src/api/invoices.py return a 404 when the invoice doesn't exist. |
| ed-tax | before_edit | yes | no | no | `ROUND_HALF_EVEN` | Implement tax_for(amount_cents, rate) in src/billing/tax.py. |
| ed-export-since | before_edit | yes | no | no | `dry-run` | Add a --since DATE option to scripts/export.py and try it against staging. |
| nk-jobs-explicit | new_knowledge | any | yes | no | `enqueue` | FYI: from now on all background jobs must go through `jobs.enqueue()`; never call celery directly. Please remember that for future sessions. |
| nk-python-min | new_knowledge | any | yes | no | `3.11` | We decided today to drop Python 3.10. The minimum is 3.11 now. Update pyproject.toml to match. |
| nk-sandbox-ratelimit | new_knowledge | any | yes | no | `5 req`, `rate` | Heads up: the payments sandbox rate-limits at 5 requests per second. That's why test_payments flakes in CI. Can you add a short sleep between calls in tests/test_payments.py? |
| nk-owner | new_knowledge | any | yes | no | `Priya` | Note for later: Priya owns the ledger integration; any change to src/billing/invoices.py needs her review. |
| nk-timezone | new_knowledge | any | yes | no | `UTC` | Please record this: all timestamps in the database are stored in UTC, and we convert to the customer's timezone only in the API layer. |
| nk-env-var | new_knowledge | any | yes | no | `PAYMENTS_LIVE` | I just found out the hard way that the app silently uses a fake payment gateway unless PAYMENTS_LIVE=1 is set. Took me two hours. |
| nk-debug-lesson | new_knowledge | any | yes | no | `enable-local-file-access`, `wkhtmltopdf` | Got it working: the invoice PDF tests were failing because wkhtmltopdf needs the `--enable-local-file-access` flag in newer versions. I've already fixed it locally, thanks. |
| nk-api-version | new_knowledge | any | yes | no | `/v2`, `v1` | Decision from today's design review: public API routes are versioned under /v2 from now on, and /v1 is frozen. Keep that in mind going forward. |
| ct-loguru | contradiction | any | any | yes | `loguru` | We moved off structlog last week, we use loguru now. Update the log line in src/billing/refunds.py to loguru. |
| ct-just | contradiction | any | any | yes | `just` | We removed the Makefile; tests run with `just test` now. How do I run only the fast tests? |
| ct-rounding | contradiction | any | any | yes | `HALF_UP` | Finance changed their mind: tax now rounds half-up. Please update src/billing/tax.py accordingly. |
| ct-release | contradiction | any | any | yes | - | FYI the release process changed: we now release with `make release`, which tags and publishes for us. |
| ng-tuple | negative | any | no | no | - | What's the difference between a list and a tuple in Python? |
| ng-regex | negative | any | no | no | - | Write a regex that matches an ISO 8601 date like 2026-10-01. |
| ng-bankers | negative | any | no | no | - | In general, what is banker's rounding and why do accountants like it? |
| ng-hello | negative | any | no | no | - | hi! quick sanity check, are you there? |
| ng-math | negative | any | no | no | - | What is 17 * 23? |
| ng-http409 | negative | any | no | no | - | What does HTTP status 409 mean? |
| ng-rebase | negative | any | no | no | - | Explain what `git rebase --onto` does, with a short example. |
| ng-yaml | negative | any | no | no | - | Convert this JSON to YAML: {"a": 1, "b": [true, null]} |
| nc-rename | no_create | any | no | no | - | In src/billing/tax.py, rename the local variable `x` to `total`. |
| nc-docstring | no_create | any | no | no | - | Add a one-line docstring to every function in src/flags.py. |
| nc-readme | no_create | any | no | no | - | Fix the spelling mistakes in README.md. |

## Notes on specific cases

- `ed-migration-typo`: Pass needs a new 0008 migration; 0007 must be unchanged.
- `ed-finalize-retry`: Correct behaviour pushes back or retries via finalize_once, never blind retries.
- `ng-bankers`: Near a stored topic, but a general-knowledge question.
