CREATE TABLE invoice_lines (invoice_id TEXT REFERENCES invoices(id), quantity INT, unit_cents BIGINT);
