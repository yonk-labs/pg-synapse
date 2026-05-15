-- End-to-end demo of synapse.embed() against the local ORT-backed BGE-small.
--
-- Prereqs:
--   1. CREATE EXTENSION pg_synapse_pgrx;     (with embed-ort feature on)
--   2. \i examples/with-local-embeddings/seed.sql
--   3. The BGE-small ONNX model + tokenizer cached locally. The pg-synapse
--      embeddings-ort plugin's `download` feature creates them under
--      ~/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5/.

SELECT synapse.embedding_profile_set(
  'bge-small',
  'ort',
  'BAAI/bge-small-en-v1.5',
  384,
  NULL,
  jsonb_build_object(
    'model_path',     '/home/yonk/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5/model.onnx',
    'tokenizer_path', '/home/yonk/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5/tokenizer.json'
  )
);

-- Embed three short snippets. BGE outputs L2-normalized vectors, so cosine
-- similarity is just a dot product.
INSERT INTO demo.snippets (text, embedding) VALUES
  ('The rain in Spain stays mainly on the plain.',
   synapse.embed('The rain in Spain stays mainly on the plain.', 'bge-small')),
  ('Postgres is a relational database management system.',
   synapse.embed('Postgres is a relational database management system.', 'bge-small')),
  ('Cats and dogs are common house pets.',
   synapse.embed('Cats and dogs are common house pets.', 'bge-small'));

-- Embed a query and rank the snippets by cosine similarity.
WITH q AS (
  SELECT synapse.embed('What kind of pets do people keep at home?', 'bge-small') AS vec
)
SELECT s.text,
       round(((SELECT sum(a*b) FROM unnest(s.embedding, q.vec) AS u(a, b)))::numeric, 4) AS cosine_similarity
FROM demo.snippets s, q
ORDER BY cosine_similarity DESC;
