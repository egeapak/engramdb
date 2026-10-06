"""Load one settlement file into the billing database."""
import csv
import io

from .parse import parse_row


def load_file(name, data, db):
    """Parse settlement file `name` (its bytes are `data`) and insert its rows through `db`."""
    rows = [parse_row(r) for r in csv.DictReader(io.StringIO(data.decode("utf-8")))]
    db.insert_settlements(rows)
    return len(rows)
