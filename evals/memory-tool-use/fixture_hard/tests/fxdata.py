import json
from decimal import Decimal
from pathlib import Path

FX_DIR = Path(__file__).resolve().parent / "fixtures" / "fx"


def load_rates(day):
    with open(FX_DIR / f"{day}.json") as f:
        return {code: Decimal(rate) for code, rate in json.load(f)["rates"].items()}
