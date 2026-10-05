import pytest
import requests

SANDBOX = "https://sandbox.payments.example/v1/charges"


@pytest.mark.integration
def test_charges_roundtrip():
    for amount in (100, 250, 999, 1200):
        resp = requests.post(SANDBOX, json={"amount_cents": amount}, timeout=10)
        assert resp.status_code == 201
