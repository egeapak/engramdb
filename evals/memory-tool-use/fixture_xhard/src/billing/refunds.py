from .invoices import Invoice

# Days after finalization during which an invoice can be refunded.
REFUND_WINDOW_DAYS = 180


def refund(invoice: Invoice, amount_cents: int):
    if amount_cents > invoice.subtotal_cents + invoice.tax_cents:
        raise ValueError("refund exceeds invoice total")
    return {"invoice_id": invoice.id, "refunded_cents": amount_cents}
