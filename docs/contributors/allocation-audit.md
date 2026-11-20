# Allocation audit

A two-pass audit of allocations, heap, RSS and CPU on EngramDB's hot paths.
Every change was measured on its own with a counting allocator. A change was
kept only when it measurably helped. The rejected ones are listed with their
numbers.

**Overall result (original baseline → final, same run, interleaved):**

| path | allocations | bytes allocated | CPU |
|---|---:|---:|---:|
| `query` filter/rank, 1k memories | −69.9% | −17% | −26 to −27% |
| `query` filter/rank, 5k memories | −69.2% | −8% | −21 to −24% |
| `query` by path, 1k memories | −53.5% | −32.7% | −20.1% |
| PreToolUse hook, 1k memories | −53.6% | −30.0% | −22.3% |
| UserPromptSubmit hook, 1k memories | −69.7% | −17.4% | −24.7% |
| SessionStart hook, 1k memories | −59.1% | −16.6% | −18.5% |
| PostToolUse hook, 1k memories | −43.2% | −31.5% | −16.2% |
| `reindex`, 1k memories | −29.5% | −1.1% | −31.1% |
| memory-file write+parse | −66.9% | −12.4% | −39.6% |
| transcript digest | −8.8% | −18.6% | −7.2% |
| dashboard grid layout, 1k memories | −72.7% | −55.4% | −1.6% (noise) |
| `create` / `update` | −1.7% / −2.5% | −0.6% / −0.8% | no reliable signal (see *Noise*) |

Peak RSS dropped 7–12% on the 5k-memory query and hook scenarios and moved
less than 4% elsewhere. Peak live heap dropped 5–6% on text queries.

Kept changes: 8 in pass 1 (plus one supporting `as_str` commit) and 3 in pass 2.
Rejected changes: 1 in each pass.

## Environment

- 4 vCPU Intel Xeon @ 2.10 GHz, 15 GiB RAM, Linux 6.18, shared cloud VM.
- rustc 1.97.0. Built with the `bench` profile, which is the release profile
  (`opt-level = 3`) with symbols kept (`CARGO_PROFILE_BENCH_STRIP=false`).
- No model is loaded. Queries take the keyword path and hooks use keyword
  situations, so the numbers cover EngramDB's own code plus LanceDB, never
  ONNX.
- Base commit: master `62659c4`. The harness commit `14a2568` is the
  measured baseline: it changes no library code.

## Harness

`crates/engram-cli/examples/alloc_profile.rs` installs a counting
`#[global_allocator]`, which exists only in this example and never reaches
the shipped binary. It runs each scenario in its own child process and
reports these values per operation:

- **allocs/op** and **KiB/op**: number of allocations and bytes allocated.
- **peak heap**: peak live heap above the scenario's starting point.
- **peak RSS**: `VmHWM` of the measured phase, after a `clear_refs` reset.
- **CPU**: user + sys time for the whole process, including tokio workers
  and LanceDB's threads, from `getrusage`.
- **wall**: elapsed time.

Each scenario does one warm-up rep that is not kept, then `ALLOC_REPS`
measured reps. The default is 7; the runs in this report used 5.

Scenarios:

- **Queries.** `q-filter`, `q-rank` and `q-path` at 100, 1,000 and 5,000
  memories, run on persistent, compacted stores. `q-filter` and `q-rank` are
  text queries; `q-path` is a file-path query with no text, the shape the
  PreToolUse hook sends.
- **Writes.** `create-1k`, `update-1k` and `reindex-1k`, each on a scratch
  store built per process.
- **Hooks.** `hook-pre`, `hook-prompt`, `hook-post` and `hook-start`.
  `execute_hook` runs with a cached store, keyword situations and collected
  usage events, the same shape as the daemon path.
- **Pure functions.**
  - `memfile-roundtrip`: write and parse one memory file.
  - `transcript-digest`: parse a synthetic 200-turn session and render its
    index text.
  - `defang`: the transcript sanitizer.
  - `daemon-frame`: write and read one protocol frame.
  - `layout-1k`: the dashboard's scope-force and grid layouts.

To re-run it:

```bash
# ORT/protoc as in .claude/CLAUDE.md, then:
cargo build --profile bench -p engram-cli --example alloc_profile
B=target/release/examples/alloc_profile
$B /tmp/allocm build                       # the persistent stores, once
$B /tmp/allocm all base.json               # every scenario
$B /tmp/allocm show base.json              # absolute table with CPU spread
$B /tmp/allocm sizes                       # struct sizes
# interleaved A/B: copy each build's binary aside first
ALLOC_REPS=5 $B /tmp/allocm ab 3 out/ q-,hook- ./bin-before ./bin-after
$B /tmp/allocm compare out/bin-before.json out/bin-after.json
```

`ab` runs every matching scenario once per binary per round and alternates
the binary order each round, so load drift falls on every binary alike.
Comparisons are always made within one `ab` run.

For profiling, run
`valgrind --tool=dhat --dhat-out-file=x.json $B /tmp/allocm run <scenario>`.
The hot spots in this report came from dhat profiles of the baseline and of
each pass's head.

### Noise

- **Allocation counts are deterministic.** They repeat exactly between runs
  of the same binary, except `create` and `update`, which vary about ±0.5%
  with LanceDB internals.
- **CPU spread across reps** is ±4–19% on queries and hooks, and up to ±64%
  on `create-1k`. A CPU change smaller than the spread is reported as noise.
- **`create-1k` and `update-1k` CPU cannot be trusted.** Each rep adds
  memories, and each write adds a LanceDB fragment. Per-op cost therefore
  rises from rep to rep: in one run allocations went from 124k to 365k per
  op between the first and ninth rep. Absolute counts also drift between
  runs. Three A/Bs of the same two binaries (base → pass 1) gave create CPU
  of +21%, +19% and −12.6%. Only the within-run allocation deltas for
  create and update are reported as findings.
- **`reindex-1k` CPU** spreads ±17%. Its large pass-1 gains (−22% to −31%,
  repeated across runs) are well outside that spread. Its pass-2 delta
  (−14% in the isolated A/B, +9% in the combined run) is not.

## Baseline

`14a2568`, 5 reps × 3 interleaved rounds. All values are per operation. The
± figure is the CPU spread across reps.

| scenario | ops/rep | allocs/op | KiB alloc/op | peak heap KiB | peak RSS MiB | CPU µs/op (±%) | wall µs/op |
|---|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 60 | 49400 | 4486.5 | 599 | 83.4 | 8585 (±8%) | 7757 |
| q-filter-1k | 20 | 121667 | 14142.0 | 5365 | 98.2 | 22207 (±6%) | 21339 |
| q-filter-5k | 6 | 451746 | 65807.5 | 23121 | 139.7 | 102078 (±11%) | 88728 |
| q-rank-100 | 60 | 48458 | 4234.2 | 599 | 83.8 | 7744 (±4%) | 7205 |
| q-rank-1k | 20 | 121430 | 14806.4 | 5365 | 98.3 | 23223 (±8%) | 22271 |
| q-rank-5k | 6 | 451753 | 71383.2 | 23149 | 140.8 | 105358 (±8%) | 92741 |
| q-path-100 | 60 | 40724 | 3369.5 | 221 | 81.7 | 6292 (±6%) | 5986 |
| q-path-1k | 20 | 56392 | 6373.2 | 1653 | 86.5 | 11008 (±12%) | 10757 |
| q-path-5k | 6 | 133444 | 24384.5 | 6095 | 101.2 | 41357 (±19%) | 34184 |
| create-1k | 20 | 204912 | 25459.1 | 6427 | 122.1 | 136188 (±64%) | 60301 |
| update-1k | 20 | 129201 | 19829.6 | 5888 | 122.2 | 54592 (±11%) | 38400 |
| reindex-1k | 2 | 465288 | 117342.6 | 12798 | 122.0 | 445112 (±17%) | 166638 |
| hook-pre-1k | 40 | 59412 | 7173.9 | 1654 | 90.6 | 12477 (±12%) | 12008 |
| hook-pre-5k | 10 | 136270 | 25156.4 | 6095 | 108.4 | 47081 (±13%) | 35781 |
| hook-prompt-1k | 40 | 122346 | 14233.0 | 5366 | 100.1 | 26643 (±10%) | 25862 |
| hook-post-1k | 40 | 9412 | 781.8 | 69 | 76.7 | 1845 (±10%) | 1891 |
| hook-start-1k | 40 | 52100 | 12986.7 | 5538 | 88.3 | 10726 (±11%) | 10619 |
| memfile-roundtrip | 1000 | 229 | 43.8 | 21 | 26.9 | 42 (±33%) | 42 |
| transcript-digest | 5 | 46855 | 9463.8 | 821 | 24.4 | 7052 (±13%) | 7066 |
| defang | 200 | 12 | 2.9 | 3 | 21.8 | 14 (±27%) | 14 |
| daemon-frame | 500 | 116 | 484.3 | 226 | 21.3 | 242 (±36%) | 244 |
| layout-1k | 2 | 50876 | 10238.9 | 411 | 22.2 | 121902 (±16%) | 123950 |

