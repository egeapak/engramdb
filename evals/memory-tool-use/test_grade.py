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
        self.assertEqual(g, {"query_before_act": 1, "fact_used": 1, "no_false_create": 1, "no_spurious_revise": 1, "pass": 1})

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
