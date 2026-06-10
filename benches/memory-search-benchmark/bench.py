#!/usr/bin/env python3
"""MCP-driven benchmark matrix for EngramDB memory search & lookup.

Drives real `engramdb serve` (MCP, stdio) sessions through the JSON-RPC tool
surface and measures:

  * latency  — per-op and per-iteration wall time
  * memory   — resident set size (RSS) of every session process plus the
               shared embedding daemon

across the matrix:

  execution  : in_process  vs  daemon   (shared embedding host)
  target     : local project  vs  global store
  ops/iter   : 1, 2, 4   (mixed search + lookup operations)
  sessions   : 1, 2, 4   parallel MCP sessions doing the same workload

The point of the daemon is model sharing: in_process every session loads its
own ONNX embedding model (hundreds of MB + ~240ms init); with the daemon the
model is resident once machine-wide and sessions are light clients. The matrix
makes both the latency and the memory consequences visible.

Output: a JSON results file plus a printed Markdown summary.
"""
import glob
import itertools
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path
from statistics import median

BIN = os.environ["ENGRAM_BIN"]
PROJECTS_DIR = Path(os.environ["BENCH_PROJECTS_DIR"])
DATA_DIR = Path(os.environ["ENGRAMDB_DATA_DIR"])
OUT = Path(os.environ.get("BENCH_OUT", "results.json"))
SOCKET = os.environ["ENGRAMDB_DAEMON_SOCKET"]

# Workload: "search" (query + get, lock-free reads) or "save" (create, which
# takes the per-project write lock and embeds in the background).
WORKLOAD = os.environ.get("BENCH_WORKLOAD", "search")
# For the save workload: reset the store to empty before each cell (default), or
# leave it seeded so creates insert into a pre-populated index (BENCH_SAVE_RESET=0).
SAVE_RESET = os.environ.get("BENCH_SAVE_RESET", "1") != "0"

LOCAL_PROJECT = "web-api"  # the project used for "local" cells
ITERS = int(os.environ.get("BENCH_ITERS", "8"))
SESSIONS_LEVELS = [1, 2, 4]
OPS_LEVELS = [1, 2, 4]

# Config files the MCP server reads. We keep the daemon *enabled* in config and
# toggle execution mode per session via the ENGRAMDB_IN_PROCESS env var, which
# the MCP server now honors (engram-types::in_process_override, gating
# daemon_path_enabled). Setting it forces in-process model loading; leaving it
# unset routes inference to the shared daemon.
CONFIG_FILES = [
    PROJECTS_DIR / LOCAL_PROJECT / ".engramdb" / "config.toml",
    DATA_DIR / "global" / ".engramdb" / "config.toml",
]


def write_daemon_config():
    """Ensure both stores have the daemon enabled (env var does the toggling)."""
    body = ("# EngramDB configuration (benchmark-managed)\n"
            f"[daemon]\nenabled = true\n"
            f'socket_path = "{SOCKET}"\nidle_timeout_secs = 3600\n')
    for p in CONFIG_FILES:
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body)


def find_daemon_pid():
    """Locate the running `engramdb daemon run` bound to our socket via /proc."""
    for cmdpath in glob.glob("/proc/[0-9]*/cmdline"):
        try:
            with open(cmdpath, "rb") as f:
                parts = f.read().split(b"\0")
        except OSError:
            continue
        cmd = [p.decode("utf-8", "replace") for p in parts if p]
        if any("engramdb" in c for c in cmd) and "daemon" in cmd and SOCKET in cmd:
            try:
                return int(cmdpath.split("/")[2])
            except ValueError:
                continue
    return None


def stop_daemons():
    pid = find_daemon_pid()
    while pid:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        time.sleep(0.3)
        nxt = find_daemon_pid()
        if nxt == pid:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            time.sleep(0.3)
            nxt = find_daemon_pid()
        pid = nxt


def start_daemon():
    stop_daemons()
    subprocess.Popen(
        [BIN, "daemon", "run", "--socket", SOCKET, "--idle-timeout", "3600"],
        stdout=open("/tmp/bench-daemon.log", "w"), stderr=subprocess.STDOUT,
    )
    for _ in range(50):
        r = subprocess.run([BIN, "daemon", "status", "--socket", SOCKET],
                           capture_output=True, text=True)
        if r.returncode == 0 and "running" in r.stdout:
            break
        time.sleep(0.2)
    return find_daemon_pid()


