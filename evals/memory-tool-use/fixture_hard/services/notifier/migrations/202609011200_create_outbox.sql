CREATE TABLE outbox (
    id TEXT PRIMARY KEY,
    template TEXT NOT NULL,
    recipent TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
