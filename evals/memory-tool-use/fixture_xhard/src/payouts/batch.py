"""Payout runs: what each seller gets paid, and when."""
from dataclasses import dataclass

from src.jobs.queue import enqueue
from src.payouts import bankco


@dataclass
class Payout:
    id: str
    seller_id: str
    amount_minor: int
    currency: str
    iban: str
    status: str = "pending"


def eligible_balance(seller_id, rows):
    """How much we can pay `seller_id` now, in minor units, from their settlement rows."""
    raise NotImplementedError


def should_pay_out(balance_minor, currency):
    """Whether a balance is paid out in this run or carried over to the next one."""
    raise NotImplementedError


def submit_batch(payouts):
    """Hand each payout of a run to the background workers that call BankCo."""
    raise NotImplementedError


def mark_paid(payout, user):
    """Record that BankCo executed `payout`; `user` is who confirmed it."""
    raise NotImplementedError
