"""Calls from ingest to the billing app."""
import os

import requests

BILLING_URL = os.environ.get("BILLING_INTERNAL_URL", "http://ledgerline.internal")


def post_settlement(record):
    """Report one settled payment (a record from parse_row) to billing."""
    raise NotImplementedError
