"""Subscription plans."""
from dataclasses import dataclass


@dataclass(frozen=True)
class Plan:
    id: str
    price_cents: int
    interval: str  # "month" or "year"


PLANS = {
    "plan_basic_month": Plan("plan_basic_month", 1500, "month"),
    "plan_pro_month": Plan("plan_pro_month", 4500, "month"),
    "plan_pro_year": Plan("plan_pro_year", 45000, "year"),
}
