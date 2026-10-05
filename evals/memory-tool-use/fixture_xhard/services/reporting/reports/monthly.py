"""Monthly revenue reports.

Since the 2026-09 close, a report month runs from midnight to midnight
America/New_York, the time zone Finance closes the books in (REP-19).
"""
from decimal import Decimal


def load_rows(month):
    """Rows for one month: dicts with customer_id, currency and total_cents."""
    raise NotImplementedError("reads from the billing warehouse")


def load_payout_rows(month):
    """Seller payout rows for one month, from the billing warehouse's payouts table."""
    raise NotImplementedError("reads from the billing warehouse")


def month_bounds(month):
    """The [start, end) instants of report month "YYYY-MM", as aware datetimes."""
    raise NotImplementedError
