"""Client for BankCo, the bank partner that executes seller payouts."""
import os

import requests

BASE_URL = "https://api.bankco.example/v1"

# BankCo raised the per-file limit from 100 to 250 transfers on 2026-09-16 (BANK-77).
MAX_TRANSFERS_PER_FILE = 250


def send_transfer(request_id, iban, amount_minor, currency, remittance):
    """Ask BankCo to pay `amount_minor` to `iban`. Returns BankCo's transfer id."""
    raise NotImplementedError


def split_into_files(transfers):
    """Split one payout run (a list of transfers) into the batch files BankCo accepts."""
    raise NotImplementedError
