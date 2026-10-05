import uuid
from dataclasses import dataclass, field
from datetime import datetime

import requests
import structlog

log = structlog.get_logger()

LEDGER_URL = "https://ledger.internal.acme.example/v1/postings"


@dataclass
class Invoice:
    id: str
    customer_id: str
    subtotal_cents: int
    tax_cents: int = 0
    ledger_ref: str | None = None
    lines: list = field(default_factory=list)
    origin: str | None = None
    finalized_at: datetime | None = None


def create_invoice(customer_id, lines, *, origin=None):
    """Create a draft invoice for `customer_id` from `lines` (dicts with quantity and unit_cents)."""
    subtotal = sum(line["quantity"] * line["unit_cents"] for line in lines)
    return Invoice(id=f"inv_{uuid.uuid4().hex[:12]}", customer_id=customer_id,
                   subtotal_cents=subtotal, lines=list(lines), origin=origin)


def finalize(invoice):
    resp = requests.post(LEDGER_URL, json={"invoice": invoice.id, "amount": invoice.subtotal_cents + invoice.tax_cents}, timeout=10)
    resp.raise_for_status()
    invoice.ledger_ref = resp.json()["ref"]
    log.info("invoice_finalized", invoice_id=invoice.id)
    return invoice


def finalize_once(invoice):
    if invoice.ledger_ref:
        return invoice
    return finalize(invoice)
