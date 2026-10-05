"""Grading for the memory-tool-use eval.

Every check is a program; there is no LLM judge. Each property is its own
metric so one score cannot hide another:

    query_before_act     an EngramDB query/get/list call comes before the
                         final answer, or before the first Edit/Write
    fact_used            the final answer or the workspace diff contains an
                         expected string (passes when a hook supplied the fact)
    explicit_create      a new or changed memory contains a create term
    implicit_capture     same check; a rate only, never part of `pass`
    no_false_create      no new memory appeared where none was expected
    revise               challenge/update targeted the contradicted memory
    no_spurious_revise   no challenge/update where nothing was contradicted
    consulted_before_act query_before_act, or a hook injected the target
                         memory's title and body
    pass                 every applicable metric above passed, except
                         implicit_capture and query_before_act

A metric is omitted from a row when it does not apply to the case.
"""

import hashlib
import json
import math
import re
import subprocess
from pathlib import Path

CONSULT = {"query", "get", "list"}
# Memory-changing calls that revise what a memory says. `verify` confirms a
# memory and `compress_apply` merges, so neither revises. `create` revises only
# with a non-empty `supersedes` (it closes the old memory's validity window).
REVISE = {"challenge", "update", "resolve", "delete"}
EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
MEMORY_TOOL = re.compile(r"^mcp__(?P<server>[^_].*?)__(?P<op>[a-z_]+)$")

METRICS = [
    {"id": "pass", "label": "pass", "kind": "binary"},
    {"id": "consulted_before_act", "label": "consulted", "kind": "binary"},
    {"id": "query_before_act", "label": "query first", "kind": "binary"},
    {"id": "fact_used", "label": "fact used", "kind": "binary"},
    {"id": "explicit_create", "label": "expl. create", "kind": "binary"},
    {"id": "implicit_capture", "label": "impl. capture", "kind": "binary"},
    {"id": "costly_capture", "label": "costly capture", "kind": "binary"},
    {"id": "no_false_create", "label": "no false create", "kind": "binary"},
    {"id": "revise", "label": "revise", "kind": "binary"},
    {"id": "no_spurious_revise", "label": "no spur. revise", "kind": "binary"},
    {"id": "no_stale_fact", "label": "no stale fact", "kind": "binary"},
]
PERF_FIELDS = [
    {"id": "memory_calls", "label": "memory calls"},
    {"id": "cost_usd", "label": "cost", "unit": "$"},
    {"id": "latency_s", "label": "latency", "unit": "s"},
    {"id": "turns", "label": "turns"},
    {"id": "hook_hit", "label": "hook hit"},
    {"id": "in_tokens", "label": "in tok"},
    {"id": "out_tokens", "label": "out tok"},
    {"id": "wrote_auto_memory", "label": "auto-mem write"},
    {"id": "failed_tool_calls", "label": "failed calls"},
]


def memory_op(tool_name):
    """Return the EngramDB operation for an MCP tool name, else None."""
    m = MEMORY_TOOL.match(tool_name or "")
    if m and ("engram" in m["server"] or "memory" in m["server"]):
        return m["op"]
    return None


# ---------------------------------------------------------------- end state

def _memory_files(ws, env):
    roots = [Path(ws) / ".engramdb", Path(env["ENGRAMDB_DATA_DIR"])]
    for root in roots:
        if root.exists():
            for p in root.rglob("*.md"):
                if "memories" in p.parts or "personal" in p.parts:
                    yield p


def snapshot_store(ws, env):
    """Map every memory file to (sha256, text)."""
    snap = {}
    for p in _memory_files(ws, env):
        text = p.read_text(errors="replace")
        snap[str(p)] = (hashlib.sha256(text.encode()).hexdigest(), text)
    return snap


