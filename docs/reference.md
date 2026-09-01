# Reference

Terse index for pg_synapse v0.1. Schema is `synapse`; GUC namespace is
`pg_synapse.*`; extension name is `pg_synapse_pgrx`.

## `pg-synapse-core` public types

### Traits

- `Executor` - `async execute(ExecutionContext) -> Result<ExecutorOutcome, ExecutorError>`.
- `Tool` - `name()`, `schema() -> &ToolSchema`, `async run(Value, &ToolCtx) -> Result<ToolOutput, ToolError>`.
- `LlmProvider` - `async complete()`, `async stream()`, `model_name()`.
- `LlmProviderFactory` - `provider_name()`, `build(LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError>`.
- `EmbeddingProvider` - `async embed(&[&str])`, `dimension()`, `model_name()`.
- `EmbeddingProviderFactory` - `provider_name()`, `build(EmbeddingProfileRow) -> Result<Arc<dyn EmbeddingProvider>, ProviderError>`.
- `MemoryProvider` - `async read(scope)`, `async write(scope, entry)`, `async search(scope, query, limit)`.
- `Compressor` - `async compress(&[Message], CompressionBudget) -> Result<Compressed, CompressionError>`.
- `Plugin` - `name()`, `version()`, `register(self, &mut Registry)`.
- `ProfileSource` - `async llm_profiles()`, `async embedding_profiles()`, `async agents()`, plus secret resolution.

### Structs / enums

- `Runtime` - operator-facing facade; `execute`, `execute_with_caller`, `embed`, `registry`, `agents`.
- `RuntimeBuilder` - `with_plugin`, `with_llm_profile`, `with_agent`, `with_secret`, `load_profiles_from`, `build`.
- `Registry` - aggregate: `tools`, `executors`, `llm_factories`, `embedding_factories`, `memory`, `compressor`.
- `ToolRegistry` - `add`, `add_arc`, `get`, `names`, `contains`, `async add_mcp(url)`.
- `ExecutorRegistry` / `LlmFactoryRegistry` / `EmbeddingFactoryRegistry` - name-keyed maps.
- `ExecutorService<E>` - the one `tower::Service` adapter (`ExecutionContext -> ExecutorOutcome`).
- `BoxedExecutorService` - `ExecutorService<dyn Executor>`; `service_from_dyn(Arc<dyn Executor>)`.
- `ConversationExecutor`, `ReActExecutor`, `ReflectionExecutor` - the three built-in executors.
- `ExecutionContext` - per-run input bundle (prompt, input, providers, tools, caps); `shallow_clone`.
- `ExecutorOutcome` - `output`, `messages`, `tool_calls`, `tokens_in/out`, `cost_usd`, `duration_ms`, `status`.
- `OutcomeStatus` - `Completed | MaxIterations | TimedOut | CostCapExceeded | Errored(String)`.
- `AgentRow`, `LlmProfileRow`, `EmbeddingProfileRow` - serde views of the config tables.
- `Message`, `Role` (`System|User|Assistant|Tool`) - conversation primitives.
- `CompletionRequest`, `CompletionResponse`, `CompletionChunk`, `ToolCall`, `ToolDefinition`, `Usage` - LLM I/O.
- `ToolSchema` - newtype over `schemars::RootSchema`; `from_root`, `from_json_value`, `as_root_schema`.
- `ToolOutput` - `Text(String) | Json(Value) | Empty`.
- `ToolCtx` - `execution_id`, `caller_role`, `agent_name`.
- `EmbeddingVector` - newtype over `Vec<f32>`; `dimension`, `as_slice`, `into_inner`.
- `MemoryId`, `MemoryScope`, `MemoryEntry`, `MemorySnapshot` - memory primitives.
- `Compressed`, `CompressionBudget` - compression primitives.
- `McpClient`, `McpTool`, `McpToolDef`, `McpServerInfo` - MCP HTTP client.
- `testing::{MockLlmProvider, MockEmbeddingProvider, MockTool, MockLlmFactory}`,
  `runtime::test_utils::MockProfileSource` - test doubles.

