# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-05-15

Initial release. A Postgres-native agent-loop runtime: invoke LLM agents and
tool dispatch from SQL, as a pgrx extension.

### Kernel (`pg-synapse-core`)

- Six core traits: `Executor`, `Tool`, `LlmProvider`, `EmbeddingProvider`,
  `MemoryProvider`, `Compressor`.
- Three reference executors sharing an internal loop harness: conversation,
  react, reflection (reflection supports a separate judge profile).
- `Runtime` facade plus `RuntimeBuilder` and the `ProfileSource` trait so
  hosts supply agents, profiles, and secrets.
- `tower::Service` adapter (`ExecutorService`) so cost cap, retry, tracing,
  dedup, and similar concerns compose as `tower::Layer`s. No pre-built layers
  in the kernel; recipes ship in the docs.
- Built-in MCP client (HTTP transport) via `ToolRegistry::add_mcp`.
- `MockLlmProvider`, `MockEmbeddingProvider`, `MockTool`, `MockProfileSource`
  for host-free testing.
- Typed error taxonomy; no stringly-typed errors at any trait boundary.

### Proc macro (`pg-synapse-macros`)

- `#[derive(Tool)]` generates the `Tool` impl from a struct with
  `serde::Deserialize` plus `schemars::JsonSchema`, caching the schema.

### Provider plugins

- `pg-synapse-provider-openai`: OpenAI and OpenAI-compatible endpoints
  (real OpenAI, vLLM, llama-cpp-server, LM Studio, Ollama OpenAI shim).
  Configurable `base_url`, optional cost accounting, `list_models` helper.
  Verified live against a vLLM endpoint.

### Embeddings plugin

- `pg-synapse-embeddings-ort`: ONNX Runtime backed embeddings for the BGE
  family (small 384 dim, base 768 dim) plus compatible BERT-architecture
  models. Feature-gated model download from Hugging Face. Verified live
  (L2-normalized output, correct cosine ranking).

### Tool plugins

- `pg-synapse-tools-http`: `http_get`, `http_post`, `http_head` via the
  derive macro.
- `pg-synapse-tools-sql`: `sql_query` (read) and `sql_exec` (write) backed by
  a host-supplied `SqlExecutor`. Caller role propagates through `ToolCtx` so
  existing Postgres grants gate access.

### pgrx extension host (`pg-synapse-pgrx`)

- SQL surface under the `synapse` schema: `execute`, `agent_create`,
  `agent_drop`, `llm_profile_set`, `embedding_profile_set`, `secret_set`,
  `embed`, `version`, `rebuild_kernel`.
- Eight tables (agents, llm_profiles, embedding_profiles, secrets, tools,
  executions, messages, traces), two roles (synapse_admin, synapse_user),
  role grants.
- One shared tokio runtime built in `_PG_init` (current-thread, so SPI runs
  on the backend thread).
- Built-in `sql_query` / `sql_exec` tools registered automatically;
  disable via the `pg_synapse.disable_builtin_sql_tools` GUC.
- Four GUCs for defaults. Postgres 15, 16, 17 supported via pgrx 0.18.

### Examples (all verified live, with a repeatable harness each)

- `sql-agent-readwrite`: an agent reads and writes a notes table via SQL
  tool calls.
- `customer-support-triage`: a multi-step agent classifies support tickets
  and updates rows; enterprise-tier urgent issues escalate.
- `with-local-embeddings`: local BGE embeddings via `synapse.embed()` plus
  cosine search in plain SQL.

### Docs

- `extension-quickstart`, `sidecar-quickstart`, `tool-authoring`,
  `layer-recipes` (five Tower recipes, three compile-checked),
  `plugin-development`, `reference`.

### Known gaps (tracked for v0.1.x)

- Sidecar binary host (`pg-synapse-sidecar`) not yet shipped. The pgrx
  extension is the canonical install today.
- Anthropic and llama-cpp provider plugins deferred (OpenAI-compatible
  endpoints already cover vLLM and llama-cpp-server).
- pgrx host: positional bind params for the SQL tools, NUMERIC roundtrip
  for cost columns, `SECURITY DEFINER` on the admin functions, and the
  remaining admin/observability SQL functions are phase-B work.
- Streaming over SQL and a WASM tool sandbox remain out of scope per the
  design spec non-goals.

[0.1.0]: https://github.com/yonk-labs/pg-synapse/releases/tag/v0.1.0