def workspace_diff(ws, env):
    """The run's changes, including new files, excluding the store."""
    subprocess.run(["git", "add", "-A"], cwd=ws, env=env, capture_output=True)
    p = subprocess.run(["git", "diff", "--cached", "--", ".", ":(exclude).engramdb"],
                       cwd=ws, env=env, capture_output=True, text=True)
    return p.stdout


# ---------------------------------------------------------------- transcript

def parse_events(events):
    """Flatten stream-json events into ordered steps and side metrics."""
    steps, hooks, init = [], [], {}
    results = {}
    for e in events:
        t, sub = e.get("type"), e.get("subtype", "")
        if t == "system" and sub == "init":
            init = e
        elif t == "system" and "hook" in sub:
            hooks.append(e)
        elif t == "assistant":
            for b in e.get("message", {}).get("content", []):
                if b.get("type") == "tool_use":
                    steps.append({"kind": "tool", "id": b["id"], "name": b["name"], "input": b.get("input", {})})
                elif b.get("type") == "text":
                    steps.append({"kind": "text", "text": b["text"]})
                elif b.get("type") == "thinking" and b.get("thinking"):
                    steps.append({"kind": "thinking", "text": b["thinking"]})
        elif t == "turn":
            steps.append({"kind": "user", "text": e["prompt"]})
        elif t == "user":
            content = e.get("message", {}).get("content", [])
            for b in content if isinstance(content, list) else []:
                if b.get("type") == "tool_result":
                    c = b.get("content")
                    refs = []
                    if isinstance(c, list):
                        refs = [x.get("tool_name", "") for x in c
                                if isinstance(x, dict) and x.get("type") == "tool_reference"]
                        c = "\n".join(x.get("text", "") for x in c if isinstance(x, dict))
                    results[b["tool_use_id"]] = {"text": c or "", "is_error": bool(b.get("is_error")),
                                                 "refs": refs}
    for s in steps:
        if s["kind"] == "tool":
            s["result"] = results.get(s["id"], {"text": "", "is_error": False, "refs": []})
    final = next((e for e in reversed(events) if e.get("type") == "result"), {})
    return steps, hooks, init, final


def to_trace(steps, hooks, prompt):
    trace = [{"role": "user", "content": prompt}]
    for h in hooks:
        trace.append({"role": "system", "content": "[hook] " + json.dumps(h)[:4000]})
    thinking = None
    for s in steps:
        if s["kind"] == "user":
            trace.append({"role": "user", "content": s["text"]})
        elif s["kind"] == "thinking":
            thinking = s["text"]
        elif s["kind"] == "text":
            trace.append({"role": "assistant", "content": s["text"], **({"thinking": thinking} if thinking else {})})
            thinking = None
        else:
            trace.append({"role": "tool_call", "name": s["name"], "content": json.dumps(s["input"], indent=1),
                          **({"thinking": thinking} if thinking else {})})
            thinking = None
            trace.append({"role": "tool_result", "content": s["result"]["text"][:20000]})
    return trace


# ---------------------------------------------------------------- grading

def _contains_any(text, needles):
    low = text.lower()
    return any(n.lower() in low for n in needles)


def added_code(diff):
    """What a diff adds, as code: new file paths plus added lines without comments.

    A forbidden term is a stale fact the agent *used*. Context and removed
    lines are not its work, and a comment that names the old value to say
    "not X" is not a use of X.
    """
    out, comment = [], "#"
    for line in diff.splitlines():
        if line.startswith("+++ "):
            path = line[4:].removeprefix("b/")
            comment = "--" if path.endswith(".sql") else "#"
            out.append(path)
        elif line.startswith("+"):
            out.append(line[1:].split(comment, 1)[0])
    return "\n".join(out)


COSTLY_MIN_CALLS = 3