### Where the baseline went

dhat profiles of the baseline (shares of allocation blocks):

- **LanceDB projection parsing, ~45% of a path query's allocations and ~22%
  of a text query's** (that is what c1 removed).
  `Select::Columns` backtick-escapes every column name. Lance's
  `Planner::parse_expr` fast path does not accept a quoted identifier, so
  every projected column went through a full SQL parse and a
  `Schema::try_from`, on every index read.
- **Per-row JSON parse of `keyword_stems` in `batch_to_for_filtering`, about
  64% of a text query's allocations once c1 had landed.** That is ~111 allocations per candidate
  row, one `String` per stem plus the hash-map growth.
- **Memory-file parse and write.** The parser built a `format!` marker
  string per field per line. The writer built intermediate `String`s and
  `Vec`s for every section.
- **`get_batch` directory scan.** It allocated an id and a path for every
  file in the directory, including ids nobody asked for.
- **Hex encoding.** `format!("{b:02x}")` was called once per byte, in 13
  places: digests, project ids, the layout cache and model integrity checks.
- **Dashboard grid layout.** It built a fresh `HashMap<(i64, i64), Vec<usize>>`
  every iteration. Removing that (p2a) removed 73% of the layout scenario's
  allocations, which also include the scope-force layout.

## Pass 1: needless allocations, layout, false sharing

Each change was measured alone against the baseline binary. A table shows
only the scenarios that A/B covered.

### Kept

**c1 `7dae0de`: Project LanceDB queries without per-column SQL parsing.**
`lance_index::project_columns` builds `Select::Dynamic` with each bare name
as its own expression, which takes Lance's fast path. All 24
`Select::Columns` sites were changed: memories, edges, conversations,
hook_events and memory_events. The projected columns are the same.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 49392 → 22310 | -54.8% | 4483.0 → 2839.6 | -36.7% | 599 → 599 | +0.1% | 83.5 → 83.3 | -0.3% | 8721 → 6788 | -22.2% | 8024 → 6053 | -24.6% |
| q-filter-1k | 121666 → 94581 | -22.3% | 14143.4 → 12496.8 | -11.6% | 5365 → 5365 | +0.0% | 98.4 → 98.4 | -0.0% | 23986 → 21787 | -9.2% | 23088 → 21157 | -8.4% |
| q-filter-5k | 451695 → 424606 | -6.0% | 65818.8 → 64166.6 | -2.5% | 23118 → 23111 | -0.0% | 139.2 → 140.4 | +0.9% | 112880 → 105143 | -6.9% | 98301 → 91306 | -7.1% |
| q-rank-100 | 48457 → 21372 | -55.9% | 4234.2 → 2589.9 | -38.8% | 599 → 599 | +0.1% | 83.5 → 83.6 | +0.0% | 8030 → 6102 | -24.0% | 7480 → 5654 | -24.4% |
| q-rank-1k | 121414 → 94345 | -22.3% | 14804.1 → 13159.8 | -11.1% | 5365 → 5365 | +0.0% | 98.6 → 98.3 | -0.3% | 22772 → 21738 | -4.5% | 22125 → 20632 | -6.7% |
| q-rank-5k | 451735 → 424655 | -6.0% | 71385.7 → 69740.6 | -2.3% | 23149 → 23149 | +0.0% | 139.8 → 140.9 | +0.8% | 120956 → 114680 | -5.2% | 104270 → 100316 | -3.8% |
| q-path-100 | 40724 → 14996 | -63.2% | 3369.4 → 1807.6 | -46.4% | 221 → 221 | +0.1% | 81.7 → 82.0 | +0.3% | 6405 → 4585 | -28.4% | 6114 → 4316 | -29.4% |
| q-path-1k | 56392 → 30664 | -45.6% | 6373.2 → 4811.3 | -24.5% | 1653 → 1654 | +0.0% | 87.2 → 86.6 | -0.7% | 11210 → 9370 | -16.4% | 10972 → 9218 | -16.0% |
| q-path-5k | 133444 → 107717 | -19.3% | 24384.5 → 22823.1 | -6.4% | 6095 → 6095 | +0.0% | 104.7 → 103.4 | -1.3% | 47027 → 41801 | -11.1% | 35331 → 32618 | -7.7% |
| create-1k | 160477 → 157898 | -1.6% | 24179.8 → 24033.8 | -0.6% | 6427 → 6427 | +0.0% | 120.1 → 121.2 | +1.0% | 131007 → 144260 | +10.1% | 53350 → 56751 | +6.4% |
| update-1k | 101099 → 98637 | -2.4% | 19027.9 → 18873.8 | -0.8% | 5888 → 5888 | -0.0% | 121.2 → 120.7 | -0.4% | 51881 → 45990 | -11.4% | 33126 → 30959 | -6.5% |
| reindex-1k | 453666 → 451875 | -0.4% | 116996.4 → 116894.2 | -0.1% | 12797 → 12797 | +0.0% | 122.0 → 122.0 | +0.1% | 520711 → 544724 | +4.6% | 178409 → 180547 | +1.2% |
| hook-pre-1k | 59412 → 33684 | -43.3% | 7173.9 → 5612.0 | -21.8% | 1654 → 1654 | +0.0% | 90.7 → 90.7 | +0.0% | 12848 → 10845 | -15.6% | 12555 → 10544 | -16.0% |
| hook-pre-5k | 136270 → 110542 | -18.9% | 25157.3 → 23594.3 | -6.2% | 6095 → 6096 | +0.0% | 109.0 → 110.6 | +1.5% | 46460 → 45113 | -2.9% | 37157 → 34620 | -6.8% |
| hook-prompt-1k | 122349 → 95261 | -22.1% | 14233.2 → 12589.1 | -11.6% | 5366 → 5366 | +0.0% | 99.9 → 100.2 | +0.3% | 25797 → 25113 | -2.7% | 24891 → 24075 | -3.3% |
| hook-post-1k | 9412 → 5350 | -43.2% | 781.8 → 535.2 | -31.5% | 69 → 69 | +0.3% | 77.2 → 73.9 | -4.2% | 1749 → 1395 | -20.2% | 1732 → 1383 | -20.1% |
| hook-start-1k | 52100 → 26372 | -49.4% | 12986.7 → 11424.8 | -12.0% | 5538 → 5539 | +0.0% | 88.0 → 88.2 | +0.2% | 11322 → 9501 | -16.1% | 11070 → 9243 | -16.5% |

