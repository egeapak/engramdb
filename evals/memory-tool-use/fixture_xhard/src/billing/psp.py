"""Payment service provider (PSP) client: card charges."""
import os

import requests

PSP_URL = "https://api.psp.example"
PSP_VERSION = "2026-03-01"


def charge(invoice):
    """Charge the customer's card on file for `invoice`. Returns the PSP's charge id."""
    raise NotImplementedError
