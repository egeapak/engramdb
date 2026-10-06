from decimal import Decimal


def line_total(lines):
    x = 0
    for line in lines:
        x += line["quantity"] * line["unit_cents"]
    return x


def tax_for(amount_cents, rate):
    raise NotImplementedError
