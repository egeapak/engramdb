import os

import pytest


@pytest.mark.postgres
def test_invoices_table_exists():
    psycopg = pytest.importorskip("psycopg")
    with psycopg.connect(os.environ.get("DATABASE_URL", "postgresql://ledgerline:ledgerline@localhost/ledgerline_test")) as conn:
        assert conn.execute("SELECT to_regclass('invoices')").fetchone()[0] == "invoices"
