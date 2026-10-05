#!/usr/bin/env python3
"""Run the memory-tool-use eval: real Claude Code + the engram plugin.

Each (case, rep) runs `claude -p` in a fresh copy of fixture/ with the seeded
memories, then grades the transcript and the store's end state (grade.py).

Output (the layout the report builder reads):

    .claude/hillclimb/memory-tool-use/<variant>/
        results.jsonl        one row per graded (case, rep), written as it finishes
        errors.jsonl         failed attempts (timeout, harness error); never in results
        traces/<id>_rep<k>.json   the conversation, report-builder shape
        raw/<id>_rep<k>.jsonl     the raw stream-json events, for gap analysis
        raw/<id>_rep<k>.diff      the workspace diff after the run
        gaps.md              written by gaps.py

Resume is idempotent at the (case, rep) key: rows already in results.jsonl
are skipped. Requires ANTHROPIC_API_KEY (or another credential `claude`
accepts) in the environment.
"""

import argparse
import hashlib
import concurrent.futures as cf
import json
import os
import random
import shutil
import sqlite3
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path

import grade

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
FLOW_DIR = REPO / ".claude" / "hillclimb" / "memory-tool-use"
# `cargo build --profile eval` (thin LTO, same behavior as release) writes
# target/eval; MTE_PROFILE=eval picks it up. Default: the release binary.
ENGRAMDB_BIN = REPO / "target" / os.environ.get("MTE_PROFILE", "release")
# Per-variant copy of everything the run reads from the repo (binary, plugin
# files, ENGRAM.md), so a rebuild during a run cannot mix two versions.
SNAPSHOT = {"bin": ENGRAMDB_BIN, "plugin": REPO, "engram_md": None}

DEBRIEF_PROMPT = (
    "Evaluation debrief, not part of the task. Do not call any tools; answer in text only.\n"
    "Looking back at this session: was there anything you wanted to do with the project's "
    "memory store (EngramDB) that its MCP tools, their parameters, the hooks' injected "
    "context, or the engramdb commands did not let you do, or made awkward? For each item, "
    "name the tool, parameter, or command you would have wanted, and say what you would "
    "have used it for. If nothing was missing, answer exactly: nothing missing."
)

PASSTHROUGH_ENV = {
    "PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "TMPDIR",
    "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "https_proxy", "http_proxy", "no_proxy",
    "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE",
    "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN",
    # Auth through a host-managed proxy (Claude Code on the web).
    "CLAUDE_SESSION_INGRESS_TOKEN_FILE", "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
}

# Grants every tool of the plugin's MCP server. A bare `mcp__*` does not.
MEMORY_SERVER_TOOLS = "mcp__plugin_engram_memory"

# Fixture variants: the same project with different seeded memories. "vague"
# gives every memory a vague title and puts the key fact in the body.
FIXTURES = {
    "default": {"seeds": "seed_memories.json", "cases": "cases.jsonl"},
    "vague": {"seeds": "seed_memories_vague.json", "cases": "cases_vague.jsonl"},
    # Harder cases for hill-climbing once the default set hit its ceiling:
    # multi-memory tasks, facts buried past the hook preview, superseded and
    # distractor memories, multi-turn sessions, facts discovered mid-task.
    "hard": {"seeds": "seed_memories_hard.json", "cases": "cases_hard.jsonl", "dir": "fixture_hard"},
}
FIXTURE = {"name": "default"}

_write_lock = threading.Lock()


def append_jsonl(path, row):
    with _write_lock:
        with open(path, "a") as f:
            f.write(json.dumps(row) + "\n")


def done_keys(results_path):
    keys = set()
    if results_path.exists():
        for line in results_path.read_text().splitlines():
            if line.strip():
                r = json.loads(line)
                keys.add((r["prompt_id"], r["rep"]))
    return keys


def sh(cmd, cwd, env, timeout=120, check=True):
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode != 0:
        raise RuntimeError(f"{cmd[0]} {cmd[1] if len(cmd) > 1 else ''} failed ({p.returncode}): {p.stderr[-2000:]}")
    return p