# ----------------------------------------------------------------------------
# /proc-based RSS sampling (no psutil dependency)
# ----------------------------------------------------------------------------
def rss_kb(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError):
        pass
    return 0


# ----------------------------------------------------------------------------
# Minimal MCP stdio client (newline-delimited JSON-RPC)
# ----------------------------------------------------------------------------
class McpSession:
    def __init__(self, in_process, label):
        # Execution mode is selected with ENGRAMDB_IN_PROCESS, now honored by the
        # MCP server: set -> in-process model load; unset -> shared daemon.
        env = dict(os.environ)
        if in_process:
            env["ENGRAMDB_IN_PROCESS"] = "1"
        else:
            env.pop("ENGRAMDB_IN_PROCESS", None)
        self.errlog = open(f"/tmp/bench-serve-{label}.err", "w")
        self._spawn_t0 = time.perf_counter()
        self.p = subprocess.Popen(
            [BIN, "--dir", str(PROJECTS_DIR / LOCAL_PROJECT), "serve",
             "--transport", "stdio"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errlog,
            text=True, bufsize=1, env=env,
        )
        self.pid = self.p.pid
        self._id = 0
        self.startup_ms = None  # spawn -> initialize complete (model-load cost)

    def _send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def _read_result(self, want_id):
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("server closed stdout")
            line = line.strip()
            if not line:
                continue
            msg = json.loads(line)
            if msg.get("id") == want_id:
                return msg

    def request(self, method, params):
        self._id += 1
        rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        return self._read_result(rid)

    def initialize(self):
        # In-process serve loads the embedding model during startup/handshake,
        # so spawn->initialize wall time captures the per-session model-load
        # cost the daemon amortizes away.
        self.request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "engram-bench", "version": "0"},
        })
        self.startup_ms = (time.perf_counter() - self._spawn_t0) * 1000
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call_tool(self, name, arguments):
        """Return (latency_seconds, ok)."""
        t0 = time.perf_counter()
        resp = self.request("tools/call", {"name": name, "arguments": arguments})
        dt = time.perf_counter() - t0
        ok = "error" not in resp and not resp.get("result", {}).get("isError", False)
        return dt, ok

    def close(self):
        try:
            self.p.stdin.close()
            self.p.terminate()
            self.p.wait(timeout=10)
        except Exception:
            self.p.kill()
        self.errlog.close()


# ----------------------------------------------------------------------------
# Workload definitions
#
# Each op is a zero-arg factory returning (tool, args). Search ops are static;
# save ops mint fresh unique content per call (so embeds are distinct and there
# are no id collisions) from a process-global counter.
# ----------------------------------------------------------------------------
def search_ops(target, ids):
    """Mixed search + lookup: search, lookup, search, lookup."""
    proj = "global" if target == "global" else None

    def q(args):
        a = dict(args)
        if proj:
            a["project"] = proj
        return lambda: ("query", a)

    def g(mid):
        a = {"id": mid}
        if proj:
            a["project"] = proj
        return lambda: ("get", a)

    if target == "global":
        return [
            q({"mode": "rank", "query": "secret handling and validation",
               "logical": ["org.security"], "max_results": 5}),
            g(ids[0]),
            q({"mode": "filter", "query": "commit message format", "max_results": 5}),
            g(ids[1] if len(ids) > 1 else ids[0]),
        ]
    return [
        q({"mode": "rank", "path": "src/users.rs",
           "logical": ["api.users"], "max_results": 5}),
        g(ids[0]),
        q({"mode": "filter", "query": "pagination cursor", "max_results": 5}),
        g(ids[1] if len(ids) > 1 else ids[0]),
    ]


SAVE_TYPES = ["decision", "convention", "hazard", "context", "intent",
              "relationship", "debug", "preference"]