**c2 `e527c7f`: Keep only the requested ids when `get_batch` scans a memory directory.**
`scan_dir_to_map` matches the filename against the wanted set before
allocating an id or a path.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 49390 → 49139 | -0.5% | 4482.5 → 4448.7 | -0.8% | 599 → 599 | +0.0% | 83.4 → 83.2 | -0.2% | 9066 → 8984 | -0.9% | 8470 → 8410 | -0.7% |
| q-filter-1k | 121662 → 118798 | -2.4% | 14142.1 → 13670.6 | -3.3% | 5365 → 5365 | +0.0% | 98.3 → 97.0 | -1.3% | 24065 → 23877 | -0.8% | 23238 → 22751 | -2.1% |
| q-filter-5k | 451703 → 436876 | -3.3% | 65814.0 → 63554.2 | -3.4% | 23120 → 23111 | -0.0% | 136.8 → 130.8 | -4.4% | 111340 → 112349 | +0.9% | 98352 → 97124 | -1.2% |
| q-rank-100 | 48455 → 48172 | -0.6% | 4233.2 → 4194.8 | -0.9% | 599 → 599 | +0.0% | 84.0 → 83.1 | -1.1% | 8355 → 8181 | -2.1% | 7786 → 7585 | -2.6% |
| q-rank-1k | 121423 → 118541 | -2.4% | 14804.1 → 14333.2 | -3.2% | 5365 → 5365 | +0.0% | 98.0 → 97.0 | -1.0% | 23909 → 24104 | +0.8% | 23166 → 23191 | +0.1% |
| q-rank-5k | 451718 → 436846 | -3.3% | 71386.2 → 69136.6 | -3.2% | 23149 → 23149 | +0.0% | 141.5 → 132.3 | -6.5% | 115622 → 119964 | +3.8% | 101035 → 103570 | +2.5% |
| q-path-100 | 40724 → 40455 | -0.7% | 3369.5 → 3332.6 | -1.1% | 221 → 221 | +0.0% | 82.1 → 81.7 | -0.5% | 6681 → 6639 | -0.6% | 6540 → 6401 | -2.1% |
| q-path-1k | 56392 → 53526 | -5.1% | 6373.1 → 5900.9 | -7.4% | 1653 → 1653 | +0.0% | 86.6 → 85.3 | -1.5% | 11682 → 11034 | -5.5% | 11474 → 10881 | -5.2% |
| q-path-5k | 133444 → 118576 | -11.1% | 24384.3 → 22132.8 | -9.2% | 6095 → 6095 | +0.0% | 105.6 → 93.3 | -11.7% | 47988 → 42776 | -10.9% | 37388 → 32078 | -14.2% |
| update-1k | 104471 → 103910 | -0.5% | 19110.8 → 19123.7 | +0.1% | 5888 → 5888 | +0.0% | 120.8 → 121.0 | +0.2% | 48328 → 51860 | +7.3% | 33848 → 33933 | +0.3% |
| hook-pre-1k | 59412 → 56606 | -4.7% | 7173.9 → 6708.3 | -6.5% | 1654 → 1654 | +0.0% | 90.8 → 87.6 | -3.6% | 12550 → 11892 | -5.2% | 12189 → 11761 | -3.5% |
| hook-pre-5k | 136270 → 121463 | -10.9% | 25156.9 → 22912.0 | -8.9% | 6095 → 6095 | +0.0% | 110.2 → 95.7 | -13.2% | 48238 → 43564 | -9.7% | 37072 → 33091 | -10.7% |
| hook-prompt-1k | 122348 → 119478 | -2.3% | 14233.1 → 13760.3 | -3.3% | 5366 → 5366 | +0.0% | 100.1 → 98.9 | -1.2% | 27489 → 26265 | -4.5% | 27155 → 25516 | -6.0% |
| hook-post-1k | 9412 → 9412 | +0.0% | 781.8 → 781.8 | +0.0% | 69 → 69 | +0.0% | 77.0 → 76.7 | -0.4% | 2024 → 1984 | -2.0% | 1981 → 2010 | +1.4% |
| hook-start-1k | 52100 → 49233 | -5.5% | 12986.7 → 12514.3 | -3.6% | 5538 → 5538 | +0.0% | 88.9 → 86.9 | -2.2% | 11275 → 10895 | -3.4% | 11133 → 10779 | -3.2% |

**c3 `3fbb2e0`: Write memory files without intermediate strings.**
The frontmatter and hidden block serialize from borrowed `*Ref` structs.
Sections are written with `write!` into one pre-sized `String`. The
sanitizers return `Cow`. Byte-identity was checked by writing stores with
both binaries and running `diff -r` on every `memories/` tree.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| create-1k | 164880 → 164100 | -0.5% | 24307.9 → 24285.2 | -0.1% | 6427 → 6427 | +0.0% | 121.0 → 120.7 | -0.2% | 156557 → 123658 | -21.0% | 59592 → 50312 | -15.6% |
| update-1k | 107843 → 107216 | -0.6% | 19219.6 → 19215.8 | -0.0% | 5888 → 5888 | +0.0% | 120.4 → 121.8 | +1.1% | 52494 → 47100 | -10.3% | 34701 → 31801 | -8.4% |
| reindex-1k | 454900 → 454684 | -0.0% | 117216.3 → 117072.8 | -0.1% | 12910 → 12797 | -0.9% | 121.8 → 122.6 | +0.7% | 577710 → 583878 | +1.1% | 189244 → 193510 | +2.3% |
| memfile-roundtrip | 229 → 165 | -27.9% | 43.8 → 41.0 | -6.3% | 21 → 21 | +0.0% | 27.0 → 28.0 | +3.6% | 42 → 38 | -9.3% | 42 → 38 | -7.6% |

**c4 `878eb35`: Parse memory files without per-field marker strings.**
`strip_field_marker` replaces a `format!("**{field}:**")` per field per
line. The body sections map borrows its keys, and values are moved out
rather than cloned.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-path-100 | 40724 → 40218 | -1.2% | 3369.5 → 3353.9 | -0.5% | 221 → 221 | +0.0% | 81.9 → 82.0 | +0.2% | 6386 → 6198 | -2.9% | 6171 → 6023 | -2.4% |
| q-path-1k | 56392 → 55491 | -1.6% | 6373.2 → 6346.8 | -0.4% | 1653 → 1653 | +0.0% | 86.8 → 86.9 | +0.2% | 11252 → 11817 | +5.0% | 11395 → 11866 | +4.1% |
| q-path-5k | 133444 → 132591 | -0.6% | 24384.4 → 24357.6 | -0.1% | 6095 → 6095 | -0.0% | 104.0 → 104.3 | +0.3% | 43207 → 43875 | +1.5% | 34160 → 34643 | +1.4% |
| update-1k | 111212 → 110567 | -0.6% | 19323.0 → 19309.3 | -0.1% | 5888 → 5888 | -0.0% | 121.3 → 120.0 | -1.1% | 55763 → 54526 | -2.2% | 36643 → 35416 | -3.4% |
| reindex-1k | 456142 → 367172 | -19.5% | 117080.6 → 114473.3 | -2.2% | 12798 → 12816 | +0.1% | 122.9 → 122.3 | -0.5% | 516170 → 367374 | -28.8% | 180952 → 140310 | -22.5% |
| hook-pre-1k | 59412 → 57199 | -3.7% | 7173.9 → 7107.4 | -0.9% | 1654 → 1654 | +0.0% | 90.9 → 90.8 | -0.1% | 13498 → 12878 | -4.6% | 13172 → 12737 | -3.3% |
| hook-pre-5k | 136270 → 134141 | -1.6% | 25156.8 → 25091.6 | -0.3% | 6095 → 6095 | +0.0% | 107.5 → 108.7 | +1.1% | 46916 → 49163 | +4.8% | 36917 → 37185 | +0.7% |
| hook-start-1k | 52100 → 51190 | -1.7% | 12986.7 → 12958.7 | -0.2% | 5538 → 5538 | +0.0% | 88.7 → 88.5 | -0.3% | 11103 → 11461 | +3.2% | 11212 → 11488 | +2.5% |
| memfile-roundtrip | 229 → 140 | -38.8% | 43.8 → 41.1 | -6.1% | 21 → 20 | -3.3% | 27.0 → 27.4 | +1.5% | 41 → 34 | -15.3% | 42 → 34 | -18.1% |

**c5 `0569f12`: Encode digests as hex without a `String` per byte.**
Adds `engram_types::hex::lower_hex`, tested against `format!("{b:02x}")`.
It replaces 13 call sites.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-path-1k | 56392 → 56392 | +0.0% | 6373.1 → 6373.2 | +0.0% | 1653 → 1653 | +0.0% | 86.9 → 86.9 | +0.1% | 11418 → 11608 | +1.7% | 11324 → 11516 | +1.7% |
| create-1k | 169285 → 168518 | -0.5% | 24434.0 → 24413.1 | -0.1% | 6427 → 6427 | +0.0% | 120.5 → 121.5 | +0.8% | 137299 → 110718 | -19.4% | 56226 → 48551 | -13.6% |
| update-1k | 114586 → 113992 | -0.5% | 19424.8 → 19411.3 | -0.1% | 5888 → 5888 | +0.0% | 122.0 → 121.7 | -0.2% | 58586 → 50075 | -14.5% | 37216 → 34785 | -6.5% |
| reindex-1k | 457394 → 423202 | -7.5% | 117110.0 → 116876.6 | -0.2% | 12797 → 12829 | +0.3% | 122.7 → 122.0 | -0.6% | 535434 → 447202 | -16.5% | 180743 → 162421 | -10.1% |
| hook-pre-1k | 59412 → 59412 | +0.0% | 7173.8 → 7173.9 | +0.0% | 1654 → 1654 | +0.0% | 90.6 → 90.7 | +0.1% | 12706 → 12367 | -2.7% | 12299 → 12027 | -2.2% |

**`7ae3a65`: `as_str()` on `MemoryType`, `Status`, `Visibility` and `ProvenanceSource`.**
This is a supporting commit, with no effect of its own, used by c6 and c7.
A test pins each value to the lowercased `Debug` name the old
`format!("{:?}").to_lowercase()` produced.

