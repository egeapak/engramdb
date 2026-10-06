# Retrieval probe results

149 real agent queries from the memory-tool-use eval (99 labeled with a target
memory, 50 with no relevant memory), replayed through the CLI against the
seeded fixture with the default config. Run with `probe.py --bin <dir>`.

## Step 5 (rank threshold after rerank, marked fallback, keyword never lowers a score)

| Run as rank | recall | right memory first | empty | all-weak | noise | no-match results |
|---|---|---|---|---|---|---|
| Before (step 4 binary) | 52% | 52% | 45% | 0% | 0.2 | 0.2 |
| After (step 5 binary) | **97%** | **91%** | **0%** | 36% | 0.8 | 3.0 (all marked) |
| Agent asked rank, before | 24% | 24% | 76% | 0% | 0.0 | — |
| Agent asked rank, after | **100%** | 82% | **0%** | 58% | 1.2 | — |

Filter mode is unchanged: recall 98%, right memory first 91%, 0% empty.

The remaining "first" misses are `ed-discount` queries about
`src/billing/invoices.py`, where the memory scoped to that file ranks first and
the labeled money memory second or third; both are relevant.
