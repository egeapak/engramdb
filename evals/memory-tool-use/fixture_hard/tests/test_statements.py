import json
import unittest
from pathlib import Path

from src.billing.statements import render_statement
from tests.goldens import read_golden

INPUTS = Path(__file__).resolve().parent / "golden" / "inputs"


class StatementGoldenTest(unittest.TestCase):
    def test_basic_statement(self):
        case = json.loads((INPUTS / "statement_basic.json").read_text())
        self.assertEqual(render_statement(case["customer_id"], case["invoices"]),
                         read_golden("statement_basic.txt"))


if __name__ == "__main__":
    unittest.main()