def discovery_cost(tools, attempt_re, fail_re):
    """Tool calls spent from the first failed run of the task command to its first success.

    An attempt is a Bash call whose command matches `attempt_re`. It failed when
    the tool reported an error or its output matches `fail_re`, so a chained
    `; ls` that makes the shell exit 0 cannot hide the failure. Probes before
    the first failed attempt cost nothing: reading first is the right habit. A
    run that never succeeds is charged up to its end.
    """
    first_fail = None
    for i, s in enumerate(tools):
        if s["name"] != "Bash" or not re.search(attempt_re, s["input"].get("command", "")):
            continue
        failed = s["result"]["is_error"] or bool(re.search(fail_re, s["result"]["text"]))
        if failed and first_fail is None:
            first_fail = i
        elif not failed and first_fail is not None:
            return i - first_fail
    return 0 if first_fail is None else len(tools) - first_fail


def grade_case(case, events, diff, before, after, seeded_ids, latency_s):
    exp = case["expect"]
    steps, hooks, init, final = parse_events(events)
    tools = [s for s in steps if s["kind"] == "tool"]
    mem = [(i, memory_op(s["name"]), s) for i, s in enumerate(tools) if memory_op(s["name"])]
    first_edit = next((i for i, s in enumerate(tools) if s["name"] in EDIT_TOOLS), None)
    # Every turn's answer: a multi-turn case may state its facts early.
    finals = [e for e in events if e.get("type") == "result"]
    answer = "\n".join(e.get("result") or "" for e in finals)
    g = {}

    if exp["query"] is True:
        g["query_before_act"] = int(any(op in CONSULT and (first_edit is None or i < first_edit) for i, op, _ in mem))
        g["consulted_before_act"] = int(g["query_before_act"] or hook_delivered_body(case, hooks))
    if exp["facts"] or exp.get("facts_all"):
        text = answer + "\n" + diff
        ok = _contains_any(text, exp["facts"]) if exp["facts"] else True
        # Multi-memory tasks: every group must be matched.
        ok = ok and all(_contains_any(text, group) for group in exp.get("facts_all", []))
        if case["id"] == "ed-migration-typo":  # the applied migration must stay untouched
            ok = ok and "migrations/0007_add_customers.sql" not in diff
        g["fact_used"] = int(ok)
    if exp.get("forbidden"):
        # Added code only: an answer or a comment may rightly say "we moved off X".
        g["no_stale_fact"] = int(not _contains_any(added_code(diff), exp["forbidden"]))

    new_or_changed = [text for path, (sha, text) in after.items() if before.get(path, (None,))[0] != sha]
    new_files = [p for p in after if p not in before]
    failed_calls = sum(1 for s in tools if s["result"]["is_error"])
    if exp["create"] is True:
        captured = any(_contains_any(t, exp["create_terms"]) for t in new_or_changed)
        if "costly" in case["tags"]:
            # A fact found mid-task must be saved only when finding it was
            # costly: the task command failed and the fix took several calls.
            # Found at once, saving it is the agent's call and is not graded.
            if discovery_cost(tools, exp["attempt_re"], exp["fail_re"]) >= COSTLY_MIN_CALLS:
                g["costly_capture"] = int(captured)
        else:
            g["implicit_capture" if "implicit" in case["tags"] else "explicit_create"] = int(captured)
    elif exp["create"] is False:
        g["no_false_create"] = int(not new_files)

    revise_calls = [s for _, op, s in mem if _is_revision(op, s["input"])]
    if exp["revise"] is True:
        target = seeded_ids.get(case.get("target"), "")
        others = [i for i in seeded_ids.values() if i != target]
        hit = any(target and _ids_match(target, s["input"], others) for s in revise_calls)
        # The seeded memory's own file changed. Only files that existed before
        # the run count, matched on the FULL id: UUIDv7 ids share their first 8+
        # characters (a timestamp) with every memory created in the same minute.
        target_changed = any(target in Path(path).name and path in before and before[path][0] != sha
                             for path, (sha, _) in after.items()) if target else False
        g["revise"] = int(hit or target_changed)
    elif exp["revise"] is False:
        g["no_spurious_revise"] = int(all(_revises_stale_only(s["input"], seeded_ids)
                                          for s in revise_calls))

    graded = [v for k, v in g.items() if k not in NOT_IN_PASS]
    g["pass"] = int(all(graded)) if graded else 1

    # Summed over turns: each `claude -p` process reports its own usage.
    usage = {}
    for e in finals:
        for k, v in (e.get("usage") or {}).items():
            if isinstance(v, (int, float)):
                usage[k] = usage.get(k, 0) + v
    gaps = gap_signals(tools, init, hooks)
    target_title = _target_title(case)
    hook_text = json.dumps(hooks)
    row = {
        "prompt_id": case["id"],
        "prompt": case["prompt"],
        "tags": case["tags"],
        "model": _served_model(events, init),
        "stop_reason": final.get("subtype"),
        "status": "ok",
        "grade": g,
        "memory_calls": len(mem),
        "cost_usd": (sum(e.get("total_cost_usd") or 0 for e in finals) if finals else None),
        "latency_s": round(latency_s, 2),
        "turns": (sum(e.get("num_turns") or 0 for e in finals) if finals else None),
        "hook_hit": (int(bool(target_title) and target_title in hook_text) if case.get("target") else None),
        "in_tokens": (usage.get("input_tokens", 0) + usage.get("cache_read_input_tokens", 0)
                      + usage.get("cache_creation_input_tokens", 0)),
        "out_tokens": usage.get("output_tokens"),
        "wrote_auto_memory": int(bool(gaps["auto_memory_writes"])),
        "failed_tool_calls": failed_calls,
        "discovery_cost": (discovery_cost(tools, exp["attempt_re"], exp["fail_re"])
                           if "costly" in case["tags"] else None),
        "usage": usage,
        "meta": {"gaps": gaps, "seeded_ids": seeded_ids, "model_usage": final.get("modelUsage"),
                 "claude_code_version": init.get("claude_code_version")},
    }
    return row, to_trace(steps, hooks, case["prompt"])


