"""Monthly revenue reports."""
from decimal import Decimal


def load_rows(month):
    """Rows for one month: dicts with customer_id, currency and total_cents."""
    raise NotImplementedError("reads from the billing warehouse")
