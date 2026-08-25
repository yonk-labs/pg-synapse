# NL2SQL with pg-lexicon schema context

This example proves the pg-lexicon ⇄ pg-synapse integration: a synapse agent
with **no schema knowledge** answers a natural-language question by fetching
schema + relationships from pg-lexicon, then writing and running the SQL.

## What's real vs stubbed

Real: the synapse agent runtime, the `get_schema_context` plugin
(`plugins/pg-synapse-tools-lexicon`), the authenticated HTTP call to
pg-lexicon, the returned `ContextPackage`, and the `sql_query` execution against
a seeded `ecommerce` database.

Stubbed: the LLM. `mock_llm.py` is a deterministic OpenAI-compatible endpoint
(no API key needed). It scripts a 3-step loop, but on the SQL turn it **parses
the real pg-lexicon context and builds the JOIN from the foreign key
pg-lexicon supplied**. If that FK were missing, it would fall back to a
single-table query and the demo's JOIN assertion would fail. Going live is a
base-URL + API-key swap in the LLM profile.

## The loop

```
NL question ("total revenue per customer")
  -> synapse agent (schema-free system prompt)
  -> get_schema_context tool  -> pg-lexicon POST /v1/context-packages (Bearer token)
  -> ContextPackage { tables, columns, orders.customer_id -> customers.id, ... }
  -> LLM writes: SELECT c.name, SUM(o.total_amount)::float8
                 FROM ecommerce.orders o JOIN ecommerce.customers c
                 ON o.customer_id = c.id GROUP BY c.name
  -> sql_query executes  -> correct revenue-per-customer rows
```

## Prerequisites (started outside this script)

1. Test DB up and ecommerce + synapse schema seeded (from the pg-lexicon repo:
   `scripts/test.sh` once, then the DB stays up on `localhost:5439`).
2. pg-lexicon crawled + serving with a token:
   ```
   PGLEXICON_STORE_URL=postgres://postgres:postgres@localhost:5439/pg_lexicon_dev \
   PGLEXICON_SERVE_TOKEN=demo-secret-token \
   pg-lexicon serve --addr 127.0.0.1:9777
   ```
3. Mock LLM: `python3 examples/nl2sql-lexicon/mock_llm.py 8991`
4. Sidecar pointed at the seeded DB + pg-lexicon:
   ```
   DATABASE_URL=postgres://postgres:postgres@localhost:5439/pg_agents_dev \
   PG_SYNAPSE_PORT=8089 PG_SYNAPSE_ADMIN_TOKEN=demo-admin-token \
   PG_LEXICON_BASE_URL=http://127.0.0.1:9777 PG_LEXICON_TOKEN=demo-secret-token \
   target/debug/pg-synapse-sidecar
   ```

Then: `bash examples/nl2sql-lexicon/run-demo.sh`

## Run against a REAL model (no mock)

Point an LLM profile at any OpenAI-compatible endpoint (vLLM, etc.) and use it
instead of the mock: the model itself writes the SQL:

```
H='X-PG-Synapse-Admin-Token: demo-admin-token'
curl -sX POST localhost:8089/v1/admin/profile/llm -H "$H" -d '{
  "name":"live","provider":"openai","model":"<model-id>",
  "base_url":"http://<host>:8000/v1","params":{"api_key":"sk-none","temperature":0.0}}'
curl -sX POST localhost:8089/v1/admin/agent -H "$H" -d '{
  "name":"shop_analyst_live","executor_name":"react","llm_profile_main":"live",
  "tools":["get_schema_context","sql_query"],
  "system_prompt":"You are a SQL analyst for a shop database on PostgreSQL. You do NOT know the schema in advance. FIRST call get_schema_context with the question; THEN write one PostgreSQL query using ONLY that context (schema-qualify tables) and call sql_query; then state the answer."}'
curl -sX POST localhost:8089/v1/execute -d '{"agent":"shop_analyst_live","input":"What is the total revenue per customer?"}'
```

Verified live with `gemma-4-26B` (vLLM): the schema-blind model fetched
context, wrote `SELECT c.id, c.name, SUM(o.total_amount)::float8 FROM
ecommerce.customers c JOIN ecommerce.orders o ON c.id = o.customer_id GROUP BY
c.id, c.name`, executed it, and returned the exactly-correct revenue, and
generalized to a different join path (products⋈reviews, AVG rating per
category) on a second question. The generated SQL is provable via
`pg_stat_statements` on the target DB.

## Note: synapse NUMERIC serialization fix

`sql_query` previously serialized Postgres `NUMERIC` as JSON `null`, so any
money/aggregate query returned nulls. Fixed in
`crates/pg-synapse-sidecar/src/db.rs` (`try_decode_column` now decodes
`BigDecimal` via the sqlx `bigdecimal` feature). Without this, aggregate NL2SQL
answers are wrong.
