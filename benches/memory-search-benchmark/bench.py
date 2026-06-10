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
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path
from statistics import median

BIN = os.environ["ENGRAM_BIN"]
PROJECTS_DIR = Path(os.environ["BENCH_PROJECTS_DIR"])
OUT = Path(os.environ.get("BENCH_OUT", "results.json"))
SOCKET = os.environ["ENGRAMDB_DAEMON_SOCKET"]

LOCAL_PROJECT = "web-api"  # the project used for "local" cells
ITERS = int(os.environ.get("BENCH_ITERS", "8"))
SESSIONS_LEVELS = [1, 2, 4]
OPS_LEVELS = [1, 2, 4]


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
        env = dict(os.environ)
        if in_process:
            env["ENGRAMDB_IN_PROCESS"] = "1"
        else:
            env.pop("ENGRAMDB_IN_PROCESS", None)
        self.errlog = open(f"/tmp/bench-serve-{label}.err", "w")
        self.p = subprocess.Popen(
            [BIN, "--dir", str(PROJECTS_DIR / LOCAL_PROJECT), "serve",
             "--transport", "stdio"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errlog,
            text=True, bufsize=1, env=env,
        )
        self.pid = self.p.pid
        self._id = 0

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
        self.request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "engram-bench", "version": "0"},
        })
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
# Workload definitions (mixed search + lookup)
# ----------------------------------------------------------------------------
def workload_ops(target, ids):
    """Ordered list of (tool, args) — search, lookup, search, lookup."""
    proj = "global" if target == "global" else None

    def q(args):
        a = dict(args)
        if proj:
            a["project"] = proj
        return ("query", a)

    def g(mid):
        a = {"id": mid}
        if proj:
            a["project"] = proj
        return ("get", a)

    if target == "global":
        ops = [
            q({"mode": "rank", "query": "secret handling and validation",
               "logical": ["org.security"], "max_results": 5}),
            g(ids[0]),
            q({"mode": "filter", "query": "commit message format", "max_results": 5}),
            g(ids[1] if len(ids) > 1 else ids[0]),
        ]
    else:
        ops = [
            q({"mode": "rank", "path": "src/users.rs",
               "logical": ["api.users"], "max_results": 5}),
            g(ids[0]),
            q({"mode": "filter", "query": "pagination cursor", "max_results": 5}),
            g(ids[1] if len(ids) > 1 else ids[0]),
        ]
    return ops


def fetch_ids(target, n=2):
    """Pull a few real memory ids to use for `get` lookups."""
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
def run_cell(execution, target, ops_per_iter, n_sessions, ids, daemon_pid):
    in_process = execution == "in_process"
    sessions = [McpSession(in_process, f"{execution}-{target}-{ops_per_iter}-{n_sessions}-{i}")
                for i in range(n_sessions)]
    for s in sessions:
        s.initialize()

    ops = workload_ops(target, ids)[:ops_per_iter]

    # Warmup: each session runs the full op set once. For in_process this is
    # where the embedding model loads; we record it as the "cold" first op.
    cold_lat = []
    for s in sessions:
        for (tool, args) in ops:
            dt, ok = s.call_tool(tool, args)
            if not ok:
                raise RuntimeError(f"warmup op {tool} failed (execution={execution}, target={target})")
        cold_lat.append(dt)

    # Peak RSS sampled after warmup (models are now resident).
    session_rss = [rss_kb(s.pid) for s in sessions]
    d_rss = rss_kb(daemon_pid) if (not in_process and daemon_pid) else 0
    total_rss = sum(session_rss) + d_rss

    # Timed phase: ITERS iterations. Each iteration runs all sessions in
    # parallel; each session executes ops_per_iter ops sequentially.
    iter_walls = []
    op_lats = []  # warm per-op latencies across everything
    lock = threading.Lock()

    def session_iter(s, store):
        local = []
        for (tool, args) in ops:
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
        "execution": execution,
        "target": target,
        "ops_per_iter": ops_per_iter,
        "sessions": n_sessions,
        "cold_first_op_ms": round(median(cold_lat) * 1000, 1),
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
    results = []
    for execution in ["in_process", "daemon"]:
        # The daemon's pid is rediscovered each cell (it may reap/respawn).
        for target in ["local", "global"]:
            ids = fetch_ids(target, 2)
            for ops_per_iter in OPS_LEVELS:
                for n_sessions in SESSIONS_LEVELS:
                    daemon_pid = read_daemon_pid() if execution == "daemon" else None
                    cell = run_cell(execution, target, ops_per_iter, n_sessions, ids, daemon_pid)
                    results.append(cell)
                    print(f"[{execution:11}] {target:6} ops={ops_per_iter} "
                          f"sess={n_sessions}  warm_p50={cell['warm_op_p50_ms']:7}ms  "
                          f"thru={cell['throughput_ops_s']:7}/s  "
                          f"rss_total={cell['rss_total_mb']:7}MB  "
                          f"(sess={cell['rss_sessions_total_mb']} dmn={cell['rss_daemon_mb']})")
    OUT.write_text(json.dumps(results, indent=2))
    print(f"\nWrote {len(results)} cells -> {OUT}")


def read_daemon_pid():
    """Find the running daemon pid via its status, falling back to pidfile."""
    pf = os.environ.get("BENCH_DAEMON_PIDFILE")
    if pf and Path(pf).exists():
        try:
            return int(Path(pf).read_text().strip())
        except Exception:
            return None
    return None


if __name__ == "__main__":
    main()
