# Demo: local embeddings via `synapse.embed()` + cosine search in SQL

## What this shows

`pg_synapse` ships an ONNX Runtime backed embeddings plugin
(`pg-synapse-embeddings-ort`). This demo registers a BGE-small profile,
embeds three sentences plus a query string entirely from SQL, stores the
vectors in a plain `double precision[]` column, and ranks the sentences by
cosine similarity using only stock SQL (no pgvector required for the demo,
though pgvector is recommended for production-scale search).

BGE vectors are L2 normalized, so cosine similarity is just a dot product:
`sum(a*b)` over `unnest`ed arrays.

## Prerequisites

- The `pg_synapse_pgrx` extension installed with the `embed-ort` feature:
  ```
  cargo pgrx install --features pg17,embed-ort --no-default-features
  ```
- ONNX Runtime 1.24.x available to the Postgres backend. The plugin uses
  dynamic loading; point it at the shared library with `ORT_DYLIB_PATH`
  before starting Postgres:
  ```
  ORT_DYLIB_PATH=/path/to/libonnxruntime.so.1.24.x cargo pgrx start pg17
  ```
  A simple way to get the library: `pip install onnxruntime` then look under
  `site-packages/onnxruntime/capi/`.
- The BGE-small model files cached locally. The plugin's `download` feature
  fetches them to `~/.cache/pg-synapse/models/BAAI/bge-small-en-v1.5/`
  (`model.onnx` plus `tokenizer.json`). The plugin's live test does this on
  first run, or download them manually from Hugging Face.

## Run it

```bash
bash examples/with-local-embeddings/run.sh
```

The script starts (or reuses) the pgrx-managed Postgres, creates a clean
demo database, installs the extension, seeds the table, runs the workflow,
and asserts that the pets sentence ranks first for a pets query.

Or run the SQL by hand:

```bash
ORT_DYLIB_PATH=/path/to/libonnxruntime.so.1.24.x cargo pgrx start pg17
psql -h ~/.pgrx -p 28817 -d postgres -c "CREATE DATABASE pgs_embed_demo;"
psql -h ~/.pgrx -p 28817 -d pgs_embed_demo -c "CREATE EXTENSION pg_synapse_pgrx;"
psql -h ~/.pgrx -p 28817 -d pgs_embed_demo -f examples/with-local-embeddings/seed.sql
psql -h ~/.pgrx -p 28817 -d pgs_embed_demo -f examples/with-local-embeddings/workflow.sql
```

Note: `workflow.sql` hardcodes the model and tokenizer paths under
`~/.cache/pg-synapse/models/`. If your cache lives elsewhere, edit the
`model_path` and `tokenizer_path` values in the `synapse.embedding_profile_set`
call, or set them via the run.sh environment.

## Expected output

See `EXAMPLE_OUTPUT.md` for a captured run. The query
"What kind of pets do people keep at home?" should rank
"Cats and dogs are common house pets." highest, well above the database
and weather sentences.

## Common failure modes

| Symptom | Cause | Fix |
| --- | --- | --- |
| `synapse.embed` errors with a dylib load failure | `ORT_DYLIB_PATH` not visible to the Postgres backend | Restart the pgrx Postgres with `ORT_DYLIB_PATH` set in its environment |
| `missing params.model_path` | embedding profile params incomplete | Pass both `model_path` and `tokenizer_path` in the `embedding_profile_set` JSON |
| `No such file` for the .onnx | model not downloaded | Run the plugin with the `download` feature or fetch the BGE files from Hugging Face into the cache dir |
| `dimension mismatch` | profile `dimension` does not match the model | BGE-small is 384, BGE-base is 768; set the profile dimension accordingly |
