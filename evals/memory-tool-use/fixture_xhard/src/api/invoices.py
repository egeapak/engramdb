from flask import Blueprint, jsonify

bp = Blueprint("invoices", __name__)

_INVOICES = {}


@bp.get("/invoices/<invoice_id>")
def get_invoice(invoice_id):
    invoice = _INVOICES.get(invoice_id)
    return jsonify(invoice.__dict__)