SAVE_SENTENCES = [
    "Use cursor pagination instead of offset for large list endpoints.",
    "Validate and sanitize all external input at the service boundary.",
    "Rotate signing keys every 90 days with a 7-day overlap window.",
    "Store monetary amounts as integer minor units, never floats.",
    "Partition fact tables by month and order by event time.",
    "Reject webhooks whose signature header fails verification.",
    "Prefer immutable images over patching running containers.",
    "Treat refresh-token reuse as compromise and revoke the family.",
    "Index aliases enable zero-downtime reindexing on writes.",
    "Batch inference for nightly scoring favors throughput over latency.",
    "Memorize that schema drift upstream silently drops columns on load.",
    "Constant-time comparison avoids timing leaks on credential checks.",
]
_save_seq = itertools.count()
_save_lock = threading.Lock()


def save_ops(target, ops_per_iter):
    """ops_per_iter create-op factories, each minting unique content."""
    proj = "global" if target == "global" else None

    def make():
        with _save_lock:
            n = next(_save_seq)
        args = {
            "type": SAVE_TYPES[n % len(SAVE_TYPES)],
            "title": f"bench save {n}",
            "summary": f"benchmark generated memory {n}",
            "content": f"Record {n}: {SAVE_SENTENCES[n % len(SAVE_SENTENCES)]} (entry {n})",
            "logical": ["bench.save"],
            "tags": ["bench", "save"],
        }
        if proj:
            args["project"] = proj
        return ("create", args)

    return [make] * ops_per_iter


def make_workload(target, ids, ops_per_iter):
    if WORKLOAD == "save":
        return save_ops(target, ops_per_iter)
    return search_ops(target, ids)[:ops_per_iter]


def reset_save_store():
    """Wipe the save targets so every cell starts from an empty store (keeps
    create latency comparable across cells instead of growing with the index)."""
    shutil.rmtree(PROJECTS_DIR / LOCAL_PROJECT / ".engramdb", ignore_errors=True)
    shutil.rmtree(DATA_DIR / "global", ignore_errors=True)
    shutil.rmtree(DATA_DIR / "projects", ignore_errors=True)
    subprocess.run([BIN, "--quiet", "--dir", str(PROJECTS_DIR / LOCAL_PROJECT),
                    "init", "--no-embeddings"], capture_output=True)
    write_daemon_config()


def fetch_ids(target, n=2):
    """Pull a few real memory ids to use for `get` lookups (search workload)."""
    args = [BIN, "--json"]
    if target == "global":
        args += ["list", "--global", "-n", str(n)]
    else:
        args += ["--dir", str(PROJECTS_DIR / LOCAL_PROJECT), "list", "-n", str(n)]
    r = subprocess.run(args, capture_output=True, text=True)
    data = json.loads(r.stdout)
    mems = data if isinstance(data, list) else data.get("memories", data.get("results", []))
    ids = [m["id"] for m in mems][:n]
    if not ids:
        raise RuntimeError(f"no ids found for {target}: {r.stdout[:300]}")
    return ids