## SQL surface (`synapse.*`)

Verified against `pg_proc` on a live install rather than transcribed: 40
functions, with the security mode and grant each actually carries.

**Read the Security column.** `SECURITY INVOKER` means the body runs with the
caller's own privileges, which is how an agent is bounded by the role that
invoked it (F2). `SECURITY DEFINER` means it runs as `synapse_owner`, and the
grant is what decides who may call it at all. The two are independent: a
DEFINER function granted to nobody is unreachable, and an INVOKER function
granted to everyone is still bounded by each caller's own rights.

Grant column: **user** = `synapse_user` and `synapse_admin`; **admin** =
`synapse_admin` only; **owner** = granted to neither role, reachable only as
the extension owner or a superuser (internal plumbing, listed for completeness).

### Core execution

| Function | Signature | Security | Grant | Description |
| --- | --- | --- | --- | --- |
| `execute` | `(agent_name text, input text) -> jsonb` | INVOKER | user | Run the agent synchronously; returns a result envelope, never raises on agent error. The agent's SQL runs with the caller's privileges. |
| `execute_async` | `(agent_name text, input text) -> uuid` | INVOKER | user | Enqueue and run (v0.1: inline, returns execution_id; a true background worker is v0.2) |
| `execution_status` | `(execution_id uuid) -> jsonb` | DEFINER | user | Poll an execution by id; returns `{status, output, tokens_in, tokens_out, cost_usd, duration_ms}` or `{status:"not_found"}` |
| `ensure_kernel` | `() -> void` | DEFINER | user | Build this backend's kernel cache if it has not been built. Safe to grant by shape: no argument to steer it, no value returned. Called by the INVOKER entry points, which cannot do the config reads themselves. |
| `agent_trace_level` | `(p_agent text) -> text` | DEFINER | user | One agent's trace level. `synapse.agents` is not readable by `synapse_user`. |

`execute()` success envelope: `{execution_id, output, status, tokens_in,
tokens_out, cost_usd, duration_ms, tool_calls[]}`; error envelope:
`{error, status:"errored"}`.

### Agent and profile administration

| Function | Signature | Security | Grant | Description |
| --- | --- | --- | --- | --- |
| `agent_create` | `(name text, system_prompt text, executor_name text, llm_profile_main text, tools text[], max_iterations int, timeout_ms bigint)` | DEFINER | admin | Upsert an agent row; rebuilds the kernel cache |
| `agent_drop` | `(name text)` | DEFINER | admin | Delete an agent row; rebuilds the kernel cache |
| `agent_list` | `() -> jsonb` | DEFINER | user | List registered agents as `{name, executor_name, llm_profile_main, tools}` |
| `agent_set_trace_level` | `(name text, level text)` | DEFINER | owner | Set one agent's trace level |
| `llm_profile_set` | `(name text, provider text, model text, base_url text, api_key_secret text, params jsonb)` | DEFINER | admin | Upsert an LLM profile; rebuilds the kernel cache |
| `llm_profile_drop` | `(name text)` | DEFINER | admin | Delete an LLM profile; rebuilds the kernel cache |
| `embedding_profile_set` | `(name text, provider text, model text, dimension int, base_url text, params jsonb)` | DEFINER | admin | Upsert an embedding profile |
| `embedding_profile_drop` | `(name text)` | DEFINER | admin | Delete an embedding profile |
| `secret_set` | `(name text, value text)` | DEFINER | admin | Upsert a secret, encrypted when `pg_synapse.master_key` is set |
| `secret_drop` | `(name text)` | DEFINER | admin | Delete a secret |
| `rebuild_kernel` | `()` | DEFINER | admin | Mark the kernel cache stale; the next `execute()` rebuilds |
| `provider_capabilities` | `(profile_name text) -> jsonb` | DEFINER | owner | What a profile's provider supports, before assigning it to an agent |
| `purge_traces` | `(older_than_days int, agent_filter text) -> bigint` | DEFINER | owner | Delete executions older than N days, optionally for one agent |