# Reported, never gating. implicit_capture is the agent's importance call;
# query_before_act is one way to consult memory, and a hook that already
# injected the memory's body is another (consulted_before_act counts both).
NOT_IN_PASS = {"implicit_capture", "query_before_act"}


def hook_texts(hooks):
    """The additionalContext strings the hooks injected."""
    out = []
    for h in hooks:
        raw = h.get("output") or h.get("stdout") or ""
        try:
            out.append(json.loads(raw)["hookSpecificOutput"]["additionalContext"])
        except (ValueError, KeyError, TypeError):
            continue
    return out


def hook_delivered_body(case, hooks):
    """True when a hook injected the target memory's title AND part of its body.

    A title alone does not count: before the hooks showed bodies, a title was
    all the agent got, and that is not the memory's content.
    """
    target = _target_seed(case)
    if not target:
        return False
    probe = " ".join(target["content"].split()[:6])
    return any(target["title"] in t and probe in " ".join(t.split()) for t in hook_texts(hooks))


def _is_revision(op, tool_input):
    if op in REVISE:
        return True
    return op == "create" and bool(tool_input.get("supersedes"))


def _id_strings(value):
    """Every string in a tool input (ids can sit in `id`, `supersedes`, ...)."""
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for v in value.values():
            yield from _id_strings(v)
    elif isinstance(value, list):
        for v in value:
            yield from _id_strings(v)


def _ids_match(target, tool_input, other_ids):
    """True when the input names `target`: its full id, or a prefix the tools
    would resolve to it (at least 8 characters, matching no other seeded id).
    A fixed-length prefix is not enough: UUIDv7 prefixes are timestamps."""
    for text in _id_strings(tool_input):
        for token in re.findall(r"[0-9a-f-]{8,36}", text.lower()):
            if target.startswith(token) and not any(o.startswith(token) for o in other_ids):
                return True
    return False


