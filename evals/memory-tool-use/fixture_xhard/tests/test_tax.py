from src.billing.tax import line_total


def test_line_total():
    assert line_total([{"quantity": 2, "unit_cents": 150}]) == 300
