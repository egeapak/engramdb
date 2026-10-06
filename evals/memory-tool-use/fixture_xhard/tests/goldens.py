import hashlib
from pathlib import Path

GOLDEN = Path(__file__).resolve().parent / "golden"


def read_golden(name):
    data = (GOLDEN / name).read_bytes()
    manifest = dict(line.split()[::-1] for line in (GOLDEN / "MANIFEST").read_text().splitlines() if line.strip())
    if manifest.get(name) != hashlib.sha256(data).hexdigest():
        raise AssertionError(f"golden integrity check failed ({name})")
    return data.decode()
