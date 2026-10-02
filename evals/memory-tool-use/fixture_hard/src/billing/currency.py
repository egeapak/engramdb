"""Currency conversion for multi-currency invoices."""
from decimal import Decimal


def fetch_rates(day):
    """Return {currency: Decimal rate against USD} for the given date."""
    raise NotImplementedError


def convert(amount_cents, rate):
    """Convert an amount in minor units with a Decimal rate; returns minor units."""
    raise NotImplementedError