`base_url` and `api_key_secret` accept NULL.

### Tool utilities

| Function | Signature | Security | Grant | Description |
| --- | --- | --- | --- | --- |
| `tool_register` | `(name text, description text, schema_json jsonb, kind text default 'manual', config jsonb default '{}')` | DEFINER | admin | Upsert a row in `synapse.tools`; registry metadata only, does not create a plugin implementation |
| `tool_list` | `() -> jsonb` | DEFINER | user | List registered tools as `{name, description, kind}` |
| `tool_call` | `(tool_name text, input jsonb) -> jsonb` | INVOKER | user | Invoke a registered tool directly, bypassing the agent loop. Reports tool failure in the return value rather than raising. |
| `embed` | `(text text, profile_name text) -> double precision[]` | DEFINER | user | Embed text with the named (or default) profile; stores nothing |
| `version` | `() -> text` | DEFINER | user | Extension package version |

### Reactive triggers (ADR D14, operator-approved 2026-05-17)

| Function | Signature | Security | Grant | Description |
| --- | --- | --- | --- | --- |
| `enqueue` | `(agent text, input text, source text default NULL) -> uuid` | DEFINER | user | Insert a `queued` row into `synapse.agent_queue`; returns the job_id. Fire-and-forget: the INSERT commits with the calling transaction and the LLM never blocks the writer. |
| `drain_queue` | `(max_jobs int default 10) -> int` | DEFINER | admin | Claim up to `max_jobs` queued rows (`FOR UPDATE SKIP LOCKED`), run each, write the result back. Bounded by `pg_synapse.max_concurrent_runs`. Idempotent and concurrency-safe. |
| `attach_agent_trigger` | `(target_table text, agent text, mode text default 'queue', events text default 'INSERT', when_sql text default NULL, input_expr text default 'NEW::text')` | INVOKER | admin | Generate a row-level AFTER trigger on `target_table`. `mode='queue'` calls `enqueue`; `mode='inline'` calls `execute` inside the writing transaction and raises on error or `{"decision":"reject"}`, rolling the write back. INVOKER on purpose: attaching a trigger is DDL against the caller's own table and should need the caller's own privileges. Includes a `pg_trigger_depth() > 1` recursion guard. |
| `detach_agent_trigger` | `(target_table text)` | INVOKER | admin | Drop the trigger and trigger function `attach_agent_trigger` created |

