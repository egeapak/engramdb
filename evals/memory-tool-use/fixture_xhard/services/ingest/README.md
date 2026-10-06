# ingest

Loads the PSP's daily settlement files into the billing database, so billing
knows which payments actually arrived. Runs nightly as a batch job, deployed
separately from the billing app.

- `ingest/` - the job code
- `ingest.toml` - where the files come from
- `tests/` - unit tests
