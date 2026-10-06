from flask import Blueprint, request

bp = Blueprint("internal", __name__)


@bp.post("/internal/settlements")
def settlement():
    """Called by services/ingest when the PSP reports a settled payment."""
    body = request.get_json()
    # TODO: mark the invoice paid
    return "", 204
