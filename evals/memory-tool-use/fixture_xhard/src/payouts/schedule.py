"""When a payout leaves the bank."""
from datetime import time, timedelta

# BankCo's same-day cutoff, CET.
SAME_DAY_CUTOFF = time(14, 0)


def next_business_day(day):
    day += timedelta(days=1)
    while day.weekday() >= 5:
        day += timedelta(days=1)
    return day


def payout_date(submitted_at):
    """The business day on which a payout submitted at `submitted_at` (an aware datetime) leaves the bank."""
    raise NotImplementedError
