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

The full surface is verified from `#[pg_extern]` declarations in
`crates/pg-synapse-pgrx/src/sql_functions.rs` and the grant table in
`crates/pg-synapse-pgrx/sql/grants.sql`. The reactive-trigger functions
(enqueue, drain_queue, attach_agent_trigger, detach_agent_trigger) were
operator-approved and added in v0.1.1 N2.2 (ADR D14, 2026-05-17).

### Core execution

| Function | Signature | Description |
| --- | --- | --- |
| `execute` | `(agent_name text, input text) -> jsonb` | Run the agent synchronously; returns result envelope, never raises on agent error |
| `execute_async` | `(agent_name text, input text) -> uuid` | Enqueue and run (v0.1: inline, returns execution_id; true background worker is v0.2) |
| `execution_status` | `(execution_id uuid) -> jsonb` | Poll an execution by id; returns `{status, output, tokens_in, tokens_out, cost_usd, duration_ms}` or `{status:"not_found"}` |

`execute()` success envelope: `{execution_id, output, status, tokens_in,
tokens_out, cost_usd, duration_ms, tool_calls[]}`; error envelope:
`{error, status:"errored"}`.

### Agent and profile administration

| Function | Signature | Description |
| --- | --- | --- |
| `agent_create` | `(name text, system_prompt text, executor_name text, llm_profile_main text, tools text[], max_iterations int, timeout_ms bigint)` | Upsert an agent row; rebuilds the kernel cache |
| `agent_drop` | `(name text)` | Delete an agent row; rebuilds the kernel cache |
| `agent_list` | `() -> jsonb` | List all registered agents as a JSONB array of `{name, executor_name, llm_profile_main, tools}` |
| `llm_profile_set` | `(name text, provider text, model text, base_url text, api_key_secret text, params jsonb)` | Upsert an LLM profile; rebuilds the kernel cache |
| `llm_profile_drop` | `(name text)` | Delete an LLM profile; rebuilds the kernel cache |
| `embedding_profile_set` | `(name text, provider text, model text, dimension int, base_url text, params jsonb)` | Upsert an embedding profile; rebuilds the kernel cache |
| `embedding_profile_drop` | `(name text)` | Delete an embedding profile; rebuilds the kernel cache |
| `secret_set` | `(name text, value text)` | Upsert a secret; rebuilds the kernel cache |
| `secret_drop` | `(name text)` | Delete a secret; rebuilds the kernel cache |
| `rebuild_kernel` | `()` | Force a kernel-cache rebuild on the next `execute()` |

`base_url` and `api_key_secret` accept NULL.

### Tool utilities

| Function | Signature | Description |
| --- | --- | --- |
| `tool_register` | `(name text, description text, schema_json jsonb, kind text default 'manual', config jsonb default '{}')` | Upsert a row in `synapse.tools`; registry metadata only, does not create a plugin implementation |
| `tool_list` | `() -> jsonb` | List all registered tools as a JSONB array of `{name, description, kind}` |
| `tool_call` | `(tool_name text, input jsonb) -> jsonb` | Invoke a registered tool directly, bypassing the agent loop; for testing and operator introspection |
| `embed` | `(text text, profile_name text) -> double precision[]` | Embed text with the named (or default) profile; stores nothing |
| `version` | `() -> text` | Extension package version |

### Reactive triggers (ADR D14, operator-approved 2026-05-17)

These functions extend the `synapse.*` surface beyond v0.1.1 N2.2 for
reactive trigger support, approved by operator decision recorded 2026-05-17
(ADR D14).

| Function | Signature | Description |
| --- | --- | --- |
| `enqueue` | `(agent text, input text, source text default NULL) -> uuid` | Insert a `queued` row into `synapse.agent_queue`; returns the job_id. Fire-and-forget: the INSERT commits with the calling transaction and the LLM never blocks the writer. |
| `drain_queue` | `(max_jobs int default 10) -> int` | Claim up to `max_jobs` queued rows (FOR UPDATE SKIP LOCKED), run `synapse.execute` for each, write result/status back. Returns the count processed. Idempotent and concurrency-safe. In v0.1 the operator calls this on a schedule (pg_cron or a poller); a native background worker is the v0.2 upgrade. |
| `attach_agent_trigger` | `(target_table text, agent text, mode text default 'queue', events text default 'INSERT', when_sql text default NULL, input_expr text default 'NEW::text') -> void` | Generate a row-level AFTER trigger and trigger function on `target_table`. `mode='queue'`: calls `enqueue` (async). `mode='inline'`: calls `execute` synchronously inside the writing transaction and raises on error or `{"decision":"reject"}`, rolling the write back. Includes a `pg_trigger_depth() > 1` recursion guard. |
| `detach_agent_trigger` | `(target_table text) -> void` | Drop the trigger and trigger function previously created by `attach_agent_trigger` for `target_table`. |

## Tables (schema `synapse`)

| Table | Columns |
| --- | --- |
| `agents` | name (PK), system_prompt, soul, executor_name (def `conversation`), llm_profile_main, llm_profile_small, llm_profile_judge, embedding_profile, tools (text[]), max_iterations (def 10), timeout_ms (def 60000), cost_cap_usd, created_at, updated_at |
| `llm_profiles` | name (PK), provider, model, api_key_secret, base_url, params (jsonb), created_at, updated_at |
| `embedding_profiles` | name (PK), provider, model, dimension, api_key_secret, base_url, params (jsonb), created_at, updated_at |
| `secrets` | name (PK), value, created_at, updated_at |
| `tools` | name (PK), description, schema_json (jsonb), kind (def `builtin`), config (jsonb), created_at |
| `executions` | execution_id (PK uuid), agent_name, input, output, status, tokens_in, tokens_out, cost_usd, duration_ms, caller_role, started_at, finished_at |
| `messages` | execution_id (FK), seq, role, content, tool_call_id, tool_name, tool_input (jsonb), tool_output (jsonb), ts; PK (execution_id, seq) |
| `traces` | execution_id (FK), seq, event, payload (jsonb), ts; PK (execution_id, seq) |
| `agent_queue` | job_id (PK uuid, default gen_random_uuid()), agent (text), input (text), status (text, CHECK IN ('queued','running','done','error'), default 'queued'), result (jsonb), error (text), source (text, e.g. 'trigger:schema.table'), enqueued_at (timestamptz), started_at (timestamptz), finished_at (timestamptz) |

Roles: `synapse_admin` (full DML on all tables), `synapse_user` (SELECT on
`executions`, `messages`, `traces`, `agent_queue`). Both `NOLOGIN`.

## GUCs

| GUC | Type | Default |
| --- | --- | --- |
| `pg_synapse.disable_builtin_sql_tools` | bool | `false` |
| `pg_synapse.default_llm_profile_main` | string | unset (NULL) |
| `pg_synapse.default_timeout_ms` | int | `60000` (range 100 to i32::MAX) |
| `pg_synapse.default_max_iterations` | int | `10` (range 1 to 1000) |

All `Userset` context (per-session `SET`, `postgresql.conf`, or `ALTER
SYSTEM`).

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