**c6 `96b1ef0`: Render hook context lines without re-rendering them per preview length.**
The SessionStart and PreToolUse budget loop used to re-render the whole line
for every preview length it tried. It now computes the preview once and
renders into one buffer.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| hook-pre-1k | 59412 → 59019 | -0.7% | 7173.9 → 7143.5 | -0.4% | 1654 → 1654 | +0.0% | 90.4 → 91.1 | +0.7% | 12871 → 12682 | -1.5% | 12495 → 12330 | -1.3% |
| hook-pre-5k | 136270 → 135986 | -0.2% | 25157.3 → 25138.3 | -0.1% | 6095 → 6095 | +0.0% | 109.3 → 106.6 | -2.4% | 49081 → 49219 | +0.3% | 37019 → 36998 | -0.1% |
| hook-prompt-1k | 122354 → 122023 | -0.3% | 14233.4 → 14209.7 | -0.2% | 5366 → 5366 | +0.0% | 99.6 → 100.4 | +0.7% | 26301 → 26268 | -0.1% | 25779 → 25524 | -1.0% |
| hook-post-1k | 9412 → 9412 | +0.0% | 781.8 → 781.8 | +0.0% | 69 → 69 | +0.0% | 76.9 → 77.5 | +0.7% | 1845 → 1794 | -2.8% | 1808 → 1751 | -3.1% |
| hook-start-1k | 52100 → 51112 | -1.9% | 12986.7 → 12904.3 | -0.6% | 5538 → 5538 | +0.0% | 89.1 → 89.0 | -0.1% | 10994 → 11012 | +0.2% | 11109 → 10958 | -1.4% |

**c7 `f1bf26d`: Build index rows from borrowed strings.**
`entries_to_batch` pushes `&str` and `as_str()` into the Arrow builders
instead of owned `String`s.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| create-1k | 173689 → 172941 | -0.4% | 24560.5 → 24539.4 | -0.1% | 6427 → 6427 | +0.0% | 121.5 → 121.3 | -0.1% | 135452 → 126888 | -6.3% | 55291 → 52116 | -5.7% |
| update-1k | 117957 → 117381 | -0.5% | 19525.9 → 19501.2 | -0.1% | 5888 → 5888 | -0.0% | 122.1 → 121.1 | -0.8% | 50972 → 52680 | +3.4% | 35464 → 36077 | +1.7% |
| reindex-1k | 458637 → 445425 | -2.9% | 117192.9 → 117079.5 | -0.1% | 12828 → 13054 | +1.8% | 121.4 → 121.5 | +0.1% | 475078 → 476504 | +0.3% | 164968 → 167571 | +1.6% |

**c8 `46ce237`: Borrow tool-result text and build defanged transcript text in one buffer.**
`tool_result_text` returns a `Cow` that borrows a plain string result.
`truncate_chars` makes a single allocation. `defang` writes into one
buffer instead of a `Vec` plus `join`. The harness-tag escape is skipped
when the fence marker is absent.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| transcript-digest | 46855 → 42739 | -8.8% | 9463.8 → 7706.1 | -18.6% | 821 → 821 | -0.0% | 24.3 → 24.4 | +0.6% | 6836 → 6597 | -3.5% | 6862 → 6626 | -3.4% |
| defang | 12 → 11 | -8.0% | 2.9 → 2.3 | -22.2% | 3 → 2 | -33.2% | 21.9 → 22.2 | +1.6% | 9 → 8 | -9.9% | 9 → 8 | -12.5% |

### Rejected

**c9: Box `ScoreBreakdown.via`.** `Option<LinkVia>` is 48 bytes and is
almost always `None`. Boxing it shrinks `ScoreBreakdown` from 288 to 248
bytes and `ScoredMemory` from 1008 to 968. Bytes allocated fell 0.2–2.3%,
allocation count did not change, and CPU stayed within noise. Rejected: a
public type change for a gain that is mostly unmeasurable.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 49396 → 49394 | -0.0% | 4485.2 → 4468.0 | -0.4% | 599 → 599 | +0.0% | 83.6 → 83.6 | +0.1% | 8519 → 8623 | +1.2% | 7852 → 7931 | +1.0% |
| q-filter-1k | 121670 → 121664 | -0.0% | 14142.3 → 14000.5 | -1.0% | 5365 → 5365 | +0.0% | 98.1 → 98.8 | +0.7% | 23132 → 23073 | -0.3% | 22432 → 22585 | +0.7% |
| q-filter-5k | 451750 → 451674 | -0.0% | 65799.1 → 64850.8 | -1.4% | 23116 → 22594 | -2.3% | 137.5 → 140.4 | +2.1% | 110903 → 104307 | -5.9% | 97281 → 92808 | -4.6% |
| q-rank-100 | 48455 → 48458 | +0.0% | 4233.3 → 4211.4 | -0.5% | 599 → 599 | +0.0% | 83.1 → 83.1 | -0.0% | 7901 → 7899 | -0.0% | 7466 → 7345 | -1.6% |
| q-rank-1k | 121419 → 121421 | +0.0% | 14804.2 → 14570.8 | -1.6% | 5365 → 5365 | +0.0% | 98.6 → 98.6 | +0.0% | 23217 → 23484 | +1.2% | 22907 → 22884 | -0.1% |
| q-rank-5k | 451723 → 451743 | +0.0% | 71382.5 → 69722.4 | -2.3% | 23149 → 22787 | -1.6% | 140.5 → 139.0 | -1.1% | 111042 → 115932 | +4.4% | 98667 → 101841 | +3.2% |
| q-path-100 | 40724 → 40724 | +0.0% | 3369.4 → 3363.8 | -0.2% | 221 → 221 | +0.0% | 81.8 → 81.8 | +0.0% | 6439 → 6586 | +2.3% | 6275 → 6464 | +3.0% |
| q-path-1k | 56392 → 56392 | +0.0% | 6373.1 → 6324.5 | -0.8% | 1653 → 1653 | +0.0% | 87.1 → 86.6 | -0.6% | 10759 → 11230 | +4.4% | 10663 → 11018 | +3.3% |
| q-path-5k | 133444 → 133444 | +0.0% | 24384.5 → 24153.4 | -0.9% | 6095 → 6095 | +0.0% | 104.1 → 103.1 | -1.0% | 45378 → 45698 | +0.7% | 35587 → 36480 | +2.5% |

### Pass 1 combined

