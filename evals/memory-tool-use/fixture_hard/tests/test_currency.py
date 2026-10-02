import json
import os
import unittest
from decimal import Decimal
from pathlib import Path

from src.billing.currency import convert


class ConvertTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        root = os.environ.get("LEDGERLINE_FX_FIXTURES")
        if not root:
            raise RuntimeError(
                "LEDGERLINE_FX_FIXTURES is not set: point it at tests/fixtures/fx. "
                "The FX tests never call the live rate feed."
            )
        cls.rates = json.loads((Path(root) / "2026-09-30.json").read_text())

    def test_exact_conversion(self):
        self.assertEqual(convert(1000, Decimal(self.rates["EUR"])), 920)

    def test_zero(self):
        self.assertEqual(convert(0, Decimal(self.rates["GBP"])), 0)


if __name__ == "__main__":
    unittest.main()
