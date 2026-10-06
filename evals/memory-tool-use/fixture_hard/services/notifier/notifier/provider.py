import os

import requests

API_URL = "https://api.mailprovider.example/v3/messages"


def send(to_addr, subject, body):
    resp = requests.post(
        API_URL,
        headers={"Authorization": f"Bearer {os.environ['MAIL_API_KEY']}"},
        json={"to": to_addr, "subject": subject, "html": body},
        timeout=10,
    )
    resp.raise_for_status()
    return resp.json()["id"]
