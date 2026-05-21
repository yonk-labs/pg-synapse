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

Status: v0.1.1 shipped, plus PS-4 (redacted diagnostics export) and PS-5
(provider conformance suite) on `main`. The pgrx extension and the
`pg-synapse-sidecar` binary host are both shipped and verified live.

## What works today

- An agent invoked from SQL that reads and writes tables via `sql_query` /
  `sql_exec` tool calls, verified end-to-end against a live LLM.
- Three LLM provider plugins: OpenAI-compatible (real OpenAI, vLLM,
  llama-cpp-server, LM Studio, Ollama shim), Anthropic Messages API,
  and a dedicated `llama-cpp` plugin with optional GGUF download.
- Local embeddings via ONNX Runtime (BGE family), callable as
  `SELECT synapse.embed('text', 'profile')`. llama-cpp-server also
  exposes an embedding provider.
- HTTP, SQL, filesystem, calc, clock, lede, and delegate tools, plus a
  `#[derive(Tool)]` macro and an MCP client.
- Three reference executors: conversation, react, reflection.
- `tower::Layer` composition for cost cap, retry, tracing, dedup (recipes in
  `docs/layer-recipes.md`).
- Two host paths: the `pg-synapse-pgrx` extension and the
  `pg-synapse-sidecar` axum binary (12 v1 HTTP endpoints, sqlx-backed,
  admin-token-gated).
- A hermetic provider conformance suite (`pg_synapse_core::testing`)
  with golden cassettes per provider and a drift check that catches
  silent serde-shape changes. See `docs/provider-conformance.md`.

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
crates/pg-synapse-sidecar   axum HTTP host (12 v1 endpoints, sqlx-backed)
plugins/pg-synapse-provider-openai      OpenAI / OpenAI-compatible
plugins/pg-synapse-provider-anthropic   Anthropic Messages API
plugins/pg-synapse-provider-llama-cpp   llama.cpp server + GGUF download
plugins/pg-synapse-embeddings-ort       ONNX Runtime embeddings (BGE)
plugins/pg-synapse-tools-http           http_get / http_post / http_head
plugins/pg-synapse-tools-sql            sql_query / sql_exec
plugins/pg-synapse-tools-fs             read_file / write_file / list_dir
plugins/pg-synapse-tools-calc           calculator
plugins/pg-synapse-tools-clock          get_current_time
plugins/pg-synapse-tools-delegate       call_agent (multi-agent)
plugins/pg-synapse-tools-lede           lede tool
plugins/pg-synapse-compaction           context window compaction
examples/                   runnable, verified workflows
docs/                       design spec, quickstarts, reference, recipes
```

## Documentation

- `docs/AGENTS-GUIDE.md` agent catalog: benchmark agents with schemas, sample code, and reactive triggers
- `docs/design.md` the design contract
- `docs/extension-quickstart.md` install and first agent (pgrx path)
- `docs/sidecar-quickstart.md` install and first agent (sidecar path)
- `docs/tool-authoring.md` three ways to add tools
- `docs/layer-recipes.md` five Tower middleware recipes
- `docs/plugin-development.md` writing a new plugin crate
- `docs/provider-conformance.md` wiring an LlmProvider to the PS-5 conformance suite
- `docs/reference.md` full SQL surface, types, tables, GUCs, errors
- `docs/decisions.md` locked v0.1 decisions
- `docs/MASTER-PLAN.md` waves and priorities
- `docs/NEXT-STEPS.md` what's queued after the last shipped commit

## License

Dual licensed under MIT or Apache-2.0, at your option.
