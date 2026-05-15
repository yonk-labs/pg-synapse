# Captured run: local embeddings + cosine search

Captured 2026-05-15 against the pgrx-managed Postgres 17, extension built
with `--features pg17,embed-ort`, ONNX Runtime 1.24.4 via `ORT_DYLIB_PATH`,
BGE-small-en-v1.5 (384 dim) cached at
`~/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5/`.

## Setup

```
DROP DATABASE
CREATE DATABASE
CREATE EXTENSION
CREATE SCHEMA
CREATE TABLE
```

## Workflow

```
 embedding_profile_set
-----------------------

(1 row)

INSERT 0 3
```

Three sentences embedded and stored via `synapse.embed(...)` inline in the
`INSERT`. Then a query string is embedded and the rows ranked by cosine
similarity (dot product, since BGE vectors are L2 normalized):

Query: `What kind of pets do people keep at home?`

```
                         text                         | cosine_similarity
------------------------------------------------------+-------------------
 Cats and dogs are common house pets.                 |            0.7422
 Postgres is a relational database management system. |            0.3899
 The rain in Spain stays mainly on the plain.         |            0.3447
(3 rows)
```

## Result

The pets sentence ranks first by a wide margin (0.7422 vs 0.3899 and
0.3447), which is the semantically correct answer for a pets query. The
embedding path runs entirely inside Postgres: tokenization and ONNX
inference happen in-process in the pgrx host, and the cosine ranking is
plain SQL over `double precision[]` columns.

This confirms `synapse.embed()` works end-to-end against the local
ORT-backed BGE provider, and that L2-normalized BGE output makes dot-product
cosine ranking a one-liner in SQL.