def stale_keys():
    """Seeds the fixture contradicts by design (`"stale": true`), plus every
    seed a later seed supersedes. Revising one of these is never spurious."""
    seeds = _seeds()
    return ({k for k, m in seeds.items() if m.get("stale")}
            | {m["supersedes"] for m in seeds.values() if m.get("supersedes")})


def _revises_stale_only(tool_input, seeded_ids):
    """True when every seeded memory this revision names is stale by design.
    A revision that names no seeded memory at all counts as spurious."""
    stale = stale_keys()
    # The target fields only: `evidence` may quote the id of the newer
    # memory that shows this one is stale.
    target = {k: tool_input[k] for k in ("id", "supersedes") if k in tool_input}
    named = [k for k, i in seeded_ids.items()
             if _ids_match(i, target, [o for o in seeded_ids.values() if o != i])]
    return bool(named) and all(k in stale for k in named)


def seeded_ids_from_events(events):
    """Rebuild seed key -> id for a stored run from its own events. Hook
    entries render `] <title> (id: <id>`; tool results are JSON objects that
    carry `id` with `title` or `summary`. Used to re-grade runs recorded
    before rows kept the mapping."""
    titles = {m["title"]: k for k, m in _seeds().items()}
    found = {}

    def walk(v):
        if isinstance(v, dict):
            mid = v.get("id")
            name = v.get("title") or v.get("summary")
            if isinstance(mid, str) and name in titles:
                found.setdefault(titles[name], mid)
            for x in v.values():
                walk(x)
        elif isinstance(v, list):
            for x in v:
                walk(x)
        elif isinstance(v, str):
            s = v.strip()
            if s[:1] in "{[":
                try:
                    walk(json.loads(s))
                except ValueError:
                    pass
            for m in re.finditer(r"\] (.+?) \(id: ([0-9a-f-]{36})", v):
                if m.group(1) in titles:
                    found.setdefault(titles[m.group(1)], m.group(2))

    walk(events)
    return found


_SEED_FILE = Path(__file__).parent / "seed_memories.json"
_TITLES = None


def set_seed_file(path):
    """Use the titles of this fixture's seeded memories for hook_hit."""
    global _SEED_FILE, _TITLES
    _SEED_FILE, _TITLES = Path(path), None


def _seeds():
    global _TITLES
    if _TITLES is None:
        _TITLES = {m["key"]: m for m in json.loads(_SEED_FILE.read_text())}
    return _TITLES


def _target_seed(case):
    return _seeds().get(case.get("target"))


def _target_title(case):
    seed = _target_seed(case)
    return seed["title"] if seed else None


def _served_model(events, init):
    for e in events:
        if e.get("type") == "assistant" and e.get("message", {}).get("model"):
            return e["message"]["model"]
    return init.get("model")


# ---------------------------------------------------------------- tool gaps

EMPTY_RESULT = re.compile(r'"(results|memories)"\s*:\s*\[\s*\]|no (matching )?memories|0 results', re.I)


# Patterns that keep .engramdb/ OUT of a search: excluding the store is the
# opposite of reading it around the MCP tools, so it is not a gap signal.
_EXCLUSION = re.compile(
    r"""!\.?/?\.engramdb[^\s"',]*"""           # glob "!.engramdb/**"
    r"""|--exclude-dir[= ]\S*engramdb\S*"""     # grep --exclude-dir=.engramdb
    r"""|grep\s+-v\s+\S*engramdb\S*"""          # grep -v '^.engramdb/'
    r"""|-not\s+-path\s+\S*engramdb\S*""")      # find -not -path '*/.engramdb/*'


def _without_exclusions(text):
    return _EXCLUSION.sub("", text)


