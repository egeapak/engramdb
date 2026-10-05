"""Platform fees charged on seller payouts."""
from decimal import Decimal


def platform_fee(gross_minor, rate_bps):
    """Return the platform fee, in minor units, on a payout of `gross_minor` at `rate_bps` basis points."""
    raise NotImplementedError
