"""Oracle / null checks for grade.py. No API calls.

    python3 -m unittest test_grade.py
"""

import json
import unittest
from pathlib import Path

import grade

HERE = Path(__file__).resolve().parent
CASES = {c["id"]: c for c in map(json.loads, (HERE / "cases.jsonl").read_text().splitlines())}
MEM = "mcp__plugin_engram_memory__"
IDS = {"logging": "a1b2c3d4e5f60718", "tests": "0f1e2d3c4b5a6978"}


def events(tool_calls=(), answer="", errors=()):
    ev = [{"type": "system", "subtype": "init", "tools": ["Read", "Edit", MEM + "query", MEM + "create"]}]
    for i, (name, inp) in enumerate(tool_calls):
        ev.append({"type": "assistant", "message": {"model": "claude-opus-5-5",
                   "content": [{"type": "tool_use", "id": f"t{i}", "name": name, "input": inp}]}})
        ev.append({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": f"t{i}",
                   "content": "error: bad param" if i in errors else "ok", "is_error": i in errors}]}})
    ev.append({"type": "result", "subtype": "success", "result": answer, "total_cost_usd": 0.01,
               "num_turns": 1, "usage": {"input_tokens": 10, "output_tokens": 5}})
    return ev


def run(case_id, ev, diff="", before=None, after=None):
    row, _ = grade.grade_case(CASES[case_id], ev, diff, before or {}, after or {}, IDS, 1.0)
    return row["grade"], row


class Oracle(unittest.TestCase):
    def test_project_question(self):
        g, _ = run("pq-tests", events([(MEM + "query", {"mode": "filter", "query": "tests"})], "Run make test-fast."))
        self.assertEqual(g, {"query_before_act": 1, "consulted_before_act": 1, "fact_used": 1,
                             "no_false_create": 1, "no_spurious_revise": 1, "pass": 1})

    def test_edit_after_query(self):
        ev = events([(MEM + "query", {"mode": "rank", "path": "src/billing/refunds.py"}), ("Edit", {})], "done")
        g, _ = run("ed-refund-log", ev, diff="+log = structlog.get_logger()")
        self.assertEqual(g["pass"], 1)

    def test_migration_needs_new_file(self):
        ev = events([(MEM + "query", {"mode": "rank"}), ("Write", {})], "Added 0008.")
        self.assertEqual(run("ed-migration-typo", ev, diff="+++ b/migrations/0008_fix_customers.sql")[0]["pass"], 1)
        bad = "--- a/migrations/0007_add_customers.sql\n+++ b/migrations/0007_add_customers.sql\n+0008"
        self.assertEqual(run("ed-migration-typo", ev, diff=bad)[0]["fact_used"], 0)

    def test_explicit_create(self):
        after = {"/ws/.engramdb/memories/x.md": ("h1", "Use jobs.enqueue() for all background jobs")}
        g, _ = run("nk-jobs-explicit", events([(MEM + "create", {})], "Saved."), after=after)
        self.assertEqual(g["explicit_create"], 1)
        self.assertEqual(g["pass"], 1)

    def test_implicit_capture_not_in_pass(self):
        g, _ = run("nk-owner", events([], "Noted."))
        self.assertEqual(g["implicit_capture"], 0)
        self.assertEqual(g["pass"], 1)

    def test_revise_targets_seeded_id(self):
        ev = events([(MEM + "challenge", {"id": IDS["logging"][:8], "evidence": "moved to loguru"})], "from loguru import logger")
        g, _ = run("ct-loguru", ev, diff="+from loguru import logger")
        self.assertEqual(g["revise"], 1)

    def test_negative_query_is_not_graded(self):
        g, _ = run("ng-tuple", events([(MEM + "query", {"mode": "filter", "query": "tuple"})], "Tuples are immutable."))
        self.assertNotIn("query_before_act", g)
        self.assertEqual(g["pass"], 1)


