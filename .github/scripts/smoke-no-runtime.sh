#!/usr/bin/env bash
# Run a built `engramdb` binary through the paths that load an ONNX model and
# fail if any of them kills the process.
#
#   smoke-no-runtime.sh <path-to-engramdb> [--require-missing-runtime] [<expected-version>]
#
# Why this exists. Under the default `load-dynamic` feature, `ort`'s own
# loader panics when libonnxruntime is missing, and the release profile is
# `panic = "abort"`. So a model loader that forgets the pre-flight probe
# (`engram_models::ensure_onnx_runtime()`) does not degrade to keyword
# search: it kills the process with SIGABRT. A dev-profile test cannot see
# that (the panic unwinds, and tokio turns a panic inside `spawn_blocking`
# into an error the caller handles), which is why this script is run against
# a binary built with `panic = "abort"`: ci.yml's `missing-runtime` job, and
# every release binary in release.yml.
#
# What it checks:
#   - every command exits with a code from the short list it is allowed
#     (so a signal, a panic's 101, or Windows' fast-fail code all fail it,
#     whatever the platform reports them as);
#   - with a missing runtime the documented fallback really happens: `add`
#     stores the memory and `query` finds it by keyword;
#   - the config this script writes was accepted (a rejected config file is
#     ignored with a warning, which would silently test the defaults).
#
# What it deliberately does NOT do:
#   - set ENGRAMDB_OFFLINE or redirect the model cache. The loaders check
#     "is the model cached?" right after the runtime probe, so with an empty
#     cache and offline set, a loader that skipped the probe would stop at
#     "not cached" and never reach `ort`. With the real cache, the only
#     missing thing is the runtime.
#
# `--require-missing-runtime`: fail unless `doctor` reports the runtime as not
# found, and unless the model loaders report it too. Without the flag a found
# runtime is only a notice (a release runner may carry one), and the commands
# must still succeed.
set -euo pipefail

bin=""
require_missing=0
expected_version=""
for arg in "$@"; do
  case "$arg" in
    --require-missing-runtime) require_missing=1 ;;
    -*) echo "unknown option: $arg" >&2; exit 2 ;;
    *)
      if [ -z "$bin" ]; then bin=$arg
      elif [ -z "$expected_version" ]; then expected_version=$arg
      else echo "unexpected argument: $arg" >&2; exit 2
      fi ;;
  esac
done
if [ -z "$bin" ]; then
  echo "usage: $0 <path-to-engramdb> [--require-missing-runtime] [<expected-version>]" >&2
  exit 2
fi
if [ ! -f "$bin" ]; then
  echo "::error::no binary at $bin" >&2
  exit 2
fi
# Absolute, because every command below runs from a scratch project directory.
bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"

work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/engramdb-smoke.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/data" "$work/config" "$work/runtime" "$work/project"
chmod 700 "$work/runtime"

# Nothing here may touch the runner's (or a developer's) real stores, registry
# or daemon. The model cache is left alone on purpose; see the header.
export ENGRAMDB_DATA_DIR="$work/data"
export ENGRAMDB_CONFIG_DIR="$work/config"
export ENGRAMDB_REGISTRY_PATH="$work/registry.json"
export ENGRAMDB_IN_PROCESS=1
export ENGRAMDB_DAEMON_SOCKET="$work/runtime/d.sock"
# `auto` falls back to Ollama when ONNX is unavailable; `onnx` keeps every
# attempt on the loader under test.
export ENGRAMDB_EMBEDDING_BACKEND=onnx
export RUST_BACKTRACE=0
export NO_COLOR=1
unset ENGRAMDB_OFFLINE ENGRAMDB_MODEL_CACHE_DIR CLAUDE_SESSION_ID MCP_SESSION_ID

if [ "$require_missing" = 1 ] && [ -n "${ORT_DYLIB_PATH:-}" ]; then
  echo "::error::ORT_DYLIB_PATH is set ($ORT_DYLIB_PATH); the missing-runtime checks would not test anything." >&2
  exit 1
fi

failures=0
out="$work/stdout"
err="$work/stderr"
all_err="$work/all-stderr"
: > "$all_err"

fail() {
  echo "::error::$*" >&2
  failures=$((failures + 1))
}

# run "<allowed exit codes, space separated>" "<label>" <args to engramdb...>
# Reads stdin from $stdin_file when set. Leaves stdout in $out, stderr in $err.
stdin_file=""
run() {
  local allowed=$1 label=$2 code=0
  shift 2
  echo "--- engramdb $label"
  if [ -n "$stdin_file" ]; then
    "$bin" "$@" < "$stdin_file" > "$out" 2> "$err" || code=$?
  else
    "$bin" "$@" < /dev/null > "$out" 2> "$err" || code=$?
  fi
  cat "$err" >> "$all_err"
  local ok=0 a
  for a in $allowed; do
    [ "$code" = "$a" ] && ok=1
  done
  if [ "$ok" = 1 ]; then
    echo "    exit $code"
  else
    # 134 = SIGABRT (128 + 6): what `panic = "abort"` produces on Unix.
    fail "\`engramdb $label\` exited $code (allowed: $allowed). A code of 134 or another value above 128 means the process was killed by a signal; with a missing ONNX Runtime that is a model loader that did not call ensure_onnx_runtime() first."
    echo "    stdout:"; sed 's/^/      /' "$out"
    echo "    stderr:"; sed 's/^/      /' "$err"
  fi
  # A config file that fails to parse is ignored with a warning and every
  # setting falls back to its default, which would quietly turn NLI off.
  if grep -q 'ignoring config file' "$err"; then
    fail "the config this script writes was rejected; update the [nli] block below to match crates/engram-types/src/config.rs (NliConfig)."
    sed 's/^/      /' "$err"
  fi
}