def case_env(tmp, args):
    # Start from an allowlist, not the parent environment: a host that runs Claude
    # Code itself (a cloud container, CI) sets variables that change behavior, e.g.
    # MCP_CONNECTION_NONBLOCKING=true starts the turn before the plugin's MCP
    # server connects, so the memory tools never appear. Each case should start
    # like a fresh local install; only auth and network settings pass through.
    env = {k: v for k, v in os.environ.items() if k in PASSTHROUGH_ENV}
    env["PATH"] = f"{SNAPSHOT['bin']}{os.pathsep}{env.get('PATH', '')}"
    env["CLAUDE_CONFIG_DIR"] = str(tmp / "claude-config")
    env["ENGRAMDB_DATA_DIR"] = str(tmp / "engram-data")
    env["ENGRAMDB_CONFIG_DIR"] = str(tmp / "engram-config")
    env["ENGRAMDB_DAEMON_SOCKET"] = str(tmp / "daemon.sock")
    env["ENGRAMDB_OFFLINE"] = "1"  # models are pre-staged; never download mid-run
    # The fixture's ops host runs in Berlin. dc-backfill-tz depends on it: the
    # backfill only works with TZ=UTC, and nothing in the repo says so.
    env["TZ"] = "Europe/Berlin"
    if args.ort_dylib:
        env["ORT_DYLIB_PATH"] = args.ort_dylib
    (tmp / "claude-config").mkdir()
    # The global config dir: registry.json and the global config.toml, which
    # is where [daemon] lives. The minimum idle timeout (60 s) lets a daemon
    # that the finally-block in run_one fails to stop reap itself soon.
    (tmp / "engram-config").mkdir()
    (tmp / "engram-config" / "config.toml").write_text("[daemon]\nidle_timeout_secs = 60\n")
    return env


def engram_md_text():
    """ENGRAM.md exactly as `engramdb setup` writes it (read from its source)."""
    src = (REPO / "crates" / "engram-cli" / "src" / "commands" / "setup.rs").read_text()
    start = src.index('const ENGRAM_MD_CONTENT: &str = r#"') + len('const ENGRAM_MD_CONTENT: &str = r#"')
    return src[start:src.index('"#;', start)]


def install_engram_md(ws):
    """Do what `engramdb setup` does for instructions, without its hooks/.mcp.json.

    Running setup itself would also write settings hooks and an .mcp.json,
    because it cannot see a --plugin-dir plugin, and the case would then load
    every hook and the MCP server twice.
    """
    claude_dir = ws / ".claude"
    claude_dir.mkdir(exist_ok=True)
    (claude_dir / "ENGRAM.md").write_text(SNAPSHOT["engram_md"] or engram_md_text())
    (claude_dir / "CLAUDE.md").write_text("@ENGRAM.md\n")


def runtime_state(ws, case_id=None):
    """Leave git-ignored state in var/ that the costly discovered cases depend on.

    None of it is in the repo, so reading the code first does not reveal it.
    - dc-seed-dev: a preview database from an older checkout. It predates
      migration 0005, so seed_dev.py fails on the missing refunds table and
      swallows the error. Only running migrate_dev.py fixes it.
    - dc-reconcile-format: the ledger's September export, in the provider's
      v1 column order. reconcile_ledger.py defaults to v2, so every invoice
      looks missing until it runs with --format v1.
    - dc-search-index-cache: token shards cached by an older tokenizer.
      build_search_index.py rejects them until it runs with --clear-cache.
    - Every other case gets the generated FX fixtures, as a checkout where
      scripts/gen_fx_fixtures.py ran once would. Only dc-fx-fixtures starts
      without them; otherwise every task that runs the tests (ds-fx-rounding,
      mt-sandbox-region) hits the same discovery.
    """
    var = ws / "var"
    var.mkdir()
    conn = sqlite3.connect(var / "dev.sqlite3")
    conn.execute("CREATE TABLE schema_migrations (name TEXT PRIMARY KEY)")
    for path in sorted((ws / "migrations").glob("*.sql"))[:4]:
        conn.executescript(path.read_text())
        conn.execute("INSERT INTO schema_migrations VALUES (?)", (path.name,))
    conn.commit()
    conn.close()
    (var / "ledger").mkdir()
    rows = [f"inv_2026_09_{i:03d},le_{9000 + i},{1000 * i}" for i in range(1, 13)]
    (var / "ledger" / "2026-09.csv").write_text("col1,col2,col3\n" + "\n".join(rows) + "\n")
    (var / "cache" / "search").mkdir(parents=True)
    for i in range(3):
        (var / "cache" / "search" / f"shard-{i:03d}.json").write_text(json.dumps({"tokenizer": 2, "tokens": []}))
    if case_id != "dc-fx-fixtures":
        subprocess.run([sys.executable, "scripts/gen_fx_fixtures.py"], cwd=ws, check=True, capture_output=True)