# ----------------------------------------------------------------------------
# One matrix cell
# ----------------------------------------------------------------------------
def run_cell(execution, target, ops_per_iter, n_sessions, ids):
    in_process = execution == "in_process"
    # Save workload: optionally start each cell from an empty store so create
    # latency stays comparable across cells (the index grows within a cell, not
    # across them). Disable to measure creates into a pre-seeded large store.
    if WORKLOAD == "save" and SAVE_RESET:
        reset_save_store()
    sessions = [McpSession(in_process, f"{execution}-{target}-{ops_per_iter}-{n_sessions}-{i}")
                for i in range(n_sessions)]
    for s in sessions:
        s.initialize()

    ops = make_workload(target, ids, ops_per_iter)

    # Warmup: each session runs the full op set once. The embedding model loads
    # in a background task at serve startup; the first model-dependent op blocks
    # until it is ready, so spawn->first-result (ttfr) captures the per-session
    # model-load cost that the daemon amortizes. Cleanest at sessions=1 (no
    # overlap between sessions' concurrent background loads).
    cold_op_lat = []
    ttfr = []
    for s in sessions:
        first = True
        for op in ops:
            tool, args = op()
            dt, ok = s.call_tool(tool, args)
            if first:
                ttfr.append((time.perf_counter() - s._spawn_t0) * 1000)
                cold_op_lat.append(dt * 1000)
                first = False
            if not ok:
                raise RuntimeError(f"warmup op {tool} failed (execution={execution}, target={target})")

    # Peak RSS sampled after warmup (models are now resident). For daemon mode
    # the daemon pid is re-discovered here in case it respawned.
    session_rss = [rss_kb(s.pid) for s in sessions]
    daemon_pid = None if in_process else find_daemon_pid()
    d_rss = rss_kb(daemon_pid) if daemon_pid else 0
    total_rss = sum(session_rss) + d_rss

    # Timed phase: ITERS iterations. Each iteration runs all sessions in
    # parallel; each session executes ops_per_iter ops sequentially.
    iter_walls = []
    op_lats = []  # warm per-op latencies across everything
    lock = threading.Lock()

    def session_iter(s, store):
        local = []
        for op in ops:
            tool, args = op()
            dt, ok = s.call_tool(tool, args)
            local.append(dt)
            if not ok:
                raise RuntimeError("timed op failed")
        with lock:
            store.extend(local)

    for _ in range(ITERS):
        store = []
        threads = [threading.Thread(target=session_iter, args=(s, store)) for s in sessions]
        t0 = time.perf_counter()
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        iter_walls.append(time.perf_counter() - t0)
        op_lats.extend(store)

    for s in sessions:
        s.close()

    total_ops = ITERS * n_sessions * ops_per_iter
    wall_total = sum(iter_walls)
    return {
        "workload": WORKLOAD,
        "save_reset": SAVE_RESET if WORKLOAD == "save" else None,
        "execution": execution,
        "target": target,
        "ops_per_iter": ops_per_iter,
        "sessions": n_sessions,
        "session_startup_ms": round(median([s.startup_ms for s in sessions]), 1),
        "time_to_first_result_ms": round(median(ttfr), 1),
        "cold_first_op_ms": round(median(cold_op_lat), 1),
        "warm_op_p50_ms": round(median(op_lats) * 1000, 2),
        "warm_op_p95_ms": round(sorted(op_lats)[int(len(op_lats) * 0.95) - 1] * 1000, 2),
        "iter_wall_p50_ms": round(median(iter_walls) * 1000, 2),
        "throughput_ops_s": round(total_ops / wall_total, 1),
        "rss_per_session_mb": [round(r / 1024, 1) for r in session_rss],
        "rss_sessions_total_mb": round(sum(session_rss) / 1024, 1),
        "rss_daemon_mb": round(d_rss / 1024, 1),
        "rss_total_mb": round(total_rss / 1024, 1),
    }


def main():
    # Daemon stays enabled in config; ENGRAMDB_IN_PROCESS toggles mode per session.
    write_daemon_config()
    results = []
    for execution in ["in_process", "daemon"]:
        # Bring the daemon to the matching state: absent for in-process (the env
        # var also stops serve from spawning one), running for daemon mode.
        if execution == "in_process":
            stop_daemons()
        else:
            pid = start_daemon()
            # Pre-warm: force the daemon to load its model once so per-session
            # ttfr reflects connecting to an already-warm daemon (steady state).
            subprocess.run([BIN, "--dir", str(PROJECTS_DIR / LOCAL_PROJECT),
                            "query", "--mode", "filter", "--query", "warmup",
                            "-n", "1"], capture_output=True, text=True)
            print(f"==> daemon for daemon-mode cells: pid {pid} (pre-warmed)")
        for target in ["local", "global"]:
            # Save workload mints its own content; only search needs real ids.
            ids = None if WORKLOAD == "save" else fetch_ids(target, 2)
            for ops_per_iter in OPS_LEVELS:
                for n_sessions in SESSIONS_LEVELS:
                    cell = run_cell(execution, target, ops_per_iter, n_sessions, ids)
                    results.append(cell)
                    print(f"[{execution:11}] {target:6} ops={ops_per_iter} "
                          f"sess={n_sessions}  ttfr={cell['time_to_first_result_ms']:8}ms "
                          f"warm_p50={cell['warm_op_p50_ms']:7}ms  "
                          f"thru={cell['throughput_ops_s']:7}/s  "
                          f"rss_total={cell['rss_total_mb']:8}MB  "
                          f"(sess={cell['rss_sessions_total_mb']} dmn={cell['rss_daemon_mb']})")
    stop_daemons()
    OUT.write_text(json.dumps(results, indent=2))
    print(f"\nWrote {len(results)} cells -> {OUT}")


if __name__ == "__main__":
    main()
