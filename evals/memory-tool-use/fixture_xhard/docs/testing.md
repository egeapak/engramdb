# Testing notes

## Unit tests

`make test-fast` runs everything that needs neither Postgres nor the network.

## FX fixtures

The FX tests never call a live rate feed. They read per-day rate fixtures from
`tests/fixtures/fx/`.

## Golden files

Statement rendering is checked against golden files in `tests/golden/`.
