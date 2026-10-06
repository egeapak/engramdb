from dataclasses import dataclass, field

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
