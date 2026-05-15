-- A small snippets table the demo stores BGE-small embeddings into.
-- We use double precision[] (raw float8 arrays) for portability.
-- For production deployments pgvector is recommended.

CREATE SCHEMA IF NOT EXISTS demo;

CREATE TABLE IF NOT EXISTS demo.snippets (
  id        SERIAL PRIMARY KEY,
  text      TEXT NOT NULL,
  embedding DOUBLE PRECISION[]
);
