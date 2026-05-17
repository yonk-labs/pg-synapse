# pg_synapse

A Postgres-native agent-loop runtime in Rust. Invoke LLM agents and tool
dispatch from SQL, like stored procedures:

```sql
SELECT synapse.execute('notes_agent', 'Add a note that says "Hello"');
```

The agent runs a real tool-calling loop against an LLM, and its tools can
read and write Postgres tables. The kernel is intentionally small: six
traits, three reference executors, a `tower` middleware seam, a built-in MCP
client, and a `Runtime` facade. Everything opinionated (providers, memory,
compression, embeddings) is a plugin.

Status: v0.1.0. The pgrx extension host is shipped and verified live. The
sidecar binary host is planned for v0.1.x.

## What works today

- An agent invoked from SQL that reads and writes tables via `sql_query` /
  `sql_exec` tool calls, verified end-to-end against a live LLM.
- OpenAI-compatible LLM provider (real OpenAI, vLLM, llama-cpp-server,
  LM Studio, Ollama OpenAI shim).
- Local embeddings via ONNX Runtime (BGE family), callable as
  `SELECT synapse.embed('text', 'profile')`.
- HTTP tools, a `#[derive(Tool)]` macro, and an MCP client.
- Three reference executors: conversation, react, reflection.
- `tower::Layer` composition for cost cap, retry, tracing, dedup (recipes in
  `docs/layer-recipes.md`).

## Quick start

Prerequisites: Rust 1.85+, `cargo-pgrx` 0.18, a Postgres 15/16/17, and an
OpenAI-compatible LLM endpoint. ONNX Runtime 1.24.x only if you want local
embeddings.

```bash
cargo install --locked cargo-pgrx --version 0.18.0
cargo pgrx init                       # one-time, sets up managed Postgres
cargo pgrx install --features pg17 --no-default-features
```

Then in psql:

```sql
CREATE EXTENSION pg_synapse_pgrx;

SELECT synapse.llm_profile_set(
  'llm', 'openai', '<model>', 'http://your-endpoint:8000/v1', NULL, '{}'::jsonb);

SELECT synapse.agent_create(
  'asst', 'You are a helpful assistant that can query the database.',
  'conversation', 'llm', ARRAY['sql_query','sql_exec'], 5, 60000);

SELECT synapse.execute('asst', 'How many rows are in public.users?');
SELECT seq, role, tool_name, content FROM synapse.messages ORDER BY seq;
```

Full walkthrough: `docs/extension-quickstart.md`.

## Examples

Each example has a repeatable `run.sh` and a captured `EXAMPLE_OUTPUT.md`.

- `examples/sql-agent-readwrite/` agent reads and writes a notes table
- `examples/customer-support-triage/` multi-step ticket triage agent
- `examples/with-local-embeddings/` BGE embeddings plus cosine search in SQL

See `examples/README.md` for the index.

## Workspace layout

```
crates/pg-synapse-core      kernel: traits, executors, Runtime, MCP, tower seam
crates/pg-synapse-macros    #[derive(Tool)] proc macro
crates/pg-synapse-pgrx      Postgres extension host (pgrx)
plugins/pg-synapse-provider-openai    OpenAI-compatible LLM provider
plugins/pg-synapse-embeddings-ort     ONNX Runtime embeddings (BGE)
plugins/pg-synapse-tools-http         http_get / http_post / http_head
plugins/pg-synapse-tools-sql          sql_query / sql_exec
examples/                   three runnable, verified workflows
docs/                       design spec, quickstarts, reference, recipes
```

## Documentation

- `docs/AGENTS-GUIDE.md` agent catalog: all 6 benchmark agents with schemas, sample code, and reactive triggers
- `docs/design.md` the design contract
- `docs/extension-quickstart.md` install and first agent
- `docs/tool-authoring.md` three ways to add tools
- `docs/layer-recipes.md` five Tower middleware recipes
- `docs/plugin-development.md` writing a new plugin crate
- `docs/reference.md` full SQL surface, types, tables, GUCs, errors
- `docs/decisions.md` locked v0.1 decisions (D1 to D13)

## License

Dual licensed under MIT or Apache-2.0, at your option.