class Revise(unittest.TestCase):
    # Two seeds that share a timestamp prefix, as UUIDv7 ids created in the same minute do.
    SHARED = {"logging": "01a0f7c2-1264-7000-8000-000000000001", "tests": "01a0f7c2-3c9a-7000-8000-000000000002"}

    def grade(self, calls, before=None, after=None):
        ev = events(calls, "ok")
        row, _ = grade.grade_case(CASES["ct-loguru"], ev, "+loguru", before or {}, after or {}, self.SHARED, 1.0)
        return row["grade"]["revise"]

    def test_full_id_and_unique_prefix_count(self):
        target = self.SHARED["logging"]
        self.assertEqual(self.grade([(MEM + "challenge", {"id": target, "evidence": "moved"})]), 1)
        self.assertEqual(self.grade([(MEM + "update", {"id": target[:13]})]), 1)

    def test_shared_timestamp_prefix_does_not_count(self):
        self.assertEqual(self.grade([(MEM + "challenge", {"id": "01a0f7c2"})]), 0)
        self.assertEqual(self.grade([(MEM + "challenge", {"id": self.SHARED["tests"]})]), 0)

    def test_supersede_and_resolve_count_plain_create_and_verify_do_not(self):
        target = self.SHARED["logging"]
        self.assertEqual(self.grade([(MEM + "create", {"summary": "use loguru", "supersedes": [target]})]), 1)
        self.assertEqual(self.grade([(MEM + "resolve", {"id": target, "action": "invalidate"})]), 1)
        self.assertEqual(self.grade([(MEM + "create", {"summary": "use loguru"})]), 0)
        self.assertEqual(self.grade([(MEM + "verify", {"id": target})]), 0)

    def test_only_the_existing_target_file_counts_as_changed(self):
        target = self.SHARED["logging"]
        old = f"/ws/.engramdb/memories/use-structlog_{target}.md"
        new = f"/ws/.engramdb/memories/use-loguru_{target[:8]}-9999-7000-8000-000000000009.md"
        self.assertEqual(self.grade([], before={old: ("a", "x")}, after={old: ("b", "y")}), 1)
        self.assertEqual(self.grade([], before={old: ("a", "x")}, after={old: ("a", "x"), new: ("c", "z")}), 0)

    def test_create_with_supersedes_is_a_spurious_revise_elsewhere(self):
        ev = events([(MEM + "create", {"summary": "x", "supersedes": ["abc"]})], "ok")
        row, _ = grade.grade_case(CASES["pq-tests"], ev, "", {}, {}, IDS, 1.0)
        self.assertEqual(row["grade"]["no_spurious_revise"], 0)


class Signals(unittest.TestCase):
    def test_tool_search_and_auto_memory(self):
        auto = "/tmp/x/claude-config/projects/p/memory/"
        ev = [{"type": "system", "subtype": "init", "tools": ["ToolSearch", "Write"], "memory_paths": {"auto": auto}},
              {"type": "assistant", "message": {"content": [
                  {"type": "tool_use", "id": "a", "name": "ToolSearch", "input": {"query": "select:" + MEM + "create"}}]}},
              {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "a",
                  "content": [{"type": "tool_reference", "tool_name": MEM + "create"}]}]}},
              {"type": "assistant", "message": {"content": [
                  {"type": "tool_use", "id": "b", "name": "Write", "input": {"file_path": auto + "jobs.md"}}]}},
              {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "b", "content": "ok"}]}},
              {"type": "result", "subtype": "success", "result": "saved", "usage": {}}]
        _, row = run("nk-jobs-explicit", ev)
        ts = row["meta"]["gaps"]["tool_search"]
        self.assertEqual(ts[0]["matched"], [MEM + "create"])
        self.assertTrue(ts[0]["hit_memory"])
        self.assertFalse(ts[0]["followed_by_memory_call"])
        self.assertEqual(row["wrote_auto_memory"], 1)
        self.assertEqual(row["grade"]["explicit_create"], 0)