Measured as the pass-1 head (`46ce237`) against the baseline, in the same
3-round run as the overall result.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 49400 → 21213 | -57.1% | 4486.5 → 2781.6 | -38.0% | 599 → 599 | +0.1% | 83.4 → 83.6 | +0.1% | 8585 → 6494 | -24.4% | 7757 → 5639 | -27.3% |
| q-filter-1k | 121667 → 90818 | -25.4% | 14142.0 → 11999.0 | -15.2% | 5365 → 5365 | +0.0% | 98.2 → 97.2 | -1.0% | 22207 → 20753 | -6.5% | 21339 → 19942 | -6.6% |
| q-filter-5k | 451746 → 408946 | -9.5% | 65807.5 → 61890.9 | -6.0% | 23121 → 23127 | +0.0% | 139.7 → 131.9 | -5.6% | 102078 → 97598 | -4.4% | 88728 → 85989 | -3.1% |
| q-rank-100 | 48458 → 20831 | -57.0% | 4234.2 → 2544.8 | -39.9% | 599 → 599 | +0.1% | 83.8 → 83.3 | -0.7% | 7744 → 5929 | -23.4% | 7205 → 5369 | -25.5% |
| q-rank-1k | 121430 → 90742 | -25.3% | 14806.4 → 12664.7 | -14.5% | 5365 → 5365 | +0.0% | 98.3 → 97.1 | -1.3% | 23223 → 21287 | -8.3% | 22271 → 19878 | -10.7% |
| q-rank-5k | 451753 → 408954 | -9.5% | 71383.2 → 67461.1 | -5.5% | 23149 → 23149 | +0.0% | 140.8 → 132.6 | -5.8% | 105358 → 105551 | +0.2% | 92741 → 93862 | +1.2% |
| q-path-100 | 40724 → 14220 | -65.1% | 3369.5 → 1755.1 | -47.9% | 221 → 221 | +0.1% | 81.7 → 81.5 | -0.2% | 6292 → 4431 | -29.6% | 5986 → 4146 | -30.7% |
| q-path-1k | 56392 → 26896 | -52.3% | 6373.2 → 4312.6 | -32.3% | 1653 → 1654 | +0.0% | 86.5 → 84.6 | -2.2% | 11008 → 8867 | -19.4% | 10757 → 8393 | -22.0% |
| q-path-5k | 133444 → 91995 | -31.1% | 24384.5 → 20544.7 | -15.7% | 6095 → 6095 | +0.0% | 101.2 → 93.6 | -7.5% | 41357 → 35285 | -14.7% | 34184 → 26367 | -22.9% |
| create-1k | 204912 → 202240 | -1.3% | 25459.1 → 25312.0 | -0.6% | 6427 → 6427 | +0.0% | 122.1 → 122.2 | +0.0% | 136188 → 129653 | -4.8% | 60301 → 57721 | -4.3% |
| update-1k | 129201 → 126542 | -2.1% | 19829.6 → 19687.8 | -0.7% | 5888 → 5888 | +0.0% | 122.2 → 122.2 | -0.1% | 54592 → 61278 | +12.2% | 38400 → 40563 | +5.6% |
| reindex-1k | 465288 → 327762 | -29.6% | 117342.6 → 114005.5 | -2.8% | 12798 → 12816 | +0.1% | 122.0 → 122.3 | +0.3% | 445112 → 281296 | -36.8% | 166638 → 114511 | -31.3% |
| hook-pre-1k | 59412 → 28254 | -52.4% | 7173.9 → 5045.1 | -29.7% | 1654 → 1654 | +0.0% | 90.6 → 87.5 | -3.4% | 12477 → 9774 | -21.7% | 12008 → 9254 | -22.9% |
| hook-pre-5k | 136270 → 93304 | -31.5% | 25156.4 → 21261.5 | -15.5% | 6095 → 6096 | +0.0% | 108.4 → 95.3 | -12.1% | 47081 → 40874 | -13.2% | 35781 → 30224 | -15.5% |
| hook-prompt-1k | 122346 → 91183 | -25.5% | 14233.0 → 12058.9 | -15.3% | 5366 → 5366 | +0.0% | 100.1 → 99.0 | -1.1% | 26643 → 23987 | -10.0% | 25862 → 23115 | -10.6% |
| hook-post-1k | 9412 → 5350 | -43.2% | 781.8 → 535.2 | -31.5% | 69 → 69 | +0.3% | 76.7 → 74.5 | -2.8% | 1845 → 1569 | -15.0% | 1891 → 1620 | -14.3% |
| hook-start-1k | 52100 → 21575 | -58.6% | 12986.7 → 10833.3 | -16.6% | 5538 → 5539 | +0.0% | 88.3 → 86.5 | -2.0% | 10726 → 8481 | -20.9% | 10619 → 8463 | -20.3% |
| memfile-roundtrip | 229 → 76 | -66.7% | 43.8 → 38.4 | -12.4% | 21 → 20 | -3.3% | 26.9 → 27.7 | +2.9% | 42 → 32 | -24.6% | 42 → 32 | -24.5% |
| transcript-digest | 46855 → 42739 | -8.8% | 9463.8 → 7706.1 | -18.6% | 821 → 821 | -0.0% | 24.4 → 24.4 | +0.1% | 7052 → 6586 | -6.6% | 7066 → 6588 | -6.8% |
| defang | 12 → 11 | -8.0% | 2.9 → 2.3 | -22.2% | 3 → 2 | -33.2% | 21.8 → 22.1 | +1.3% | 14 → 6 | -55.4% | 14 → 6 | -55.4% |
| daemon-frame | 116 → 116 | +0.0% | 484.3 → 484.3 | +0.0% | 226 → 226 | +0.0% | 21.3 → 21.6 | +1.3% | 242 → 243 | +0.4% | 244 → 245 | +0.7% |
| layout-1k | 50876 → 50876 | +0.0% | 10238.9 → 10238.9 | +0.0% | 411 → 411 | +0.0% | 22.2 → 22.5 | +1.3% | 121902 → 132548 | +8.7% | 123950 → 135995 | +9.7% |

The CPU increase on `layout-1k` is noise: pass 1 does not change the layout
code, and an earlier pass-1 A/B measured −0.2%. `create-1k` is discussed
under *Noise*; a 5-round A/B with 9 reps measured −12.6% CPU.

## Pass 2: pools, capacity, static data, borrowing, stack storage

Each change was measured against the pass-1 combined state. The decay change
was measured against pass 1 + grid + compact, and the rejected stem-vector
change against that plus decay, because each builds on the one before.

### Kept

**p2a `a5bd781`: Reuse the grid layout's bucket map across iterations.**
One `HashMap` lives across all iterations. Each pass drops the cells that
were empty last time and clears the others, keeping their capacity. A
bucket still lists members in ascending index order, so the force sums run
in the same order. The pinned layout digests
(`the_fixture_layouts_match_their_pinned_digests`) are unchanged, so the
output is identical and `LAYOUT_VERSION` was not bumped.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| layout-1k | 50876 → 13905 | -72.7% | 10238.9 → 4562.6 | -55.4% | 411 → 411 | +0.0% | 22.8 → 22.5 | -1.2% | 125609 → 121329 | -3.4% | 126435 → 126101 | -0.3% |

**p2b `abbf7ea`: Store keyword stems as inline compact strings.**
`KeywordStems` now holds `CompactString` (24 bytes, the same size as
`String`). Almost every stem fits inline, so parsing a candidate row's
stems JSON needs no allocation per stem. The JSON on disk is unchanged.
`compact_str` was already in the shipped dependency tree through
`tokenizers`. Writes pay one extra copy per stem, which shows as +1.9% bytes
on `reindex`.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 21210 → 15843 | -25.3% | 2781.2 → 2753.3 | -1.0% | 599 → 571 | -4.7% | 82.9 → 83.8 | +1.0% | 6204 → 5861 | -5.5% | 5568 → 5206 | -6.5% |
| q-filter-1k | 90820 → 37366 | -58.9% | 11997.8 → 11718.9 | -2.3% | 5365 → 5085 | -5.2% | 97.0 → 95.3 | -1.7% | 18948 → 16404 | -13.4% | 17931 → 15301 | -14.7% |
| q-filter-5k | 408930 → 142376 | -65.2% | 61886.9 → 60493.2 | -2.3% | 23121 → 21708 | -6.1% | 131.7 → 123.5 | -6.2% | 101958 → 79333 | -22.2% | 88217 → 64679 | -26.7% |
| q-rank-100 | 20835 → 15462 | -25.8% | 2545.6 → 2516.9 | -1.1% | 599 → 571 | -4.7% | 83.3 → 82.8 | -0.6% | 5891 → 5558 | -5.7% | 5275 → 5030 | -4.6% |
| q-rank-1k | 90739 → 37285 | -58.9% | 12664.6 → 12382.6 | -2.2% | 5365 → 5085 | -5.2% | 96.8 → 95.6 | -1.3% | 19665 → 16865 | -14.2% | 18827 → 15605 | -17.1% |
| q-rank-5k | 408978 → 142407 | -65.2% | 67463.7 → 66070.1 | -2.1% | 23149 → 21745 | -6.1% | 133.8 → 124.4 | -7.0% | 94737 → 81450 | -14.0% | 82677 → 67529 | -18.3% |
| q-path-100 | 14220 → 14220 | +0.0% | 1755.1 → 1755.1 | +0.0% | 221 → 221 | +0.0% | 81.7 → 80.8 | -1.0% | 4305 → 4350 | +1.1% | 4018 → 4043 | +0.6% |
| q-path-1k | 26896 → 26896 | +0.0% | 4312.5 → 4312.5 | +0.0% | 1654 → 1654 | +0.0% | 85.0 → 84.6 | -0.6% | 8610 → 8624 | +0.2% | 8185 → 8301 | +1.4% |
| q-path-5k | 91995 → 91995 | -0.0% | 20544.2 → 20544.3 | +0.0% | 6095 → 6095 | -0.0% | 93.3 → 93.3 | +0.0% | 41829 → 39736 | -5.0% | 29432 → 28228 | -4.1% |
| reindex-1k | 323823 → 324604 | +0.2% | 113912.1 → 116038.9 | +1.9% | 12798 → 12515 | -2.2% | 121.9 → 109.9 | -9.8% | 340860 → 292293 | -14.2% | 132026 → 113644 | -13.9% |
| hook-pre-1k | 28254 → 28254 | +0.0% | 5045.1 → 5045.1 | +0.0% | 1654 → 1654 | +0.0% | 87.6 → 87.1 | -0.6% | 9905 → 9685 | -2.2% | 9357 → 9167 | -2.0% |
| hook-pre-5k | 93304 → 93304 | +0.0% | 21262.2 → 21262.0 | -0.0% | 6096 → 6096 | +0.0% | 95.1 → 95.7 | +0.6% | 40267 → 43500 | +8.0% | 29220 → 30410 | +4.1% |
| hook-prompt-1k | 91179 → 37727 | -58.6% | 12058.8 → 11775.8 | -2.3% | 5366 → 5086 | -5.2% | 98.6 → 96.2 | -2.4% | 22919 → 19452 | -15.1% | 22260 → 18686 | -16.1% |
| hook-post-1k | 5350 → 5350 | +0.0% | 535.2 → 535.2 | +0.0% | 69 → 69 | +0.0% | 74.2 → 74.3 | +0.2% | 1469 → 1425 | -3.0% | 1438 → 1414 | -1.7% |
| hook-start-1k | 21575 → 21575 | +0.0% | 10833.3 → 10833.3 | +0.0% | 5539 → 5539 | +0.0% | 86.4 → 87.1 | +0.8% | 8155 → 8314 | +1.9% | 8037 → 8100 | +0.8% |