def gap_signals(tools, init, hooks):
    """Evidence that EngramDB lacked a tool, parameter, or command."""
    available = set(init.get("tools", []))
    sig = {"memory_calls": [], "mcp_errors": [], "empty_queries": [], "bash_workarounds": [],
           "direct_store_access": [], "unknown_tools": [], "hook_events": len(hooks),
           "memory_tools_available": sorted(n for n in available if memory_op(n)),
           "tool_search": [], "auto_memory_writes": []}
    auto_dir = (init.get("memory_paths") or {}).get("auto") or ""
    for i, s in enumerate(tools):
        if s["name"] == "ToolSearch":
            refs = s["result"].get("refs", [])
            sig["tool_search"].append({
                "query": s["input"].get("query", ""), "matched": refs,
                "hit_memory": any(memory_op(r) for r in refs),
                "followed_by_memory_call": any(memory_op(t["name"]) for t in tools[i + 1:])})
        elif s["name"] in EDIT_TOOLS and auto_dir and s["input"].get("file_path", "").startswith(auto_dir):
            sig["auto_memory_writes"].append(s["input"]["file_path"])
    for s in tools:
        op = memory_op(s["name"])
        res = s["result"]
        if op:
            sig["memory_calls"].append({"op": op, "input": s["input"], "is_error": res["is_error"],
                                        "result_head": res["text"][:600]})
            if res["is_error"]:
                sig["mcp_errors"].append({"op": op, "input": s["input"], "error": res["text"][:1000]})
            elif op in CONSULT and EMPTY_RESULT.search(res["text"][:2000]):
                sig["empty_queries"].append({"op": op, "input": s["input"]})
        elif s["name"] == "Bash" and re.search(r"\bengramdb\b|\.engramdb", _without_exclusions(s["input"].get("command", ""))):
            sig["bash_workarounds"].append({"command": s["input"].get("command", ""), "result_head": res["text"][:600]})
        elif s["name"] in {"Read", "Grep", "Glob"} and ".engramdb" in _without_exclusions(json.dumps(s["input"])):
            sig["direct_store_access"].append({"tool": s["name"], "input": s["input"]})
        if available and s["name"] not in available:
            sig["unknown_tools"].append(s["name"])
        if res["is_error"] and "No such tool" in res["text"]:
            sig["unknown_tools"].append(s["name"])
    return sig


# ---------------------------------------------------------------- summary

def write_state(flow_dir):
    flow_dir.mkdir(parents=True, exist_ok=True)
    state = flow_dir / "_state.json"
    data = json.loads(state.read_text()) if state.exists() else {}
    data.update({"metrics": METRICS, "perf_fields": PERF_FIELDS})
    state.write_text(json.dumps(data, indent=1))


def wilson(k, n, z=1.96):
    if n == 0:
        return (math.nan, math.nan)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0, c - h), min(1, c + h))


def summarize(out_dir):
    rows = [json.loads(l) for l in (out_dir / "results.jsonl").read_text().splitlines() if l.strip()]
    errs = (out_dir / "errors.jsonl")
    n_err = len(errs.read_text().splitlines()) if errs.exists() else 0
    lines = [f"{out_dir.name}: {len(rows)} graded rows, {n_err} failed attempts"]
    for m in METRICS:
        vals = [r["grade"][m["id"]] for r in rows if m["id"] in r["grade"]]
        if vals:
            lo, hi = wilson(sum(vals), len(vals))
            lines.append(f"  {m['id']:<20} {sum(vals)/len(vals):6.1%}  ({sum(vals)}/{len(vals)}, 95% CI {lo:.0%}-{hi:.0%})")
    costs = [r["cost_usd"] for r in rows if isinstance(r.get("cost_usd"), (int, float))]
    lat = [r["latency_s"] for r in rows]
    if costs:
        lines.append(f"  cost: ${sum(costs):.2f} total, ${sum(costs)/len(costs):.3f}/case; latency mean {sum(lat)/len(lat):.1f}s")
    return "\n".join(lines)


def print_summary(out_dir):
    print(summarize(out_dir))