Inline mode is bounded by construction: `pg_synapse.inline_timeout_ms` caps the
run (only ever lowering the agent's own budget), and an agent holding a network
tool is refused inline before any lock is taken.

### Internal

Called by the extension, granted to neither role. Listed so the surface is
complete, not so you call them.

| Function | Security | Why it is not granted |
| --- | --- | --- |
| `config_agents`, `config_llm_profiles`, `config_embedding_profiles` | DEFINER | Kernel config reads. Reachable via `ensure_kernel`. |
| `config_secrets(names text[])` | DEFINER | **Granting this is `SELECT any_secret_you_like`.** It is why `ensure_kernel` exists in the shape it does. |
| `record_run(p jsonb)`, `record_status(p jsonb)` | DEFINER | The unguarded audit writers. Callers reach them through `audit_run` / `audit_status`. |
| `secret_value(secret_name text)` | INVOKER | Decrypts one secret. plpgsql, not sql, because the pgrx test harness has no pgcrypto and a sql body is parsed at creation. |
| `app_drop`, `tick`, `track_provenance`, `record_provenance` | INVOKER | pg-one app-model plumbing, driven by the harness. |

### Audit writers

| Function | Signature | Security | Grant | Description |
| --- | --- | --- | --- | --- |
| `audit_run` | `(payload jsonb, token text)` | DEFINER | user | Write a whole run's audit rows in one call |
| `audit_status` | `(payload jsonb, token text)` | DEFINER | user | Write one `executions` row at a given status, upserting |

Granted to caller roles because an INVOKER entry point must reach them, and
**the grant is not the authorisation**: each refuses any call that cannot
present a capability token an entry point minted for a run in flight and
retires when it ends. A caller invoking them directly is refused. Without that
the grant would just move audit forgery one function along. See
`crates/pg-synapse-pgrx/src/audit_capability.rs`.

## Tables (schema `synapse`)

### Kernel

| Table | Columns |
| --- | --- |
| `agents` | name (PK), system_prompt, soul, executor_name (def `conversation`), llm_profile_main, llm_profile_small, llm_profile_judge, embedding_profile, tools (text[]), max_iterations (def 10), timeout_ms (def 60000), cost_cap_usd, trace_level, created_at, updated_at |
| `llm_profiles` | name (PK), provider, model, api_key_secret, base_url, params (jsonb), created_at, updated_at |
| `embedding_profiles` | name (PK), provider, model, dimension, api_key_secret, base_url, params (jsonb), created_at, updated_at |
| `secrets` | name (PK), value, is_encrypted, created_at, updated_at |
| `tools` | name (PK), description, schema_json (jsonb), kind (def `builtin`), config (jsonb), created_at |

### Audit

| Table | Columns |
| --- | --- |
| `executions` | execution_id (PK uuid), agent_name, input, output, status, tokens_in, tokens_out, cost_usd, duration_ms, caller_role, model, started_at, finished_at |
| `messages` | execution_id (FK), seq, role, content, tool_call_id, tool_name, tool_input (jsonb), tool_output (jsonb), ts; PK (execution_id, seq) |
| `traces` | execution_id (FK), seq, event, payload (jsonb), ts; PK (execution_id, seq) |
| `row_provenance` | execution_id, table_schema, table_name, row_pk, op, written_at. Which run wrote a given row. |

### Queue

| Table | Columns |
| --- | --- |
| `agent_queue` | job_id (PK uuid), agent, input, status (CHECK IN `queued`/`running`/`done`/`error`, def `queued`), result (jsonb), error, source (e.g. `trigger:schema.table`), enqueued_at, started_at, finished_at |

### The pg-one app model

An app is a schema, an agent and a row here sharing one name. See
`docs/pg-one.md`.

| Table | Columns |
| --- | --- |
| `apps` | name (PK), title, description, schema_name, connection, created_at, updated_at |
| `app_agents` | app, agent. Which agents belong to which app. |
| `schedules` | schedule_id (PK), app, agent, input, every_interval, next_run_at, last_run_at, enabled, created_at |
| `questions` | app, name, nl_text, kind, sql_text, created_at, confirmed_at. A saved question, compiled once and re-run by Postgres. |
| `connections` | name (PK), host, port, dbname, user, password_secret, scan_json, scanned_at, reviewed_at, reviewed_by, created_at, updated_at. External databases, plus the brownfield scan and its human review gate. |

Roles: `synapse_admin` (full DML on all tables), `synapse_user` (SELECT on
`executions`, `messages`, `traces`, `agent_queue`, and nothing on `agents`,
`secrets` or either profile table). Both `NOLOGIN`. Functions are owned by
`synapse_owner`, a non-superuser, so a `SECURITY DEFINER` body cannot reach
`COPY ... FROM PROGRAM`.

## GUCs

All sixteen, with the context each is actually registered with.

| GUC | Type | Default | Context |
| --- | --- | --- | --- |
| `pg_synapse.disable_builtin_sql_tools` | bool | `false` | Userset |
| `pg_synapse.default_llm_profile_main` | string | unset | Userset |
| `pg_synapse.default_llm_profile_small` | string | unset | Userset |
| `pg_synapse.default_llm_profile_judge` | string | unset | Userset |
| `pg_synapse.default_embedding_profile` | string | unset | Userset |
| `pg_synapse.default_executor` | string | unset | Userset |
| `pg_synapse.default_timeout_ms` | int | `60000` (100 to i32::MAX) | Userset |
| `pg_synapse.default_timeout_seconds` | int | `60` (1 to i32::MAX/1000) | Userset |
| `pg_synapse.default_max_iterations` | int | `10` (1 to 1000) | Userset |
| `pg_synapse.default_cost_cap_usd` | string | unset (empty means no cap) | Userset |
| `pg_synapse.compression_threshold_tokens` | int | `0` (0 to i32::MAX) | Userset |
| `pg_synapse.trace_enabled` | bool | `true` | Userset |
| `pg_synapse.sidecar_url` | string | unset | Userset |
| `pg_synapse.inline_timeout_ms` | int | `2000` (1 to i32::MAX) | **Suset** |
| `pg_synapse.max_concurrent_runs` | int | `4` (1 to 1024) | **Suset** |
| `pg_synapse.master_key` | string | unset | **Suset + SUPERUSER_ONLY** |

Most are `Userset`: settable per session, in `postgresql.conf`, or with
`ALTER SYSTEM`. Three are not, and the difference is deliberate.

`inline_timeout_ms` and `max_concurrent_runs` are `Suset` because they bound
what one session can do to everyone else, so they are the operator's to set
rather than the caller's.

`master_key` is additionally `SUPERUSER_ONLY`, which governs **reading** it and
not just setting it. `Suset` alone would leave it readable: with default flags
`SELECT current_setting('pg_synapse.master_key')` returned the key through an
ordinary `sql_query` tool call, as `synapse_user`, in one line. Per-caller
isolation does not help there, because a GUC read is not a table read and no
grant governs it. See `docs/threat-model.md` for what encryption at rest does
and does not buy.

## Error enums (`pg_synapse_core::error`)

### `ExecutorError`

- `MaxIterationsReached(u32)` - iteration cap hit without termination.
- `Timeout(u64)` - wall-clock budget exceeded (ms).
- `CostCapExceeded { cap, spent }` - USD cap tripped mid-run.
- `Tool(ToolError)` - a tool invocation failed (`#[from]`).
- `Llm(LlmError)` - an LLM call failed (`#[from]`).
- `Provider(ProviderError)` - provider hydration failed (`#[from]`).
- `Memory(MemoryError)` - memory backend call failed (`#[from]`).
- `Compression(CompressionError)` - compressor call failed (`#[from]`).
- `Internal(String)` - executor invariant violation.

### `ToolError`

- `NotFound { name }` - no tool registered under that name.
- `InvalidInput { name, reason }` - input did not match the schema.
- `Execution { name, reason }` - tool ran but returned an error.
- `Timeout { name, timeout_ms }` - tool exceeded its runtime.
- `Mcp(String)` - MCP handshake / transport / framing failure.

### `LlmError`

- `Provider { provider, reason }` - provider-specific error.
- `Auth(String)` - authentication/authorization failure.
- `RateLimited { provider, retry_after_ms }` - rate limited.
- `ContextLengthExceeded { model, tokens, limit }` - request over context window.
- `Network(String)` - DNS/TCP/TLS/HTTP failure.

### `EmbeddingError`

- `Provider(String)` - provider-specific embedding failure.
- `DimensionMismatch { expected, actual }` - vector dimension mismatch.
- `Network(String)` - network failure.

### `MemoryError`

- `Backend(String)` - backend-specific failure.
- `InvalidScope(String)` - scope not understood by the backend.
- `NotFound` - requested entry not found.

### `CompressionError`

- `BudgetTooSmall { needed, available }` - budget cannot fit the input.
- `Backend(String)` - backend-specific failure.

### `ProviderError`

- `NotRegistered(String)` - no factory matched the profile's provider.
- `InvalidProfile { provider, reason }` - factory rejected the profile.
- `SecretNotFound(String)` - referenced secret name does not exist.

### `RuntimeError`

- `AgentNotFound(String)` - agent name not registered.
- `Executor(ExecutorError)` - executor returned an error (`#[from]`).
- `Config(String)` - missing/invalid configuration.
- `Provider(ProviderError)` - provider hydration failed (`#[from]`).

All error enums implement `std::error::Error + Send + Sync + 'static` and
`serde::Serialize + serde::Deserialize`.