def setup_workspace(tmp, env, engram_md=False, case_id=None):
    ws = tmp / "ledgerline"
    shutil.copytree(HERE / FIXTURES[FIXTURE["name"]].get("dir", "fixture"), ws)
    if engram_md:
        install_engram_md(ws)
    if FIXTURE["name"] == "hard":
        runtime_state(ws, case_id)
    git = lambda *a: sh(["git", *a], ws, env)
    git("init", "-q", "-b", "main")
    git("remote", "add", "origin", "https://git.example.com/acme/ledgerline.git")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.com", "add", "-A")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.com", "commit", "-qm", "fixture")
    sh(["engramdb", "init"], ws, env)
    ids = {}
    for m in json.loads((HERE / FIXTURES[FIXTURE["name"]]["seeds"]).read_text()):
        ids[m["key"]] = seed_memory(ws, env, m, ids)
    warm_daemon(ws, env)
    # Commit the seeded store so the end-state diff shows only what the run changed.
    git("-c", "user.name=eval", "-c", "user.email=eval@example.com", "add", "-A")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.com", "commit", "-qm", "seed", "--allow-empty")
    return ws, ids


def warm_daemon(ws, env):
    """Start the embedding daemon before Claude Code starts the MCP server.

    A cold `engramdb serve` (no daemon running) needs about 3 s to answer
    `initialize`. Claude Code 2.1.x's protocol-discovery probe times out first,
    and the reconnect fails on every list request, so the session has no memory
    tools at all. That is a product bug, reported separately; the eval measures
    Claude's behavior with working tools, as with a daemon already running.
    """
    proc = subprocess.Popen(["engramdb", "serve", "--dir", "."], cwd=ws, env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    try:
        init = {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "warmup", "version": "1"}}}
        proc.stdin.write(json.dumps(init) + "\n")
        proc.stdin.flush()
        proc.stdout.readline()
    finally:
        proc.kill()
        proc.wait()


def seed_memory(ws, env, m, ids=None):
    cmd = ["engramdb", "add", "--format", "json", "--type", m["type"], "--title", m["title"],
           "--summary", m["title"], "--content", m["content"]]
    for p in m["paths"]:
        cmd += ["--physical", p]
    if m["tags"]:
        cmd += ["--tags", ",".join(m["tags"])]
    # Optional fields (hard fixture). `supersedes` names an earlier seed's key.
    for scope in m.get("logical", []):
        cmd += ["--logical", scope]
    if "criticality" in m:
        cmd += ["--criticality", str(m["criticality"])]
    if m.get("supersedes"):
        cmd += ["--supersedes", (ids or {})[m["supersedes"]]]
    if m.get("premise"):
        cmd += ["--premise", m["premise"]]
    for glob in m.get("invalidated_by", []):
        cmd += ["--invalidated-by", glob]
    out = sh(cmd, ws, env).stdout
    return json.loads(out)["message"].split()[-1]


def run_claude(prompt, ws, env, args, session_id, resume=False, extra=()):
    cmd = [
        "claude", "-p", prompt,
        "--model", args.model,
        "--output-format", "stream-json", "--verbose", "--include-hook-events",
        "--plugin-dir", str(SNAPSHOT["plugin"]),
        "--permission-mode", "acceptEdits",
        "--allowedTools", "Read", "Edit", "Write", "Glob", "Grep", "Bash", MEMORY_SERVER_TOOLS,
        "--max-budget-usd", str(args.max_budget_usd),
    ]
    cmd += ["--resume", session_id] if resume else ["--session-id", session_id]
    if args.effort:
        cmd += ["--effort", args.effort]
    cmd += list(extra)
    proc = subprocess.Popen(cmd, cwd=ws, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, start_new_session=True)
    try:
        out, err = proc.communicate(timeout=args.case_timeout)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        raise TimeoutError(f"case exceeded {args.case_timeout}s") from None
    events = [json.loads(l) for l in out.splitlines() if l.startswith("{")]
    return events, proc.returncode, err


def is_retryable(events, stderr):
    blob = (stderr or "") + json.dumps([e for e in events if e.get("type") == "result"])
    return any(s in blob for s in ("429", "529", "overloaded", "rate_limit"))


