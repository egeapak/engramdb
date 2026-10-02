from .provider import send
from .render import render


def send_receipt(invoice_id, to_addr, total_minor):
    body = render("receipt", invoice_id=invoice_id, total_minor=total_minor)
    return send(to_addr, "Your receipt", body)


def send_payment_reminder(invoice_id, to_addr):
    body = render("payment_reminder", invoice_id=invoice_id)
    return send(to_addr, "Payment reminder", body)
