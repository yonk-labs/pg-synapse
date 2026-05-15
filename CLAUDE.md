# CLAUDE.md

Guidance for AI agents working in this repository.

## What this is

`pg_synapse` is a Postgres-native agent-loop runtime in Rust. Agents are
invoked from SQL (`SELECT synapse.execute(...)`), run a tool-calling loop
against an LLM, and their tools can read/write Postgres. Built clean-room
from public sources.

## Hard rules

- **No em-dashes** anywhere: source, docs, tests, commit messages. Use
  period, comma, parens, colon, or "to" for ranges. Grep before committing.
- **No `unsafe`** in the kernel or plugins. `#![forbid(unsafe_code)]` is set;
  the pgrx host is the only place `unsafe` could appear (pgrx FFI) and it is
  currently clean.
- **Typed errors only.** No `Result<_, String>` or `Result<_, Box<dyn Error>>`
  at any trait boundary. Extend the error enums in
  `crates/pg-synapse-core/src/error.rs`.
- **Quality gates before any commit lands:** `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (excluding `--features live-tests`).
- **Surgical changes.** Do not refactor adjacent code while implementing a
  task. Note unrelated issues, do not fix them inline.

## Schema naming gotcha

The SQL schema is `synapse`, NOT `pg_synapse`. Postgres reserves the `pg_`
prefix for system catalogs and rejects schema/role names that start with it.
The GUC namespace keeps the `pg_synapse.*` prefix (custom GUC prefixes are
unrestricted). See `crates/pg-synapse-pgrx/NOTES.md` for the full list of
pgrx 0.18 API adaptations.

## Architecture

Three tiers: SQL surface to host (pgrx) to kernel, with plugins registered
at startup.

- `crates/pg-synapse-core`: the kernel. Six traits (`Executor`, `Tool`,
  `LlmProvider`, `EmbeddingProvider`, `MemoryProvider`, `Compressor`),
  `Runtime` + `RuntimeBuilder`, `ProfileSource` trait, `Plugin`/`Registry`,
  three executors, `ExecutorService` tower adapter, MCP client. No I/O lives
  here; hosts supply the runtime, DB, and network.
- `crates/pg-synapse-macros`: `#[derive(Tool)]` proc macro.
- `crates/pg-synapse-pgrx`: the extension host. `_PG_init` builds ONE shared
  current-thread tokio runtime; SPI runs on the backend thread. SQL functions
  live in a `#[pg_schema] mod synapse`. `SpiProfileSource` and
  `SpiSqlExecutor` bridge the kernel to Postgres.
- `plugins/*`: providers and tools. Each implements `Plugin` and a factory
  trait. `provider-openai` and `tools-http` are the canonical examples.

## Conventions

- Plugins live under `plugins/`, kernel crates under `crates/`.
- Path deps carry `version = "0.1.0"` so crates.io publish works (see
  `docs/PUBLISHING.md`). When bumping the workspace version, the path-dep
  version fields move with it.
- Tests: `wiremock` for HTTP-backed plugins; the kernel's mock providers
  (`MockLlmProvider`, `MockProfileSource`, etc.) for in-memory tests; live
  tests are gated behind `--features live-tests` and skip when the endpoint
  env var is unset.
- Examples are repeatable: each `examples/*/run.sh` stands up a clean DB,
  installs the extension, seeds, runs, asserts, and the captured output is
  committed as `EXAMPLE_OUTPUT.md`.

## Testing the pgrx host

`cargo-pgrx` and the `pgrx` crate version must match. Project pins
`pgrx = "=0.18.0"`; install the matching binary:
`cargo install --locked cargo-pgrx --version 0.18.0`. If `cargo pgrx`
errors with a 0.17 path, the binary drifted; reinstall.

- `cargo pgrx test --package pg-synapse-pgrx pg17` runs the in-harness tests.
- For embeddings, the Postgres backend needs ONNX Runtime visible. Restart
  the pgrx Postgres with `ORT_DYLIB_PATH=/path/to/libonnxruntime.so.1.24.x
  cargo pgrx start pg17`. Models cache at `~/.cache/pg-synapse/models/`.
- Test Postgres runs via the cargo-pgrx managed install on port 28817,
  socket dir `~/.pgrx`. Docker is also available for isolation.

## LLM test endpoint

Default test LLM is an OpenAI-compatible vLLM at
`http://192.168.1.193:8000/v1`. Override with `PG_SYNAPSE_TEST_LLM_BASE_URL`
and `PG_SYNAPSE_TEST_LLM_MODEL`. The model must support function-calling for
the agent demos to work.

## Module map

| Path | Responsibility |
| --- | --- |
| `crates/pg-synapse-core/src/error.rs` | error taxonomy |
| `crates/pg-synapse-core/src/types/` | data types |
| `crates/pg-synapse-core/src/executor/` | 3 executors + loop harness + tower service |
| `crates/pg-synapse-core/src/tool/` | Tool trait, ToolRegistry, MCP client |
| `crates/pg-synapse-core/src/runtime/` | Runtime, RuntimeBuilder, ProfileSource |
| `crates/pg-synapse-core/src/plugin/` | Plugin trait, Registry |
| `crates/pg-synapse-core/src/testing/` | mock providers |
| `crates/pg-synapse-pgrx/src/sql_functions.rs` | the `synapse.*` SQL surface |
| `crates/pg-synapse-pgrx/src/spi_executor.rs` | SpiProfileSource + SpiSqlExecutor |
| `crates/pg-synapse-pgrx/src/schema.rs` | GUC registration |
| `crates/pg-synapse-pgrx/sql/schema.sql` | tables, roles, grants |

## Out of scope (design non-goals)

RAG/chunking, vector storage, durable agent memory, context compression,
graph retrieval, multi-agent crews, NL2SQL, streaming over SQL, WASM tool
sandbox. These are plugin or future-version concerns. Do not add them to the
kernel.
