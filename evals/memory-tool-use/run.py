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
import concurrent.futures as cf
import json
import os
import random
import shutil
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
ENGRAMDB_BIN = REPO / "target" / "release"

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
    env["PATH"] = f"{ENGRAMDB_BIN}{os.pathsep}{env.get('PATH', '')}"
    env["CLAUDE_CONFIG_DIR"] = str(tmp / "claude-config")
    env["ENGRAMDB_DATA_DIR"] = str(tmp / "engram-data")
    env["ENGRAMDB_CONFIG_DIR"] = str(tmp / "engram-config")
    env["ENGRAMDB_DAEMON_SOCKET"] = str(tmp / "daemon.sock")
    env["ENGRAMDB_OFFLINE"] = "1"  # models are pre-staged; never download mid-run
    if args.ort_dylib:
        env["ORT_DYLIB_PATH"] = args.ort_dylib
    (tmp / "claude-config").mkdir()
    cfg = tmp / "engram-config"
    cfg.mkdir()
    # The daemon stays on (the shipped default) but reaps soon after the case ends.
    (cfg / "config.toml").write_text("[daemon]\nidle_timeout_secs = 30\n")
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
    (claude_dir / "ENGRAM.md").write_text(engram_md_text())
    (claude_dir / "CLAUDE.md").write_text("@ENGRAM.md\n")


def setup_workspace(tmp, env, engram_md=False):
    ws = tmp / "ledgerline"
    shutil.copytree(HERE / "fixture", ws)
    if engram_md:
        install_engram_md(ws)
    git = lambda *a: sh(["git", *a], ws, env)
    git("init", "-q", "-b", "main")
    git("remote", "add", "origin", "https://git.example.com/acme/ledgerline.git")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.com", "add", "-A")
    git("-c", "user.name=eval", "-c", "user.email=eval@example.com", "commit", "-qm", "fixture")
    sh(["engramdb", "init"], ws, env)
    ids = {}
    for m in json.loads((HERE / "seed_memories.json").read_text()):
        ids[m["key"]] = seed_memory(ws, env, m)
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


def seed_memory(ws, env, m):
    cmd = ["engramdb", "add", "--format", "json", "--type", m["type"], "--title", m["title"],
           "--summary", m["title"], "--content", m["content"]]
    for p in m["paths"]:
        cmd += ["--physical", p]
    if m["tags"]:
        cmd += ["--tags", ",".join(m["tags"])]
    out = sh(cmd, ws, env).stdout
    return json.loads(out)["message"].split()[-1]


def run_claude(prompt, ws, env, args, session_id, resume=False, extra=()):
    cmd = [
        "claude", "-p", prompt,
        "--model", args.model,
        "--output-format", "stream-json", "--verbose", "--include-hook-events",
        "--plugin-dir", str(REPO),
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
        ws, ids = setup_workspace(tmp, env, args.engram_md)
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
        if not args.keep_workspaces:
            shutil.rmtree(tmp, ignore_errors=True)


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
    ap.add_argument("--engram-md", action="store_true",
                    help="add .claude/ENGRAM.md + @ENGRAM.md, as `engramdb setup` does (the README's recommended install)")
    ap.add_argument("--ort-dylib", default=os.environ.get("ORT_DYLIB_PATH", "/tmp/onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so"))
    args = ap.parse_args()

    if not (ENGRAMDB_BIN / "engramdb").exists():
        sys.exit(f"missing {ENGRAMDB_BIN / 'engramdb'}: run `cargo build --release -p engram-cli`")
    cases = [json.loads(l) for l in (HERE / "cases.jsonl").read_text().splitlines() if l.strip()]
    if args.cases:
        want = set(args.cases.split(","))
        cases = [c for c in cases if c["id"] in want]

    out_dir = FLOW_DIR / args.variant
    (out_dir / "traces").mkdir(parents=True, exist_ok=True)
    (out_dir / "raw").mkdir(exist_ok=True)
    grade.write_state(FLOW_DIR)
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