def run_one(case, rep, args, out_dir):
    tmp = Path(tempfile.mkdtemp(prefix=f"mte-{case['id']}-"))
    attempt = {"prompt_id": case["id"], "rep": rep, "model": args.model, "retries": 0}
    try:
        env = case_env(tmp, args)
        ws, ids = setup_workspace(tmp, env, args.engram_md, case['id'])
        before = grade.snapshot_store(ws, env)
        session_id = str(uuid.uuid4())
        for attempt_no in range(args.max_retries + 1):
            t0 = time.monotonic()
            events, code, stderr = run_claude(case["prompt"], ws, env, args, session_id)
            latency = time.monotonic() - t0
            result = next((e for e in reversed(events) if e.get("type") == "result"), None)
            if result and not (result.get("is_error") and is_retryable(events, stderr)):
                break
            if attempt_no == args.max_retries or not is_retryable(events, stderr):
                break
            attempt["retries"] += 1
            time.sleep(min(60, 2 ** attempt_no * 5) * (0.5 + random.random()))
            session_id = str(uuid.uuid4())
        if result is None:
            raise RuntimeError(f"no result event (exit {code}): {stderr[-1500:]}")
        # Follow-up turns of a multi-turn case: same session, one at a time.
        # A marker event lets the grader and the trace show where each starts.
        for turn in case.get("turns", []):
            t0 = time.monotonic()
            t_events, code, stderr = run_claude(turn, ws, env, args, session_id, resume=True)
            latency += time.monotonic() - t0
            if not any(e.get("type") == "result" for e in t_events):
                raise RuntimeError(f"no result event in a follow-up turn (exit {code}): {stderr[-1500:]}")
            events = events + [{"type": "turn", "prompt": turn}] + t_events
        # The first init event can predate the connection; the last one reflects it.
        init = next((e for e in reversed(events) if e.get("type") == "system" and e.get("subtype") == "init"), {})
        servers = {s["name"]: s.get("status") for s in init.get("mcp_servers", [])}
        engram = {n: s for n, s in servers.items() if "engram" in n}
        if not engram or any(s != "connected" for s in engram.values()):
            # Without the memory tools the case measures the harness, not Claude.
            raise RuntimeError(f"mcp_not_connected: {servers}")

        diff = grade.workspace_diff(ws, env)
        after = grade.snapshot_store(ws, env)
        row, trace = grade.grade_case(case, events, diff, before, after, ids, latency)

        if args.debrief and rep < args.debrief_reps:
            d_events, _, _ = run_claude(DEBRIEF_PROMPT, ws, env, args, session_id, resume=True,
                                        extra=["--disallowedTools", "Edit", "Write", "Bash", MEMORY_SERVER_TOOLS])
            d_result = next((e for e in reversed(d_events) if e.get("type") == "result"), {})
            row["meta"]["debrief"] = d_result.get("result", "")
            row["meta"]["debrief_cost_usd"] = d_result.get("total_cost_usd")
            trace.append({"role": "user", "content": "[debrief, not graded] " + DEBRIEF_PROMPT})
            trace.append({"role": "assistant", "content": row["meta"]["debrief"]})
            events = events + [{"type": "debrief", "events": d_events}]

        row["rep"] = rep
        row["meta"]["retries"] = attempt["retries"]
        stem = f"{case['id']}_rep{rep}"
        (out_dir / "traces" / f"{stem}.json").write_text(json.dumps(trace, indent=1))
        with open(out_dir / "raw" / f"{stem}.jsonl", "w") as f:
            for e in events:
                f.write(json.dumps(e) + "\n")
        (out_dir / "raw" / f"{stem}.diff").write_text(diff)
        append_jsonl(out_dir / "results.jsonl", row)
        return row
    except Exception as exc:  # recorded, never scored
        cls = "timeout" if isinstance(exc, TimeoutError) else "harness_error"
        append_jsonl(out_dir / "errors.jsonl", {**attempt, "failure_class": cls, "error": str(exc)[-2000:]})
        return None
    finally:
        # Each case spawns its own daemon (about 400 MB). Stop it explicitly
        # rather than wait 60 s for it to reap; otherwise a full run keeps
        # dozens of them alive at once.
        if "env" in locals():
            subprocess.run(["engramdb", "daemon", "stop"], env=env, capture_output=True, timeout=30)
        if not args.keep_workspaces:
            shutil.rmtree(tmp, ignore_errors=True)


def snapshot_name(args):
    """Per flow and variant: two flows run at once may share a variant name."""
    if args.flow == "memory-tool-use":
        return args.variant  # the original layout, kept so old variants resume
    return f"{args.flow.replace('/', '-')}-{args.variant}"


