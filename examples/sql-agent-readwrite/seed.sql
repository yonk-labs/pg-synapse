-- Seeds a small demo.notes table the agent will read and write.

CREATE SCHEMA IF NOT EXISTS demo;

CREATE TABLE IF NOT EXISTS demo.notes (
  id       SERIAL PRIMARY KEY,
  body     TEXT NOT NULL,
  added_by TEXT,
  added_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO demo.notes (body, added_by) VALUES
  ('Buy milk', 'seed'),
  ('Call mom', 'seed')
ON CONFLICT DO NOTHING;
