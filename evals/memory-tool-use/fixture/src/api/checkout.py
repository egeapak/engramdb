from flask import Blueprint, jsonify, request

bp = Blueprint("checkout", __name__)


@bp.post("/checkout")
def checkout():
    cart = request.get_json()
    total = sum(item["unit_cents"] * item["quantity"] for item in cart["items"])
    return jsonify({"total_cents": total, "status": "pending"})