def take_snapshot(root, out_dir, settings):
    """Copy the binary and plugin files once per variant; reuse them on resume."""
    root.mkdir(parents=True, exist_ok=True)
    if not (root / "bin" / "engramdb").exists():
        (root / "bin").mkdir(exist_ok=True)
        shutil.copy2(ENGRAMDB_BIN / "engramdb", root / "bin" / "engramdb")
        shutil.rmtree(root / "plugin", ignore_errors=True)
        shutil.copytree(REPO / ".claude-plugin", root / "plugin" / ".claude-plugin")
        shutil.copytree(REPO / "commands", root / "plugin" / "commands")
        (root / "ENGRAM.md").write_text(engram_md_text())
    SNAPSHOT.update(bin=root / "bin", plugin=root / "plugin", engram_md=(root / "ENGRAM.md").read_text())
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()[:16]
    git = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO, capture_output=True, text=True).stdout.strip()
    build = {"git_head_at_snapshot": git, "engramdb_sha256": sha(root / "bin" / "engramdb"),
             "plugin_json_sha256": sha(root / "plugin" / ".claude-plugin" / "plugin.json"),
             "engram_md_sha256": sha(root / "ENGRAM.md"), **settings}
    path = out_dir / "build.json"
    if path.exists():
        # The commit is recorded when the snapshot is taken and kept; HEAD moving
        # afterwards is not a different build. Hashes and run settings must match.
        recorded = json.loads(path.read_text())
        same = {k: v for k, v in recorded.items() if k != "git_head_at_snapshot"}
        if same != {k: v for k, v in build.items() if k != "git_head_at_snapshot"}:
            sys.exit(f"{path} differs from the snapshot in {root}; refusing to mix builds in one variant")
        return
    path.write_text(json.dumps(build, indent=1) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--variant", default="baseline", help="output dir name: baseline or vN")
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--effort", default=None, help="pass --effort to claude; default keeps Claude Code's own default")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--cases", default="", help="comma-separated case ids (default: all)")
    ap.add_argument("--concurrency", type=int, default=3)
    ap.add_argument("--case-timeout", type=int, default=600, help="hard wall-clock ceiling per claude call, seconds")
    ap.add_argument("--max-retries", type=int, default=2)
    ap.add_argument("--max-budget-usd", type=float, default=2.0, help="per claude call")
    ap.add_argument("--no-debrief", dest="debrief", action="store_false")
    ap.add_argument("--debrief-reps", type=int, default=1,
                    help="debrief only reps below this number (default 1: rep 0 only)")
    ap.add_argument("--keep-workspaces", action="store_true")
    ap.add_argument("--snapshot-root", default=os.path.join(tempfile.gettempdir(), "mte-snapshots"),
                    help="where per-variant copies of the binary and plugin live")
    ap.add_argument("--flow", default="memory-tool-use",
                    help="results directory under .claude/hillclimb/ (the hard set uses memory-tool-use-hard)")
    ap.add_argument("--fixture", choices=sorted(FIXTURES), default="default",
                    help="seeded memories and case set (vague: facts only in memory bodies)")
    ap.add_argument("--engram-md", action="store_true",
                    help="add .claude/ENGRAM.md + @ENGRAM.md, as `engramdb setup` does (the README's recommended install)")
    ap.add_argument("--ort-dylib", default=os.environ.get("ORT_DYLIB_PATH", "/tmp/onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so"))
    args = ap.parse_args()

    if not (ENGRAMDB_BIN / "engramdb").exists():
        sys.exit(f"missing {ENGRAMDB_BIN / 'engramdb'}: run `cargo build --release -p engram-cli`")
    FIXTURE["name"] = args.fixture
    grade.set_seed_file(HERE / FIXTURES[args.fixture]["seeds"])
    cases = [json.loads(l) for l in (HERE / FIXTURES[args.fixture]["cases"]).read_text().splitlines() if l.strip()]
    if args.cases:
        want = set(args.cases.split(","))
        cases = [c for c in cases if c["id"] in want]

    flow_dir = REPO / ".claude" / "hillclimb" / args.flow
    out_dir = flow_dir / args.variant
    out_dir.mkdir(parents=True, exist_ok=True)
    take_snapshot(Path(args.snapshot_root) / snapshot_name(args), out_dir,
                  {"model": args.model, "fixture": args.fixture, "engram_md": args.engram_md})
    (out_dir / "traces").mkdir(parents=True, exist_ok=True)
    (out_dir / "raw").mkdir(exist_ok=True)
    grade.write_state(flow_dir)
    skip = done_keys(out_dir / "results.jsonl")
    todo = [(c, r) for r in range(args.reps) for c in cases if (c["id"], r) not in skip]
    print(f"{len(todo)} attempts to run ({len(skip)} already done) -> {out_dir}", flush=True)

    with cf.ThreadPoolExecutor(args.concurrency) as pool:
        futs = {pool.submit(run_one, c, r, args, out_dir): (c["id"], r) for c, r in todo}
        for fut in cf.as_completed(futs):
            cid, r = futs[fut]
            row = fut.result()
            status = "ERROR" if row is None else ("pass" if row["grade"].get("pass") else "fail")
            print(f"  {cid} rep{r}: {status}", flush=True)

    grade.print_summary(out_dir)


if __name__ == "__main__":
    main()
