"""Proration when a subscription changes or ends mid-period."""
from decimal import ROUND_HALF_EVEN, Decimal


def prorate_amount(price_cents, period_start, period_end, at):
    """The part of `price_cents` that covers the unused time from `at` to `period_end`, in cents."""
    raise NotImplementedError


def prorated_tax(prorated_cents, rate):
    """Tax on a prorated amount, in cents."""
    return int((Decimal(prorated_cents) * Decimal(str(rate))).quantize(Decimal("1"), rounding=ROUND_HALF_EVEN))