**p2c `ef3f818`: Deserialize decay durations without building an unused error.**
chrono's `TimeDelta` deserializer calls `ok_or(Error::custom(..))`, which
builds and drops an error message on every successful parse: 2 allocations
per `half_life` or `ttl`, for every decayed row a query reads. The new
`lazy_delta` field deserializer accepts the same `(secs, nanos)` form and
builds the error only on failure. A test checks it against chrono's own
deserializer, including the error text and position.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 15840 → 15766 | -0.5% | 2751.8 → 2748.4 | -0.1% | 571 → 571 | +0.0% | 83.9 → 83.3 | -0.7% | 6190 → 6330 | +2.3% | 5522 → 5613 | +1.6% |
| q-filter-1k | 37372 → 36683 | -1.8% | 11720.7 → 11696.1 | -0.2% | 5085 → 5085 | +0.0% | 95.0 → 95.1 | +0.2% | 17095 → 17020 | -0.4% | 16127 → 15600 | -3.3% |
| q-filter-5k | 142416 → 139154 | -2.3% | 60478.9 → 60377.0 | -0.2% | 21711 → 21718 | +0.0% | 123.2 → 123.7 | +0.4% | 81314 → 82131 | +1.0% | 66885 → 67654 | +1.1% |
| q-rank-100 | 15464 → 15390 | -0.5% | 2516.9 → 2514.3 | -0.1% | 571 → 571 | +0.0% | 83.3 → 82.9 | -0.5% | 5667 → 5651 | -0.3% | 5131 → 5037 | -1.8% |
| q-rank-1k | 37274 → 36600 | -1.8% | 12384.5 → 12358.8 | -0.2% | 5085 → 5085 | +0.0% | 95.5 → 96.1 | +0.6% | 18066 → 17322 | -4.1% | 16889 → 16489 | -2.4% |
| q-rank-5k | 142377 → 139132 | -2.3% | 66073.0 → 65962.4 | -0.2% | 21745 → 21745 | +0.0% | 124.0 → 126.0 | +1.6% | 83837 → 83833 | -0.0% | 71338 → 69581 | -2.5% |
| q-path-100 | 14220 → 14150 | -0.5% | 1755.1 → 1752.9 | -0.1% | 221 → 221 | +0.0% | 80.8 → 81.1 | +0.4% | 4429 → 4393 | -0.8% | 4177 → 4077 | -2.4% |
| q-path-1k | 26896 → 26214 | -2.5% | 4312.5 → 4291.5 | -0.5% | 1654 → 1654 | +0.0% | 85.0 → 84.8 | -0.3% | 9057 → 9182 | +1.4% | 8804 → 8923 | +1.4% |
| q-path-5k | 91995 → 88717 | -3.6% | 20544.7 → 20443.8 | -0.5% | 6095 → 6095 | +0.0% | 93.1 → 93.2 | +0.1% | 40635 → 40730 | +0.2% | 28180 → 30318 | +7.6% |
| reindex-1k | 326058 → 325509 | -0.2% | 116039.4 → 115992.6 | -0.0% | 12545 → 12496 | -0.4% | 119.9 → 121.0 | +0.9% | 297388 → 333068 | +12.0% | 116905 → 130929 | +12.0% |
| hook-pre-1k | 28254 → 27572 | -2.4% | 5045.1 → 5024.1 | -0.4% | 1654 → 1654 | +0.0% | 87.1 → 86.8 | -0.3% | 10294 → 9798 | -4.8% | 9886 → 9326 | -5.7% |
| hook-pre-5k | 93304 → 90026 | -3.5% | 21261.8 → 21161.2 | -0.5% | 6096 → 6096 | +0.0% | 95.4 → 95.1 | -0.3% | 41200 → 39398 | -4.4% | 30688 → 29032 | -5.4% |
| hook-prompt-1k | 37729 → 37036 | -1.8% | 11778.7 → 11757.6 | -0.2% | 5086 → 5086 | +0.0% | 97.6 → 96.7 | -0.9% | 20537 → 19969 | -2.8% | 19390 → 19252 | -0.7% |
| hook-post-1k | 5350 → 5350 | +0.0% | 535.2 → 535.2 | +0.0% | 69 → 69 | +0.0% | 75.1 → 74.6 | -0.7% | 1413 → 1497 | +6.0% | 1399 → 1495 | +6.8% |
| hook-start-1k | 21575 → 21319 | -1.2% | 10833.2 → 10825.4 | -0.1% | 5539 → 5539 | +0.0% | 86.4 → 87.2 | +0.8% | 8422 → 8868 | +5.3% | 8239 → 8665 | +5.2% |
| memfile-roundtrip | 76 → 76 | -0.4% | 38.4 → 38.4 | -0.0% | 20 → 20 | +0.0% | 27.4 → 27.7 | +0.9% | 31 → 31 | -1.0% | 31 → 31 | -1.3% |

### Rejected

**p2d: Content stem counts as a sorted `Vec` instead of a `HashMap`.**
The idea was one buffer, a binary-search lookup, and no SipHash table built
per row; the map shape was kept on disk. Peak live heap fell 14–19%, but
allocations rose 2.6–3.4% and CPU rose 7–21%. The stores were written by the
old binary in hash order, so every row took the sort-and-copy fallback.
Existing user stores would pay the same cost until they were reindexed.
Rejected. It could be revisited together with a schema bump that rewrites
the stems sorted.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 15767 → 15859 | +0.6% | 2748.2 → 2838.9 | +3.3% | 571 → 492 | -13.9% | 83.3 → 83.7 | +0.5% | 6372 → 6442 | +1.1% | 5659 → 5780 | +2.1% |
| q-filter-1k | 36665 → 37627 | +2.6% | 11696.5 → 12560.3 | +7.4% | 5085 → 4278 | -15.9% | 95.3 → 94.7 | -0.6% | 17851 → 19173 | +7.4% | 17008 → 18227 | +7.2% |
| q-filter-5k | 139112 → 143856 | +3.4% | 60386.9 → 64369.6 | +6.6% | 21713 → 17696 | -18.5% | 122.8 → 120.1 | -2.2% | 83526 → 89923 | +7.7% | 68275 → 77116 | +12.9% |
| q-rank-100 | 15391 → 15486 | +0.6% | 2514.2 → 2605.2 | +3.6% | 571 → 492 | -13.9% | 83.2 → 83.4 | +0.3% | 5987 → 5919 | -1.1% | 5365 → 5374 | +0.2% |
| q-rank-1k | 36591 → 37534 | +2.6% | 12362.5 → 13227.4 | +7.0% | 5085 → 4278 | -15.9% | 96.0 → 95.0 | -1.0% | 17396 → 18889 | +8.6% | 16428 → 17674 | +7.6% |
| q-rank-5k | 139140 → 143907 | +3.4% | 65965.2 → 69933.8 | +6.0% | 21745 → 17730 | -18.5% | 123.7 → 120.6 | -2.5% | 80167 → 92817 | +15.8% | 68473 → 78596 | +14.8% |
| create-1k | 197103 → 196370 | -0.4% | 25166.3 → 25146.4 | -0.1% | 6427 → 6426 | -0.0% | 120.0 → 119.6 | -0.3% | 144542 → 143600 | -0.7% | 64477 → 62317 | -3.4% |
| reindex-1k | 326963 → 327760 | +0.2% | 115981.0 → 116577.6 | +0.5% | 12496 → 11714 | -6.3% | 119.3 → 119.0 | -0.3% | 353578 → 405486 | +14.7% | 130560 → 144893 | +11.0% |
| hook-prompt-1k | 37046 → 37986 | +2.5% | 11757.3 → 12619.4 | +7.3% | 5086 → 4279 | -15.9% | 96.9 → 96.8 | -0.1% | 19369 → 23432 | +21.0% | 18837 → 22501 | +19.5% |

### Pass 2 combined