def hook_event(context):
    out = json.dumps({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": context}})
    return {"type": "system", "subtype": "hook_response", "output": out}


class HookDelivery(unittest.TestCase):
    TITLE = "Run tests with make test-fast"
    BODY = "Run `make test-fast` for the unit suite (skips Postgres tests)."

    def test_hook_with_body_counts_as_consulted(self):
        ev = events([], "Run make test-fast.")
        ev.insert(1, hook_event(f"- [convention] {self.TITLE} (id: x; source: shared/human)\n  {self.BODY}"))
        g, _ = run("pq-tests", ev)
        self.assertEqual(g["query_before_act"], 0)
        self.assertEqual(g["consulted_before_act"], 1)
        self.assertEqual(g["pass"], 1, "query_before_act no longer gates pass")

    def test_title_alone_does_not_count(self):
        ev = events([], "Run make test-fast.")
        ev.insert(1, hook_event(f"- [convention] {self.TITLE} (source: shared/human)\n  "))
        g, _ = run("pq-tests", ev)
        self.assertEqual(g["consulted_before_act"], 0)
        self.assertEqual(g["pass"], 0)


class Null(unittest.TestCase):
    def test_empty_answer_fails(self):
        for cid in ("pq-tests", "ed-tax", "nk-jobs-explicit", "ct-loguru"):
            self.assertEqual(run(cid, events([], ""))[0]["pass"], 0, cid)

    def test_query_after_edit_fails(self):
        ev = events([("Edit", {}), (MEM + "query", {"mode": "rank"})], "ROUND_HALF_EVEN")
        self.assertEqual(run("ed-tax", ev, diff="+ROUND_HALF_EVEN")[0]["query_before_act"], 0)

    def test_spurious_create_fails(self):
        after = {"/ws/.engramdb/memories/new.md": ("h", "lists vs tuples")}
        self.assertEqual(run("ng-tuple", events([], "x"), after=after)[0]["pass"], 0)

    def test_revise_wrong_memory_fails(self):
        ev = events([(MEM + "challenge", {"id": IDS["tests"]})], "loguru")
        self.assertEqual(run("ct-loguru", ev)[0]["revise"], 0)

    def test_gap_signals(self):
        ev = events([(MEM + "query", {"mode": "bogus"}), ("Bash", {"command": "engramdb list --tag x"}),
                     ("Read", {"file_path": ".engramdb/memories/a.md"})], "x", errors={0})
        gaps = run("pq-tests", ev)[1]["meta"]["gaps"]
        self.assertEqual(len(gaps["mcp_errors"]), 1)
        self.assertEqual(len(gaps["bash_workarounds"]), 1)
        self.assertEqual(len(gaps["direct_store_access"]), 1)


if __name__ == "__main__":
    unittest.main()


def _costly_case():
    return {"id": "dc-x", "tags": ["discovered", "implicit", "costly"], "prompt": "p",
            "expect": {"query": None, "create": True, "revise": None, "facts": [],
                       "create_terms": ["TZ=UTC"], "attempt_re": r"backfill\.py",
                       "fail_re": r"does not line up|Traceback"}}


def _bash_events(calls):
    """calls: (command, output, is_error) per Bash call."""
    events = [{"type": "system", "subtype": "init"}]
    for i, (cmd, out, err) in enumerate(calls):
        events.append({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": f"t{i}", "name": "Bash", "input": {"command": cmd}}]}})
        events.append({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": f"t{i}", "content": out, "is_error": err}]}})
    events.append({"type": "result", "result": "done", "usage": {}})
    return events


def _events_with_failures(n):
    return _bash_events([("make test", "x", i < n) for i in range(n + 1)])


FAIL = ("python backfill.py", "RuntimeError: period does not line up", True)
OK = ("TZ=UTC python backfill.py", "would backfill", False)
PROBE = ("ls var", "No such file", True)
READ = ("cat src/periods.py", "...", False)


def _costly(calls, after):
    row, _ = grade.grade_case(_costly_case(), _bash_events(calls), "", {}, after, {}, 1.0)
    return row


def test_costly_capture_graded_when_the_fix_took_several_calls():
    saved = {"m.md": ("b", "backfills need TZ=UTC")}
    row = _costly([FAIL, READ, PROBE, OK], saved)
    assert row["discovery_cost"] == 3 and row["grade"]["costly_capture"] == 1 and row["grade"]["pass"] == 1
    row = _costly([FAIL, READ, PROBE, OK], {})
    assert row["grade"]["costly_capture"] == 0 and row["grade"]["pass"] == 0


def test_costly_capture_not_graded_when_cheap_or_avoided():
    assert "costly_capture" not in _costly([READ, OK], {})["grade"]          # read first, no failure
    assert "costly_capture" not in _costly([FAIL, OK], {})["grade"]          # fixed at once
    assert "costly_capture" not in _costly([PROBE, PROBE, READ, OK], {})["grade"]  # probes before any attempt


def test_masked_failure_still_counts_and_a_run_that_never_succeeds_is_charged():
    masked = ("python backfill.py; ls -la var", "RuntimeError: period does not line up\ntotal 0", False)
    assert _costly([masked, READ, READ, OK], {})["discovery_cost"] == 3
    assert _costly([FAIL, READ, READ], {})["discovery_cost"] == 3

HARD = {c["id"]: c for c in map(json.loads, (HERE / "cases_hard.jsonl").read_text().splitlines())}


def _stale(case_id, diff):
    row, _ = grade.grade_case(HARD[case_id], _events_with_failures(0), diff, {}, {}, {}, 1.0)
    return row["grade"]["no_stale_fact"]


def test_stale_fact_ignores_comments_docstring_mentions_and_context():
    diff = ("+++ b/src/billing/currency.py\n"
            "+from decimal import ROUND_FLOOR\n"
            "+    # not tax's ROUND_HALF_EVEN\n"
            "+    \"\"\"Different from tax rounding (ROUND_HALF_EVEN).\"\"\"\n"
            " CONTEXT rounding=ROUND_HALF_EVEN\n"
            "-    return x.quantize(q, rounding=ROUND_HALF_EVEN)\n"
            "+    return x.quantize(q, rounding=ROUND_FLOOR)\n")
    assert _stale("ds-fx-rounding", diff) == 1
    assert _stale("ds-fx-rounding", "+    return x.quantize(q, rounding=ROUND_HALF_EVEN)\n") == 0


def test_stale_fact_local_helper_is_not_the_root_module():
    local = ("+++ b/services/notifier/flags.py\n"
             "+def is_on(name):\n"
             "+    # cannot read the root flags.toml\n")
    assert _stale("ds-notifier-flag", local) == 1
    assert _stale("ds-notifier-flag", "+++ b/services/notifier/send.py\n+from src.flags import is_on\n") == 0


def test_stale_fact_sees_new_file_paths_and_sql_comments():
    assert _stale("ds-notifier-migration", "+++ b/migrations/0008_rename.sql\n+SELECT 1;\n") == 0
    assert _stale("ds-notifier-migration", "+++ b/services/notifier/m.sql\n+-- no RENAME COLUMN here\n") == 1


def test_costly_capture_not_graded_when_the_fact_was_never_found():
    workaround = ("echo '{}' > fixture.json", "", False)
    rerun = ("python backfill.py", "would backfill", False)
    row = _costly([FAIL, READ, workaround, rerun], {})
    assert row["discovery_cost"] == 3 and "costly_capture" not in row["grade"]
