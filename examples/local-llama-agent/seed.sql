-- Seeds a small demo.tasks table the llama-cpp agent will read and write.

CREATE SCHEMA IF NOT EXISTS demo;

CREATE TABLE IF NOT EXISTS demo.tasks (
  id         SERIAL PRIMARY KEY,
  title      TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending',
  added_by   TEXT,
  added_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO demo.tasks (title, status, added_by) VALUES
  ('Buy groceries', 'pending', 'seed'),
  ('Write tests', 'done', 'seed')
ON CONFLICT DO NOTHING;
