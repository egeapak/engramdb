#!/usr/bin/env bash
# Orchestrate the EngramDB memory-search benchmark end to end.
#
#   1. isolate all EngramDB state under a scratch dir (never touches real data)
#   2. generate 10 projects x 10 memories + 10 global memories
#   3. start a shared embedding daemon (known pid + socket)
#   4. run the MCP-driven matrix benchmark
#   5. render a Markdown report
#
# Requires a release binary at target/release/engramdb. The ONNX runtime build
# workaround (ORT_STRATEGY/ORT_LIB_LOCATION) only matters at build time; at run
# time the staged model cache under ~/.cache/engramdb is used.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"

export ENGRAM_BIN="${ENGRAM_BIN:-$REPO/target/release/engramdb}"
SCRATCH="${BENCH_SCRATCH:-/tmp/engram-bench}"
export ENGRAMDB_DATA_DIR="$SCRATCH/data"
export ENGRAMDB_CONFIG_DIR="$SCRATCH/config"
export BENCH_PROJECTS_DIR="$SCRATCH/projects"
export ENGRAMDB_DAEMON_SOCKET="$SCRATCH/engram-daemon.sock"
export BENCH_DAEMON_PIDFILE="$SCRATCH/daemon.pid"
export BENCH_OUT="${BENCH_OUT:-$HERE/results.json}"
export BENCH_ITERS="${BENCH_ITERS:-8}"

mkdir -p "$ENGRAMDB_DATA_DIR" "$ENGRAMDB_CONFIG_DIR" "$BENCH_PROJECTS_DIR"

DAEMON_PROC=""
start_daemon() {
  stop_daemon
  "$ENGRAM_BIN" daemon run --socket "$ENGRAMDB_DAEMON_SOCKET" --idle-timeout 3600 \
    >"$SCRATCH/daemon.log" 2>&1 &
  DAEMON_PROC=$!
  echo "$DAEMON_PROC" > "$BENCH_DAEMON_PIDFILE"
  # Wait for the socket to appear / daemon to answer.
  for _ in $(seq 1 50); do
    if "$ENGRAM_BIN" daemon status --socket "$ENGRAMDB_DAEMON_SOCKET" >/dev/null 2>&1; then
      echo "==> Daemon up (pid $DAEMON_PROC)"
      return 0
    fi
    sleep 0.2
  done
  echo "!! Daemon failed to start; see $SCRATCH/daemon.log" >&2
  return 1
}
stop_daemon() {
  if [[ -f "$BENCH_DAEMON_PIDFILE" ]]; then
    kill "$(cat "$BENCH_DAEMON_PIDFILE")" 2>/dev/null || true
    rm -f "$BENCH_DAEMON_PIDFILE"
  fi
}
trap stop_daemon EXIT

echo "==> Binary: $ENGRAM_BIN"
"$ENGRAM_BIN" --version

# --- 1+2. Generate dataset (idempotent unless BENCH_REGEN=1) ---
if [[ ! -f "$BENCH_PROJECTS_DIR/manifest.json" || "${BENCH_REGEN:-0}" == "1" ]]; then
  echo "==> Generating dataset (daemon-assisted)..."
  start_daemon
  python3 "$HERE/gen_data.py"
else
  echo "==> Dataset already present (set BENCH_REGEN=1 to rebuild)."
fi

# --- 3. Fresh daemon for the benchmark proper ---
echo "==> Starting benchmark daemon..."
start_daemon

# --- 4. Run the matrix ---
echo "==> Running benchmark matrix (ITERS=$BENCH_ITERS)..."
python3 "$HERE/bench.py"

# --- 5. Report ---
echo "==> Rendering report..."
python3 "$HERE/report.py" "$BENCH_OUT" > "$HERE/RESULTS.md"
echo "==> Wrote $HERE/RESULTS.md"
cat "$HERE/RESULTS.md"