Pass-1 head against the final head (`ef3f818`), from the same run.

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 21213 → 15770 | -25.7% | 2781.6 → 2750.8 | -1.1% | 599 → 571 | -4.7% | 83.6 → 83.4 | -0.2% | 6494 → 5864 | -9.7% | 5639 → 5284 | -6.3% |
| q-filter-1k | 90818 → 36679 | -59.6% | 11999.0 → 11694.0 | -2.5% | 5365 → 5085 | -5.2% | 97.2 → 95.1 | -2.2% | 20753 → 16499 | -20.5% | 19942 → 15670 | -21.4% |
| q-filter-5k | 408946 → 139110 | -66.0% | 61890.9 → 60383.8 | -2.4% | 23127 → 21710 | -6.1% | 131.9 → 123.4 | -6.5% | 97598 → 80901 | -17.1% | 85989 → 64781 | -24.7% |
| q-rank-100 | 20831 → 15395 | -26.1% | 2544.8 → 2515.5 | -1.2% | 599 → 571 | -4.7% | 83.3 → 82.5 | -0.9% | 5929 → 5703 | -3.8% | 5369 → 5089 | -5.2% |
| q-rank-1k | 90742 → 36596 | -59.7% | 12664.7 → 12362.2 | -2.4% | 5365 → 5085 | -5.2% | 97.1 → 95.6 | -1.5% | 21287 → 16940 | -20.4% | 19878 → 15700 | -21.0% |
| q-rank-5k | 408954 → 139147 | -66.0% | 67461.1 → 65960.2 | -2.2% | 23149 → 21745 | -6.1% | 132.6 → 125.2 | -5.6% | 105551 → 79790 | -24.4% | 93862 → 66855 | -28.8% |
| q-path-100 | 14220 → 14150 | -0.5% | 1755.1 → 1752.9 | -0.1% | 221 → 221 | +0.0% | 81.5 → 81.2 | -0.5% | 4431 → 4439 | +0.2% | 4146 → 4138 | -0.2% |
| q-path-1k | 26896 → 26214 | -2.5% | 4312.6 → 4291.5 | -0.5% | 1654 → 1654 | +0.0% | 84.6 → 85.2 | +0.8% | 8867 → 8794 | -0.8% | 8393 → 8530 | +1.6% |
| q-path-5k | 91995 → 88717 | -3.6% | 20544.7 → 20443.3 | -0.5% | 6095 → 6095 | +0.0% | 93.6 → 93.7 | +0.1% | 35285 → 38664 | +9.6% | 26367 → 28324 | +7.4% |
| create-1k | 202240 → 201507 | -0.4% | 25312.0 → 25295.1 | -0.1% | 6427 → 6427 | -0.0% | 122.2 → 120.2 | -1.6% | 129653 → 149899 | +15.6% | 57721 → 61550 | +6.6% |
| update-1k | 126542 → 125980 | -0.4% | 19687.8 → 19664.4 | -0.1% | 5888 → 5887 | -0.0% | 122.2 → 120.1 | -1.7% | 61278 → 55555 | -9.3% | 40563 → 37513 | -7.5% |
| reindex-1k | 327762 → 328214 | +0.1% | 114005.5 → 116105.2 | +1.8% | 12816 → 12528 | -2.2% | 122.3 → 119.0 | -2.7% | 281296 → 306674 | +9.0% | 114511 → 126272 | +10.3% |
| hook-pre-1k | 28254 → 27572 | -2.4% | 5045.1 → 5024.2 | -0.4% | 1654 → 1654 | +0.0% | 87.5 → 87.6 | +0.1% | 9774 → 9692 | -0.8% | 9254 → 9230 | -0.3% |
| hook-pre-5k | 93304 → 90026 | -3.5% | 21261.5 → 21161.2 | -0.5% | 6096 → 6096 | +0.0% | 95.3 → 95.1 | -0.1% | 40874 → 41852 | +2.4% | 30224 → 29515 | -2.3% |
| hook-prompt-1k | 91183 → 37038 | -59.4% | 12058.9 → 11756.3 | -2.5% | 5366 → 5086 | -5.2% | 99.0 → 97.0 | -2.0% | 23987 → 20073 | -16.3% | 23115 → 19262 | -16.7% |
| hook-post-1k | 5350 → 5350 | +0.0% | 535.2 → 535.2 | +0.0% | 69 → 69 | +0.0% | 74.5 → 74.0 | -0.6% | 1569 → 1547 | -1.4% | 1620 → 1548 | -4.4% |
| hook-start-1k | 21575 → 21319 | -1.2% | 10833.3 → 10825.4 | -0.1% | 5539 → 5539 | +0.0% | 86.5 → 86.9 | +0.4% | 8481 → 8739 | +3.0% | 8463 → 8445 | -0.2% |
| memfile-roundtrip | 76 → 76 | -0.4% | 38.4 → 38.4 | -0.0% | 20 → 20 | +0.0% | 27.7 → 27.6 | -0.2% | 32 → 25 | -19.8% | 32 → 25 | -20.0% |
| transcript-digest | 42739 → 42739 | +0.0% | 7706.1 → 7706.1 | +0.0% | 821 → 821 | +0.0% | 24.4 → 24.3 | -0.4% | 6586 → 6548 | -0.6% | 6588 → 6614 | +0.4% |
| defang | 11 → 11 | +0.0% | 2.3 → 2.3 | +0.0% | 2 → 2 | +0.0% | 22.1 → 22.0 | -0.3% | 6 → 8 | +24.2% | 6 → 8 | +29.2% |
| daemon-frame | 116 → 116 | +0.0% | 484.3 → 484.3 | +0.0% | 226 → 226 | +0.0% | 21.6 → 21.8 | +1.0% | 243 → 263 | +8.2% | 245 → 273 | +11.4% |
| layout-1k | 50876 → 13905 | -72.7% | 10238.9 → 4562.6 | -55.4% | 411 → 411 | +0.0% | 22.5 → 22.7 | +0.9% | 132548 → 119904 | -9.5% | 135995 → 119911 | -11.8% |

The text-query and UserPromptSubmit gains come from p2b. `layout-1k` comes
from p2a. Small allocation drops on every index read come from p2c.
`q-path-5k`, `reindex-1k`, `create-1k`, `defang` and `daemon-frame` CPU
moved within their spread. Their allocation counts are flat or lower, and
none of them runs code p2a–p2c touch, except reindex, which the compact
strings change (+1.8% bytes).

## Overall: baseline → final

| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| q-filter-100 | 49400 → 15770 | -68.1% | 4486.5 → 2750.8 | -38.7% | 599 → 571 | -4.6% | 83.4 → 83.4 | -0.0% | 8585 → 5864 | -31.7% | 7757 → 5284 | -31.9% |
| q-filter-1k | 121667 → 36679 | -69.9% | 14142.0 → 11694.0 | -17.3% | 5365 → 5085 | -5.2% | 98.2 → 95.1 | -3.2% | 22207 → 16499 | -25.7% | 21339 → 15670 | -26.6% |
| q-filter-5k | 451746 → 139110 | -69.2% | 65807.5 → 60383.8 | -8.2% | 23121 → 21710 | -6.1% | 139.7 → 123.4 | -11.7% | 102078 → 80901 | -20.7% | 88728 → 64781 | -27.0% |
| q-rank-100 | 48458 → 15395 | -68.2% | 4234.2 → 2515.5 | -40.6% | 599 → 571 | -4.6% | 83.8 → 82.5 | -1.6% | 7744 → 5703 | -26.4% | 7205 → 5089 | -29.4% |
| q-rank-1k | 121430 → 36596 | -69.9% | 14806.4 → 12362.2 | -16.5% | 5365 → 5085 | -5.2% | 98.3 → 95.6 | -2.7% | 23223 → 16940 | -27.1% | 22271 → 15700 | -29.5% |
| q-rank-5k | 451753 → 139147 | -69.2% | 71383.2 → 65960.2 | -7.6% | 23149 → 21745 | -6.1% | 140.8 → 125.2 | -11.1% | 105358 → 79790 | -24.3% | 92741 → 66855 | -27.9% |
| q-path-100 | 40724 → 14150 | -65.3% | 3369.5 → 1752.9 | -48.0% | 221 → 221 | +0.1% | 81.7 → 81.2 | -0.7% | 6292 → 4439 | -29.4% | 5986 → 4138 | -30.9% |
| q-path-1k | 56392 → 26214 | -53.5% | 6373.2 → 4291.5 | -32.7% | 1653 → 1654 | +0.0% | 86.5 → 85.2 | -1.4% | 11008 → 8794 | -20.1% | 10757 → 8530 | -20.7% |
| q-path-5k | 133444 → 88717 | -33.5% | 24384.5 → 20443.3 | -16.2% | 6095 → 6095 | +0.0% | 101.2 → 93.7 | -7.4% | 41357 → 38664 | -6.5% | 34184 → 28324 | -17.1% |
| create-1k | 204912 → 201507 | -1.7% | 25459.1 → 25295.1 | -0.6% | 6427 → 6427 | +0.0% | 122.1 → 120.2 | -1.6% | 136188 → 149899 | +10.1% | 60301 → 61550 | +2.1% |
| update-1k | 129201 → 125980 | -2.5% | 19829.6 → 19664.4 | -0.8% | 5888 → 5887 | -0.0% | 122.2 → 120.1 | -1.8% | 54592 → 55555 | +1.8% | 38400 → 37513 | -2.3% |
| reindex-1k | 465288 → 328214 | -29.5% | 117342.6 → 116105.2 | -1.1% | 12798 → 12528 | -2.1% | 122.0 → 119.0 | -2.5% | 445112 → 306674 | -31.1% | 166638 → 126272 | -24.2% |
| hook-pre-1k | 59412 → 27572 | -53.6% | 7173.9 → 5024.2 | -30.0% | 1654 → 1654 | +0.0% | 90.6 → 87.6 | -3.3% | 12477 → 9692 | -22.3% | 12008 → 9230 | -23.1% |
| hook-pre-5k | 136270 → 90026 | -33.9% | 25156.4 → 21161.2 | -15.9% | 6095 → 6096 | +0.0% | 108.4 → 95.1 | -12.2% | 47081 → 41852 | -11.1% | 35781 → 29515 | -17.5% |
| hook-prompt-1k | 122346 → 37038 | -69.7% | 14233.0 → 11756.3 | -17.4% | 5366 → 5086 | -5.2% | 100.1 → 97.0 | -3.1% | 26643 → 20073 | -24.7% | 25862 → 19262 | -25.5% |
| hook-post-1k | 9412 → 5350 | -43.2% | 781.8 → 535.2 | -31.5% | 69 → 69 | +0.3% | 76.7 → 74.0 | -3.4% | 1845 → 1547 | -16.2% | 1891 → 1548 | -18.1% |
| hook-start-1k | 52100 → 21319 | -59.1% | 12986.7 → 10825.4 | -16.6% | 5538 → 5539 | +0.0% | 88.3 → 86.9 | -1.5% | 10726 → 8739 | -18.5% | 10619 → 8445 | -20.5% |
| memfile-roundtrip | 229 → 76 | -66.9% | 43.8 → 38.4 | -12.4% | 21 → 20 | -3.3% | 26.9 → 27.6 | +2.6% | 42 → 25 | -39.6% | 42 → 25 | -39.6% |
| transcript-digest | 46855 → 42739 | -8.8% | 9463.8 → 7706.1 | -18.6% | 821 → 821 | -0.0% | 24.4 → 24.3 | -0.3% | 7052 → 6548 | -7.2% | 7066 → 6614 | -6.4% |
| defang | 12 → 11 | -8.0% | 2.9 → 2.3 | -22.2% | 3 → 2 | -33.2% | 21.8 → 22.0 | +1.0% | 14 → 8 | -44.6% | 14 → 8 | -42.4% |
| daemon-frame | 116 → 116 | +0.0% | 484.3 → 484.3 | +0.0% | 226 → 226 | +0.0% | 21.3 → 21.8 | +2.3% | 242 → 263 | +8.6% | 244 → 273 | +12.2% |
| layout-1k | 50876 → 13905 | -72.7% | 10238.9 → 4562.6 | -55.4% | 411 → 411 | +0.0% | 22.2 → 22.7 | +2.2% | 121902 → 119904 | -1.6% | 123950 → 119911 | -3.3% |

## Struct layout

Sizes from `alloc_profile sizes`. They are identical before and after,
because no layout change was kept. All of these types have 8-byte
alignment.

| type | size | align |
|---|---:|---:|
| `engramdb::types::Memory` | 712 | 8 |
| `engramdb::storage::IndexForFiltering` | 400 | 8 |
| `engramdb::types::KeywordStems` | 104 | 8 |
| `engramdb::scoring::ScoreBreakdown` | 288 | 8 |
| `engramdb::retrieval::engine::ScoredMemory` | 1008 | 8 |
| `engramdb::types::Link` | 168 | 8 |
| `engramdb::types::Challenge` | 88 | 8 |
| `engramdb::types::Provenance` | 104 | 8 |
| `engramdb::types::Decay` | 64 | 8 |
| `engramdb::storage::transcripts::Event` | 88 | 8 |
| `engramdb::daemon::protocol::DaemonRequest` | 288 | 8 |
| `engramdb::daemon::protocol::DaemonOp` | 88 | 8 |
| `engramdb::daemon::protocol::DaemonResponse` | 184 | 8 |
| `engram_dashboard::layout::ForceNode` | 112 | 8 |
| `engram_dashboard::layout::ForceEdge` | 40 | 8 |

- **`ScoreBreakdown.via`** (boxing it saves 40 bytes): measured as c9 and
  rejected (see above).
- **`IndexForFiltering.keyword_stems`** (`Option<KeywordStems>`, 104 bytes):
  boxing it would save 96 bytes per row but add one allocation per row on
  every text query, the path p2b had just made allocation-free per stem. Not
  tried.
- **`Memory` (712 bytes) and `ScoredMemory`**: mostly `String`, `Vec` and
  `DateTime` fields with no padding between them. Reordering would not
  shrink them; only boxing rarely-set fields could, at one allocation each.
  Not tried.
- **Const size asserts:** none were added. No type's size is load-bearing
  for anything, and an assert on a 700-byte struct would only make adding a
  field noisier.
- **False sharing:** the shared atomics are the daemon metrics counters
  (six `AtomicU64`) and the pool round-robin `next: AtomicUsize`. Each is
  touched once per daemon request, which costs milliseconds, so contention
  on one cache line is not measurable. Checked, nothing padded.

## Verification

These were run on the final head, with ORT and the models staged as in
`.claude/CLAUDE.md`:

- `cargo fmt --all`: no changes.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`:
  clean.
- `ENGRAMDB_TEST_REQUIRE_MODELS=1 cargo nextest run --workspace --all-features`:
  3557 passed, 4 skipped (the repository's `#[ignore]` tests), 0 failed.
- No insta snapshot changed, and `text_query_output_is_pinned` and the
  layout pins pass.
- `cargo tree -p engram-cli -e normal` shows no new shipped dependency.
  `libc` is a `cfg(unix)` dev-dependency of `engram-cli` used only by the
  harness, and `compact_str` already shipped through `tokenizers`.

## Not done / future

- **`batch_to_for_filtering` is still the largest allocator** on every
  query: about a third of a 1k-memory PreToolUse hook's allocation blocks.
  The remainder is per-row JSON for `physical`, `logical`, `tags`,
  `watch_paths`, `audience` and the stems map. serde_json gives no size hint
  for arrays or maps, so each `Vec` grows from 0 to 4 to 8. Storing these
  as Arrow `List<Utf8>` columns would remove the parse entirely. That is a
  schema bump (`CURRENT_SCHEMA_VERSION`) and should be measured with
  `query-latency-profile.md` alongside.
- **Sorted stems on disk** (p2d) only helps once rows are written sorted.
  Pair it with the next schema bump that reindexes.
- **Typed transcript parse.** `parse_session` parses every record into
  `serde_json::Value`. A borrowed, typed record that ignores unknown fields
  would skip `toolUseResult`, `usage` and similar blocks that real Claude Code
  transcripts carry. The synthetic transcript has none of them, so the
  harness would under-measure this; it needs a real-transcript scenario
  first. Duplicate-key behaviour also differs (`Value` keeps the last value,
  serde derive rejects the record).
- **`list --sort type`** (`src/ops/list.rs`) calls `format!("{:?}")` twice
  per comparison. The `Debug` order of multi-word variants is not the
  `as_str` order, so a change needs an explicit order. Not on a measured
  path.
- **`update` runs `rescan_manifest_stats` and `find_memory_files` twice**
  per call (found by the earlier code review). This is I/O as much as
  allocation, and `create`/`update` CPU is too noisy in this harness to
  judge a fix.
- **Harness:** `create`/`update` should reset or compact the scratch store
  between reps, so their CPU becomes comparable.
- **`read_msg`** could parse straight from the reader's buffer when the
  frame is complete. That saves 1 of 116 allocations per frame. Not worth
  the code.
- **The MCP `defang_json`** allocates even when a value is unchanged.
  `defang_memory_text` could return `Cow`. The MCP path is not in the
  harness.
