"""Parse rows of a PSP settlement file."""
from decimal import Decimal

# Minor-unit exponent per currency; every other currency has 2.
EXPONENTS = {"JPY": 0, "KWD": 3, "BHD": 3}


def exponent(currency):
    return EXPONENTS.get(currency, 2)


def parse_amount(text, currency):
    """Turn an amount string in major units ("12.50") into integer minor units (1250)."""
    raise NotImplementedError


def parse_row(row):
    """One settlement-file row (a dict from csv.DictReader) -> a settlement record."""
    return {
        "psp_reference": row["reference"],
        "invoice_id": row["merchant_reference"],
        "currency": row["currency"],
        "amount": parse_amount(row["amount"], row["currency"]),
        "status": row["status"],
    }
