# Testing notes

## Unit tests

`make test-fast` runs everything that needs neither Postgres nor the network.

## FX fixtures

The FX tests never call a live rate feed. They read per-day JSON fixtures that are
generated, not committed: run `python scripts/gen_fx_fixtures.py` once after cloning
(and after adding a snapshot CSV), then run the tests with
`LEDGERLINE_FX_FIXTURES=tests/fixtures/fx`.

## Golden files

Statement rendering is checked against golden files in `tests/golden/`. Do not edit a
golden file by hand: `tests/golden/MANIFEST` records each file's checksum and the test
refuses files that do not match it. After an intended output change, run
`python scripts/regen_goldens.py` and review the diff.
