"""Billing periods as the ledger stores them."""
import calendar
import time
from datetime import datetime


def month_window(month):
    """Return the [start, end) bounds of a billing month ("YYYY-MM")."""
    start = datetime.strptime(month, "%Y-%m")
    end = start.replace(year=start.year + 1, month=1) if start.month == 12 else start.replace(month=start.month + 1)
    # The ledger's period table is keyed by epoch seconds; both bounds must map onto it.
    for bound in (start, end):
        if time.mktime(bound.timetuple()) != calendar.timegm(bound.timetuple()):
            raise RuntimeError(f"period {month} does not line up with the ledger period table")
    return start, end
