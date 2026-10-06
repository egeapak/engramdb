CREATE TABLE refunds (id TEXT PRIMARY KEY, invoice_id TEXT REFERENCES invoices(id), refunded_cents BIGINT);
