from flask import Blueprint, request

bp = Blueprint("webhooks", __name__)


@bp.post("/webhooks/psp")
def psp_webhook():
    event = request.get_json()
    # TODO: verify the event, then dispatch on event["type"]
    return "", 204
