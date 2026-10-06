import unittest

from ingest.parse import exponent


class ExponentTest(unittest.TestCase):
    def test_default_is_two(self):
        self.assertEqual(exponent("EUR"), 2)

    def test_three_decimal_currencies(self):
        self.assertEqual(exponent("KWD"), 3)


if __name__ == "__main__":
    unittest.main()
