from .invoices import Invoice


def refund(invoice: Invoice, amount_cents: int):
    if amount_cents > invoice.subtotal_cents + invoice.tax_cents:
        raise ValueError("refund exceeds invoice total")
    return {"invoice_id": invoice.id, "refunded_cents": amount_cents}
