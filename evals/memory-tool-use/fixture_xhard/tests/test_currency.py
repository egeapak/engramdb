import unittest

from src.billing.currency import convert
from tests.fxdata import load_rates


class ConvertTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.rates = load_rates("2026-09-30")

    def test_exact_conversion(self):
        self.assertEqual(convert(1000, self.rates["EUR"]), 920)

    def test_zero(self):
        self.assertEqual(convert(0, self.rates["GBP"]), 0)


if __name__ == "__main__":
    unittest.main()
