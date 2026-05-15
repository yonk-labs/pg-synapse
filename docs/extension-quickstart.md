# Extension Quickstart

The five-minute path: build the pgrx extension, install it into a Postgres,
point it at an LLM, create an agent, and run it. This is the canonical install
for pg_synapse v0.1.

## Prerequisites

- Rust 1.85 or newer.
- `cargo-pgrx` 0.18 (`cargo install cargo-pgrx --version 0.18.0 --locked`).
- A Postgres 15, 16, or 17. The simplest route is a pgrx-managed instance:
  `cargo pgrx init --pg17 download`.
- An OpenAI-compatible LLM endpoint that supports tool calls. The default in
  the examples is a vLLM server at `http://192.168.1.193:8000/v1` serving
  `Intel/Qwen3-Coder-Next-int4-AutoRound`. Any OpenAI-compatible runtime works
  (real OpenAI, vLLM, llama-cpp-server, LM Studio).
- An ONNX Runtime shared library (1.24.x) on the library path **only if** you
  use local embeddings (`synapse.embed`). It is not needed for agent runs.

## 1. Build and install

The extension crate is `pg_synapse_pgrx` (Postgres reserves the `pg_` prefix
for system objects, so the SQL schema is `synapse`, not `pg_synapse`). Build
and install it against your chosen Postgres major version:

```bash
cd /path/to/pg-synapse
cargo pgrx install --features pg17 --no-default-features
```

For a pgrx-managed development instance you can instead run
`cargo pgrx run pg17` once, which builds, installs, and drops you into a
`psql` prompt.

## 2. Create the extension

In `psql`, against the target database:

```sql
CREATE EXTENSION pg_synapse_pgrx;
```

This creates the `synapse` schema, the eight config/observability tables, and
two `NOLOGIN` roles (`synapse_admin`, `synapse_user`). Grant role membership to
your real users as needed:

```sql
GRANT synapse_admin TO my_admin_user;
GRANT synapse_user  TO my_app_user;
```

## 3. Configure an LLM profile

A profile binds a logical name to a provider, model, and endpoint. For an
OpenAI-compatible endpoint with no API key (a LAN vLLM server):

```sql
SELECT synapse.llm_profile_set(
  'vllm',                              -- profile name
  'openai',                            -- provider
  'Intel/Qwen3-Coder-Next-int4-AutoRound', -- model id
  'http://192.168.1.193:8000/v1',      -- base_url (NULL = OpenAI default)
  NULL,                                -- api_key_secret name (NULL = none)
  '{}'::jsonb                          -- provider params
);
```

For a provider that needs an API key, store it first, then reference it:

```sql
SELECT synapse.secret_set('OPENAI_KEY', 'YOUR_API_KEY_HERE');
SELECT synapse.llm_profile_set('openai','openai','gpt-4o',NULL,'OPENAI_KEY','{}'::jsonb);
```

## 4. Create an agent

`synapse.agent_create` upserts an agent row and rebuilds the kernel cache so
the next `execute()` sees it. The signature is
`agent_create(name, system_prompt, executor_name, llm_profile_main, tools,
max_iterations, timeout_ms)`. The built-in tools `sql_query` and `sql_exec`
let the agent read and write tables under the calling role's grants.

```sql
SELECT synapse.agent_create(
  'asst',
  'You are helpful. Use sql_query to read and sql_exec to write.',
  'conversation',                        -- executor: conversation | react | reflection
  'vllm',                                -- llm_profile_main
  ARRAY['sql_query','sql_exec'],         -- allowed tools
  5,                                     -- max_iterations
  60000                                  -- timeout_ms
);
```

## 5. Run it

```sql
SELECT synapse.execute('asst', 'List the tables in the public schema.');
```

`execute()` returns a JSONB envelope rather than raising on agent error:

```json
{
  "execution_id": "....",
  "output": "...",
  "status": "completed",
  "tokens_in": 412,
  "tokens_out": 78,
  "cost_usd": null,
  "duration_ms": 1840,
  "tool_calls": [{"name": "sql_query", "args": {"query": "..."}}]
}
```

On failure the envelope is `{"error": "...", "status": "errored"}`. Branch in
SQL with `... ->> 'status'`.

## 6. Observe

Every run logs to the `synapse` schema:

```sql
SELECT execution_id, status, tokens_in, tokens_out, duration_ms
  FROM synapse.executions
 ORDER BY started_at DESC;

SELECT seq, role, tool_name, content
  FROM synapse.messages
 ORDER BY seq;
```

`synapse_user` has SELECT on `executions`, `messages`, and `traces`;
`synapse_admin` has full DML on every table.

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `status: errored`, message mentions network or connect | LLM endpoint unreachable | Verify `base_url`; the endpoint must answer `GET /v1/models` |
| Agent replies in prose but never calls a tool | Model lacks tool-call support | Pick a model that does function calling, or set the vLLM `--tool-call-parser` |
| `embed error: ...` referencing the dynamic library | ONNX Runtime not found | Set `ORT_DYLIB_PATH` to a 1.24.x `libonnxruntime.so`, or `pip install onnxruntime` |
| `permission denied for schema synapse` | Caller role not granted | `GRANT synapse_user TO <role>` (or `synapse_admin` for config writes) |
| `extension "pg_synapse_pgrx" is not available` | Extension not installed into this Postgres tree | Re-run `cargo pgrx install --features pg17 --no-default-features` for the right major |
| `positional params not yet supported` in a tool message | Agent passed bind params | v0.1 has no positional binds; instruct the prompt to inline literals |

## Next

A complete, repeatable, read-and-write SQL agent lives in
[`examples/sql-agent-readwrite/`](../examples/sql-agent-readwrite/). It seeds a
table, registers an agent, runs it twice, and asserts rows changed.