cd "$work/project"

# 1. Starts at all (no load-time dependency on the runtime), right version.
run "0" "--version" --version
version_line=$(head -n 1 "$out")
echo "    $version_line"
if [ -n "$expected_version" ] && [ "$version_line" != "engramdb $expected_version" ]; then
  fail "--version printed '$version_line', expected 'engramdb $expected_version'"
fi

# 2. doctor reports instead of dying, with and without a store. Exit 1 is
#    doctor's own "a check failed" (here: no store yet). The runtime row is
#    only printed once a store exists, so it is read from the second run.
run "0 1" "doctor (before init)" --format plain doctor
run "0" "init" --format plain init
run "0 1" "doctor" --format plain doctor
runtime_missing=0
if grep -Eq 'ONNX Runtime.*not found' "$out"; then
  runtime_missing=1
  echo "    doctor: ONNX Runtime not found (the case under test)"
else
  if [ "$require_missing" = 1 ]; then
    fail "doctor did not report the ONNX Runtime as not found; this run would not test the missing-runtime path."
    sed 's/^/      /' "$out"
  else
    echo "::notice::an ONNX Runtime was found on this machine; the commands below ran with it, so this run did not exercise the missing-runtime fallback."
  fi
fi

# 3. A store with every ONNX-backed subsystem switched on. Embeddings (the
#    `onnx` backend forced above), the reranker and the T5 title model are on
#    by default; NLI is the one that needs a config entry. All five fields are
#    required by `NliConfig`; `run` fails if the file is rejected.
cat >> .engramdb/config.toml <<'TOML'

[nli]
enabled = true
model = "Xenova/nli-deberta-v3-xsmall"
contradiction_threshold = 0.7
max_comparisons = 10
similarity_threshold = 0.3
TOML

# 4. The write path: embedding + T5 title + NLI on create.
run "0" "add" --format plain add -t convention \
  -s "Deploys use the zebrafish pipeline" \
  "Run the zebrafish pipeline before every deploy."
id=$(sed -n 's/^Created memory \([0-9a-f-]\{36\}\).*$/\1/p' "$out" | head -n 1)
if [ -z "$id" ]; then
  fail "add did not print 'Created memory <id>'"
  sed 's/^/      /' "$out"
fi
if [ "$runtime_missing" = 1 ] && ! grep -Eq 'ONNX Runtime.*not found' "$err"; then
  fail "add did not report the missing runtime on stderr: no model loader reached the runtime probe, so this run tested nothing."
  sed 's/^/      /' "$err"
fi

# 5. The read path: query embedding + reranker, in both modes. The memory
#    must come back either way (keyword fallback when there are no vectors).
for mode in filter rank; do
  run "0" "query --mode $mode" --format plain query --mode "$mode" zebrafish
  if ! grep -q 'zebrafish' "$out"; then
    fail "query --mode $mode did not return the memory just added"
    sed 's/^/      /' "$out"
  fi
done

if [ -n "$id" ]; then
  run "0" "get" --format plain get "$id"
  # 6. Re-embed on update.
  run "0" "update" --format plain update "$id" -c "Run the zebrafish pipeline twice before every deploy."
fi

# 7. Rebuild paths. `--embeddings-only` refuses (exit 1) when no embedding
#    provider is available; that is the documented behaviour, not a crash.
run "0" "reindex" --format plain reindex
run "0 1" "reindex --embeddings-only" --format plain reindex --embeddings-only
run "0" "stats" --format plain stats
run "0 1" "doctor (after add)" --format plain doctor

# 8. The UserPromptSubmit hook loads the embedding model in the hook process
#    (no daemon here). A hook must exit 0 whatever happens: Claude Code treats
#    anything else as a failed hook.
printf '%s\n' '{"session_id":"smoke-session","hook_event_name":"UserPromptSubmit","prompt":"how does the zebrafish pipeline work?"}' > "$work/hook.json"
stdin_file="$work/hook.json"
run "0" "hook user-prompt-submit" hook user-prompt-submit
stdin_file=""

echo
if [ "$failures" -gt 0 ]; then
  echo "::error::$failures smoke check(s) failed for $bin"
  exit 1
fi
if [ "$runtime_missing" = 1 ]; then
  echo "OK: $bin starts, reports the missing ONNX Runtime and degrades without aborting."
else
  echo "OK: $bin runs every smoke command (with an ONNX Runtime present)."
fi
