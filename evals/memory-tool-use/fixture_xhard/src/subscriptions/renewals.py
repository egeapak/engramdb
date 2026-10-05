"""Subscription renewals."""
from dataclasses import dataclass
from datetime import datetime

from src.billing.invoices import create_invoice, finalize_once
from src.billing.refunds import refund
from src.jobs.queue import enqueue
from src.subscriptions.plans import PLANS
from src.subscriptions.proration import prorate_amount

RENEWAL_NOTICE_DAYS = 7


@dataclass
class Subscription:
    id: str
    customer_id: str
    plan_id: str
    current_period_start: datetime
    current_period_end: datetime
    customer_tz: str = "UTC"
    status: str = "active"


def next_renewal_at(sub):
    """When `sub` renews next, as an aware datetime in UTC."""
    raise NotImplementedError


def renew(sub):
    """Start the next period of `sub`: invoice it and charge it."""
    raise NotImplementedError


def retry_failed_renewal(sub, invoice, attempt):
    """Called when the renewal charge for `invoice` failed on `attempt` (1-based)."""
    raise NotImplementedError


def reminder_due(sub, now):
    """True when the renewal reminder for `sub` should go out at `now`."""
    raise NotImplementedError


def send_renewal_notice(sub):
    """Email the customer that `sub` is about to renew."""
    raise NotImplementedError


def cancel_now(sub, at, invoice):
    """Cancel `sub` at `at` and refund the unused part of the current period's `invoice`."""
    raise NotImplementedError
