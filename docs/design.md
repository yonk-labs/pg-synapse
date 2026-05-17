# pg_synapse — Design Spec (v0.1)

- **Date:** 2026-05-13
- **Status:** Brainstorm-approved; awaiting operator final review before writing-plans
- **License:** MIT OR Apache-2.0 (dual)
- **Repo (planned):** `yonk-labs/pg-synapse`
- **MSRV:** Rust 1.85 (verify against pgrx requirements at writing-plans)
- **Postgres compatibility (v0.1):** 15, 16, 17

## TL;DR

`pg_synapse` is a Postgres-native agent-loop runtime that lets operators invoke LLM agents and tool dispatch from SQL — like stored procedures — via either a pgrx extension or a sidecar binary. The kernel is intentionally minimal: six traits (`Executor`, `Tool`, `LlmProvider`, `EmbeddingProvider`, `MemoryProvider`, `Compressor`), three reference executors (conversation / react / reflection), `tower::Service` + `Layer` middleware integration, MCP client, and a `Runtime` facade. Everything opinionated — memory, compression, RAG, graph search — lives in plugin crates, including dedicated integrations with sibling Yonk tools (stele, lede, chunkshop, pg-raggraph). The differentiator vs other Rust agent runtimes (Rig, Swiftide, AutoAgents) is Postgres-deployability + tower-middleware-shaped extensibility + typed tool args via derive macros.

---

## 1. Problem Statement

Rust developers building agent loops today face a forced choice between adopting Rig's framework opinions (the dominant Rust agent crate) or hand-rolling a 500-2,000 LOC kernel over `async-openai` (the path observed in `mini-agent`, `yoagent`, `sgr-agent`, `agent-io`, `Synaptic`, and others — each implementing roughly 80% of the same loop). None of these run inside Postgres. Operators who want agents to feel like stored procedures have no public-domain option; they either reinvent the runtime or move out of Postgres entirely.

`pg_synapse` fills the gap with a minimal kernel + Postgres deployment story + tower middleware — building the layer below Rig that the micro-crate community is rolling by hand, plus the Postgres-native deployment surface that no existing Rust agent crate provides.

## 2. Goals

- **G1:** Operators invoke agents from SQL with stored-proc UX. `SELECT pg_synapse.execute('agent_name', 'input text')` returns a JSONB result envelope.
- **G2:** Same kernel runs as a pgrx in-process extension AND as a sidecar binary (managed Postgres). Same SQL surface from the caller's perspective.
- **G3:** Kernel public surface fits on one screen — six traits + Runtime facade + three reference executors.
- **G4:** All cross-cutting concerns (cost cap, retry, tracing, rate-limit, MCP client, dedup, caching) are composable `tower::Layer`s. Zero pre-built Layers in the kernel; docs ship recipes.
- **G5:** Three tool-authoring paths in v0.1: `#[derive(Tool)]` macro, manual `Tool` trait, MCP client. All three register into the same `ToolRegistry`.
- **G6:** LLM providers are pluggable. v0.1 ships openai, anthropic, llama-cpp provider crates. Profiles in `pg_synapse.llm_profiles` are named, reusable, role-keyed (main / small / judge).
- **G7:** Embeddings are first-class. `SELECT pg_synapse.embed('text')` works directly from SQL via a registered `EmbeddingProvider`. Local embeddings via `pg-synapse-embeddings-ort` (ONNX Runtime).
- **G8:** Memory and context compression are plugin-only (no default impl in kernel). Yonk-ecosystem integrations (stele, lede) implement the traits and ship in their own repos. v0.1 agents are stateless across executions without one — documented loudly.
- **G9:** Auth via Postgres roles. Two roles: `pg_synapse_admin` (writes profiles, secrets, agents) and `pg_synapse_user` (calls execute / embed / list). SQL functions are `SECURITY DEFINER`; callers never touch the secrets table directly.
- **G10:** Apache-2.0 + MIT dual license. Workspace-wide semver. Sidecar HTTP API at `/v1/...` from day one.

## 3. Non-Goals (Explicit)

| Wanted thing | Why it's not in pg_synapse | Where it lives instead |
|---|---|---|
| RAG / chunking / retrieval | Domain-specific | `chunkshop-pg-synapse` plugin |
| Vector storage | pgvector exists | pgvector or any vector store |
| Agent memory (durable facts, sourcing, supersession) | Stele's domain | `stele-pg-synapse` plugin |
| Context compression | Lede's domain | `lede-pg-synapse` plugin |
| Graph-style retrieval | pg-raggraph's domain | `pg-raggraph-pg-synapse` plugin |
| Multi-agent crews | Strong opinion; downstream | v0.2+ `pg-synapse-crew` plugin |
| Workflow / DAG engine | Different abstraction | Downstream tool |
| NL2SQL pipeline | Domain-specific | Custom executor or tool plugin |
| Schema crawling / introspection tools | Domain-specific | Tool plugin |
| Cost ledger / billing materialized views | Use SQL aggregates over `executions.cost_usd` | Docs recipe |
| LLM / embedding fine-tuning | Out of scope | Future separate project |
| Inference engine | Candle / llama.cpp won that race | Provider plugins consume |
| Streaming over a SQL row return | Awkward in SQL semantics | v0.2 LISTEN/NOTIFY partials |
| Tool sandboxing via WASM | Heavy dep | v0.2 `pg-synapse-sandbox-wasm` |
| Auto-discovery of project files | Filesystem semantics off | Paste into `agents.system_prompt` or `agents.soul` |
| Pretty CLI / TUI | Out of scope | psql; observability via SQL |

The substrate boundary that makes this Non-Goals table binding is
formalized in ADR D14 (`decisions.md`), with a full capability
placement table in `pg-synapse-boundary-matrix.md`. When unsure whether
something belongs in core, consult that matrix.

## 4. Positioning

### Vs Rust agent-runtime landscape

| Crate | Stars (approx) | Role | Relationship to pg_synapse |
|---|---|---|---|
| Rig | ~6,400 | Framework | Bigger surface; pg_synapse is the layer below |
| Swiftide / swiftide-agents | ~628 | RAG-native | Different focus; not Postgres-deployable |
| AutoAgents | not surfaced | Actor-model | Heavier; Ractor-based |
| mistral.rs | ~5,000+ | Inference + agent | Bundles inference |
| Micro-crates (`mini-agent`, `yoagent`, etc.) | <100 each | Hand-rolled kernels | Target adopters |

`pg_synapse`'s distinct angles:
1. Postgres deployment (pgrx + sidecar) — no other Rust agent crate offers this.
2. `tower::Service` + `Layer` middleware — the differentiator the research flagged.
3. MCP-first tool interop.
4. Typed tool args via `#[derive(Tool)]`.
5. Six-trait kernel — everything opinionated is a plugin.

### Vs commercial / non-Rust frameworks

LangChain, LangGraph, LlamaIndex, Mastra, CrewAI, OpenAI Agents SDK, Anthropic Agent SDK all assume the host is a general-purpose Python/TS/Java/C# process. `pg_synapse` is the first agent runtime that's natively addressable from SQL. The market is also moving away from hosted-only options: OpenAI Assistants API deprecated August 2026, LangChain `AgentExecutor` EOL December 2026. The half-life of agent framework APIs is around 24 months; `pg_synapse` keeps its surface small to outlast that churn.

## 5. Architecture Overview

Three tiers: SQL surface → host (pgrx or sidecar) → kernel (`pg-synapse-core`) ← plugins register at startup.

```
+----------------------------------------------------------------+
|  SQL surface (the operator's view)                             |
|    pg_synapse.execute(...) / .execute_async(...) / .embed(...) |
|    pg_synapse.agent_create / .tool_register / .secret_set      |
+----------------------------------------------------------------+
                              |
              +---------------+---------------+
              |                               |
              v                               v
  +-------------------------+    +-----------------------------+
  |  pg-synapse-pgrx        |    |  pg-synapse-sidecar         |
  |  (in-process extension) |    |  (out-of-process binary)    |
  |  - One tokio runtime    |    |  - HTTP/JSON on /v1/...     |
  |    built in _PG_init    |    |  - libpq to Postgres        |
  |  - SPI for table reads  |    |  - install.sql wraps via    |
  |  - block_on for async   |    |    pg_net.http_post(...)    |
  +-----------+-------------+    +-------------+---------------+
              |                                |
              +---------------+----------------+
                              |
                              v
              +-----------------------------+
              |  pg-synapse-core (kernel)   |
              |  6 traits, 3 executors      |
              |  Runtime facade             |
              |  tower integration          |
              |  MCP client + mock LLM      |
              +--------------+--------------+
                             |
                             v
        +--------------------+---------------------+
        |  v0.1 plugin crates                      |
        |  - pg-synapse-provider-openai            |
        |  - pg-synapse-provider-anthropic         |
        |  - pg-synapse-provider-llama-cpp         |
        |  - pg-synapse-embeddings-ort             |
        |  - pg-synapse-tools-http                 |
        |                                          |
        |  Yonk-ecosystem plugins (in own repos):  |
        |  - stele-pg-synapse (memory)             |
        |  - lede-pg-synapse (compression)         |
        |  - chunkshop-pg-synapse (embeddings)     |
        |  - pg-raggraph-pg-synapse (graph)        |
        +------------------------------------------+
```

### Async stance

- Kernel: all traits are `async`. No sync variant.
- pgrx host: builds ONE shared tokio runtime in `_PG_init`, reuses for every SPI invocation via `runtime.block_on(future)`. Avoids per-execution-runtime bug observed in similar systems.
- Sidecar host: native async; awaits the kernel's futures directly inside its own tokio main.

### Workspace structure

```
pg-synapse/                                  (git repo)
├── Cargo.toml                               (cargo workspace)
├── LICENSE-APACHE
├── LICENSE-MIT
├── README.md
├── CHANGELOG.md
├── crates/
│   ├── pg-synapse-core/                     (kernel)
│   ├── pg-synapse-pgrx/                     (pgrx extension host)
│   └── pg-synapse-sidecar/                  (sidecar binary)
├── plugins/                                 (v0.1 in-workspace plugins)
│   ├── pg-synapse-provider-openai/
│   ├── pg-synapse-provider-anthropic/
│   ├── pg-synapse-provider-llama-cpp/
│   ├── pg-synapse-embeddings-ort/
│   └── pg-synapse-tools-http/
├── sql/
│   ├── pgrx-install.sql                     (generated by pgrx during build)
│   └── sidecar-install.sql                  (template; uses pg_net)
├── examples/
│   ├── minimal/
│   ├── with-tower-layers/
│   └── with-local-embeddings/
└── docs/
    ├── design.md                            (this document, evolved)
    ├── extension-quickstart.md
    ├── sidecar-quickstart.md
    ├── tool-authoring.md
    ├── layer-recipes.md
    ├── plugin-development.md
    └── reference.md
```

## 6. Kernel Public Surface (`pg-synapse-core`)

```rust
// EXECUTOR
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, ctx: ExecutionContext)
        -> Result<ExecutorOutcome, ExecutorError>;
}
// Kernel ships: ConversationExecutor, ReActExecutor, ReflectionExecutor
// (share an internal LoopHarness primitive)

// TOOL
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> &ToolSchema;
    async fn run(&self, input: serde_json::Value, ctx: &ToolCtx)
        -> Result<ToolOutput, ToolError>;
}
// + #[derive(Tool)] proc macro (pg-synapse-macros, re-exported)
// + MCP client adapter

pub struct ToolRegistry { /* ... */ }
impl ToolRegistry {
    pub fn add(&mut self, tool: impl Tool + 'static) -> &mut Self;
    pub async fn add_mcp(&mut self, server_url: &str)
        -> Result<&mut Self, McpError>;
}

// LLM PROVIDER
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompletionRequest)
        -> Result<CompletionResponse, LlmError>;
    async fn stream(&self, req: CompletionRequest)
        -> Result<BoxStream<CompletionChunk>, LlmError>;
    fn model_name(&self) -> &str;
}

pub trait LlmProviderFactory: Send + Sync {
    fn provider_name(&self) -> &str;   // e.g. "openai"
    fn build(&self, profile: LlmProfileRow)
        -> Result<Arc<dyn LlmProvider>, ProviderError>;
}

// EMBEDDING PROVIDER
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[&str])
        -> Result<Vec<Vec<f32>>, EmbeddingError>;
    fn dimension(&self) -> usize;
    fn model_name(&self) -> &str;
}

pub trait EmbeddingProviderFactory: Send + Sync {
    fn provider_name(&self) -> &str;
    fn build(&self, profile: EmbeddingProfileRow)
        -> Result<Arc<dyn EmbeddingProvider>, ProviderError>;
}

// MEMORY PROVIDER (no default impl; stele plugs in here)
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    async fn read(&self, scope: MemoryScope)
        -> Result<MemorySnapshot, MemoryError>;
    async fn write(&self, scope: MemoryScope, entry: MemoryEntry)
        -> Result<MemoryId, MemoryError>;
    async fn search(&self, scope: MemoryScope, query: &str, limit: usize)
        -> Result<Vec<MemoryEntry>, MemoryError>;
}

// COMPRESSOR (no default impl; lede plugs in here)
#[async_trait]
pub trait Compressor: Send + Sync {
    async fn compress(&self, messages: &[Message], budget: CompressionBudget)
        -> Result<Compressed, CompressionError>;
}

// PLUGIN
pub trait Plugin {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn register(self, registry: &mut Registry);
}

pub struct Registry {
    pub tools:                ToolRegistry,
    pub executors:            ExecutorRegistry,
    pub llm_factories:        LlmFactoryRegistry,
    pub embedding_factories:  EmbeddingFactoryRegistry,
    pub memory:               Option<Arc<dyn MemoryProvider>>,
    pub compressor:           Option<Arc<dyn Compressor>>,
}

// RUNTIME FACADE
pub struct Runtime { /* ... */ }
impl Runtime {
    pub fn builder() -> RuntimeBuilder;

    pub async fn execute(&self, agent_name: &str, input: &str)
        -> Result<ExecutorOutcome, RuntimeError>;

    pub async fn embed(&self, text: &str, model: Option<&str>)
        -> Result<Vec<f32>, RuntimeError>;
}

// Builder composition pattern:
// let rt = Runtime::builder()
//     .with_plugin(OpenAiProviderFactory::default())
//     .with_plugin(AnthropicProviderFactory::default())
//     .with_plugin(LlamaCppProviderFactory::default())
//     .with_plugin(OrtEmbeddingsFactory::default())
//     .with_plugin(SteleMemory::connect(stele_url))
//     .with_plugin(LedeCompressor::default())
//     .load_profiles_from(&pool)
//     .with_layer(MyCostCapLayer::new(10.0))
//     .with_layer(MyTracingLayer::default())
//     .build();

// TOWER INTEGRATION
// Each Executor is adapted to tower::Service<ExecuteRequest>.
// Users compose Layers via tower::ServiceBuilder.
// Kernel ships NO pre-built Layers; docs include 5 recipes (cost / retry /
// tracing / MCP client / dedup), each ~30-50 LOC of user code.

// ERROR MODEL
// Every trait returns a typed error implementing std::error::Error + Serialize.
// No String-typed errors at any trait boundary.
```

## 7. SQL Surface

### Schema (8 tables in `pg_synapse`)

| Table | Purpose |
|---|---|
| `agents` | Agent definitions: name, system_prompt, soul, executor_name, profile refs, tool_list, max_iter, timeout, cost_cap |
| `executions` | One row per agent run: id, agent_name, caller_role, input, output, status, started/ended_at, duration_ms, tokens_in/out, cost_usd, error, parent_execution_id |
| `messages` | Conversation history per execution: execution_id, seq, role, content, tool_call_id, tool_name, tool_input, tool_output |
| `tools` | Tool registry: name, description, schema_json, source_type (builtin / plugin / mcp), plugin_name, mcp_url, is_transactional |
| `traces` | Fine-grained event log: execution_id, seq, event_type, payload, duration_ms, ts |
| `secrets` | Admin-managed credentials: name, value (optionally pgcrypto-encrypted), encrypted, created/updated_at |
| `llm_profiles` | Named LLM configurations: name, provider, model, api_url, port, api_key_secret, headers, params, role_hint |
| `embedding_profiles` | Named embedding configurations: name, provider, model, api_url, port, api_key_secret, headers, dimension, params |

Full DDL in `sql/sidecar-install.sql`; pgrx generates the equivalent at extension build.

### LLM profile role-keyed references on `agents`

Three named roles in v0.1:

| Role | Used by | Fallback chain |
|---|---|---|
| `llm_profile_main` | Every executor | agent column → GUC `default_llm_profile_main` → ERROR |
| `llm_profile_small` | Classifiers, routers, intent detection, structured-output coercion | agent → GUC `default_llm_profile_small` → main |
| `llm_profile_judge` | `ReflectionExecutor` critique pass | agent → GUC `default_llm_profile_judge` → main |

Adding a fourth role (e.g., `vision`, `coder`) in a later release is one `ALTER TABLE` — non-breaking for existing agents.

### SQL functions (16 in v0.1)

| Function | Returns | Role | Purpose |
|---|---|---|---|
| `pg_synapse.execute(agent_name, input)` | `jsonb` | `pg_synapse_user` | Sync; blocks; full envelope |
| `pg_synapse.execute_async(agent_name, input)` | `uuid` | `pg_synapse_user` | Queues; returns execution_id |
| `pg_synapse.execution_status(execution_id)` | `jsonb` | `pg_synapse_user` | Poll async execution |
| `pg_synapse.embed(text, model_name DEFAULT NULL)` | `real[]` | `pg_synapse_user` | Direct embedding |
| `pg_synapse.tool_call(name, input)` | `jsonb` | `pg_synapse_user` | Invoke a tool without an agent |
| `pg_synapse.agent_create(config jsonb)` | `void` | `pg_synapse_admin` | Insert/upsert agent |
| `pg_synapse.agent_drop(name)` | `void` | `pg_synapse_admin` | Delete agent |
| `pg_synapse.agent_list()` | `SETOF jsonb` | `pg_synapse_user` | List agents |
| `pg_synapse.tool_register(name, schema_json, source jsonb)` | `void` | `pg_synapse_admin` | Register a tool |
| `pg_synapse.tool_list()` | `SETOF jsonb` | `pg_synapse_user` | List tools |
| `pg_synapse.secret_set(name, value)` | `void` | `pg_synapse_admin` | Write secret |
| `pg_synapse.secret_drop(name)` | `void` | `pg_synapse_admin` | Remove secret |
| `pg_synapse.llm_profile_set(name, config jsonb)` | `void` | `pg_synapse_admin` | Insert/upsert profile |
| `pg_synapse.llm_profile_drop(name)` | `void` | `pg_synapse_admin` | Drop profile |
| `pg_synapse.embedding_profile_set(name, config jsonb)` | `void` | `pg_synapse_admin` | Insert/upsert profile |
| `pg_synapse.embedding_profile_drop(name)` | `void` | `pg_synapse_admin` | Drop profile |

All functions are `SECURITY DEFINER`, owned by `pg_synapse_admin`, with explicit `GRANT EXECUTE` to the appropriate role.

### Result envelope

```jsonc
{
  "execution_id": "uuid",
  "status":       "completed",       // or "aborted" | "failed"
  "output":       "the assistant's final text",
  "tool_calls":   [ { "name": "...", "input": {...}, "output": {...}, "duration_ms": 12 } ],
  "tokens_in":    1234,
  "tokens_out":   567,
  "cost_usd":     0.0042,             // null if no pricing data registered
  "duration_ms":  823,
  "error":        null
}
```

### GUCs (10 in v0.1)

| GUC | Type | Default | Controls |
|---|---|---|---|
| `pg_synapse.default_llm_profile_main` | string | (none) | Fallback `main` profile |
| `pg_synapse.default_llm_profile_small` | string | (none) | Fallback `small` profile |
| `pg_synapse.default_llm_profile_judge` | string | (none) | Fallback `judge` profile |
| `pg_synapse.default_embedding_profile` | string | (none) | Fallback embedding profile |
| `pg_synapse.default_timeout_seconds` | int | 60 | Per-execution timeout fallback |
| `pg_synapse.default_cost_cap_usd` | real | (none) | Per-execution cost cap fallback |
| `pg_synapse.trace_enabled` | bool | true | Whether to write trace rows |
| `pg_synapse.sidecar_url` | string | (none) | If set, extension forwards to sidecar |
| `pg_synapse.master_key` | string | (none) | Pgcrypto master key for secret encryption |
| `pg_synapse.compression_threshold_tokens` | int | (none) | If Compressor registered, triggers compression above this |

## 8. Transactional Model

For each tool invocation during an agent execution:

1. Host issues `SAVEPOINT sp_<execution_id>_<call_seq>`.
2. Tool runs.
3. On success: `RELEASE SAVEPOINT sp_...`.
4. On error: `ROLLBACK TO SAVEPOINT sp_...`, record `tool_error` in `messages` + `traces`. Agent receives error in next turn; may retry/skip.

Cost cap hit: `RAISE EXCEPTION 'pg_synapse: cost cap exceeded ...'` propagates out of the entire `execute()` function. Caller's outer transaction is not rolled back automatically.

Per-tool opt-out: `pg_synapse.tools.is_transactional = FALSE` skips the savepoint. Pure-compute tools (HTTP fetch, embed, LLM call) set this; tools that mutate DB state leave it `TRUE`.

Sidecar parity: same savepoints, issued over libpq, with one connection per execution to keep savepoint scope local.

## 9. Auth Flow

- Functions are `SECURITY DEFINER`, owned by `pg_synapse_admin`.
- `pg_synapse_user` has `EXECUTE` on the user-facing functions and `SELECT` on `agents` / `tools` / `executions` / `messages` / `traces`.
- `pg_synapse_user` has no direct access to `secrets` / `llm_profiles` / `embedding_profiles`. Admin-write, function-mediated read.
- `execute()` captures `current_user` and writes to `executions.caller_role` for audit.
- Sidecar holds a privileged Postgres connection via `PG_SYNAPSE_DATABASE_URL`. The SECURITY DEFINER function passes `caller_role` in the HTTP payload to the sidecar; sidecar threads it through to `executions.caller_role`. Postgres remains the auth boundary.

## 10. Configuration Model

Order of precedence (most-specific wins):

1. Per-call override (executor-internal; not exposed to SQL caller in v0.1)
2. Per-agent column (e.g., `agents.llm_profile_main`, `agents.timeout_seconds`)
3. GUC default (e.g., `pg_synapse.default_llm_profile_main`)
4. Kernel hard default (only where one exists; some have no kernel default and error if unresolved)

Secrets are referenced by name; profile rows carry `api_key_secret` pointing at `secrets.name`. The runtime resolves secret values at execution time, optionally decrypting via pgcrypto if `pg_synapse.master_key` GUC is set.

## 11. Plugin Ecosystem

### v0.1 in-workspace plugins (published from `pg-synapse/`)

| Crate | Trait(s) implemented | Notes |
|---|---|---|
| `pg-synapse-provider-openai` | `LlmProviderFactory` | Cloud |
| `pg-synapse-provider-anthropic` | `LlmProviderFactory` | Cloud |
| `pg-synapse-provider-llama-cpp` | `LlmProviderFactory` (+ embedding via llama.cpp endpoint) | Local; features: `gpu`, `download`, `quantized` |
| `pg-synapse-embeddings-ort` | `EmbeddingProviderFactory` | Local; ORT; BGE / MiniLM / E5; features: `gpu`, `download` |
| `pg-synapse-tools-http` | A bundle of `Tool` impls (`http_get`, `http_post`, `http_head`) | Networked I/O |

### Yonk-ecosystem integration plugins (in target tool's own repo)

| Crate | Lives in | Implements | Ready when |
|---|---|---|---|
| `stele-pg-synapse` | stele repo | `MemoryProvider` | Stele's rebuild stabilizes memory API |
| `lede-pg-synapse` | lede repo | `Compressor` | v0.1-ready — lede has releases |
| `chunkshop-pg-synapse` | chunkshop repo | `EmbeddingProviderFactory` + retrieval `Tool` | chunkshop-rs stabilizes Rust API |
| `pg-raggraph-pg-synapse` | pg-raggraph repo | Graph-search `Tool`s | v0.1-ready |

pg_synapse's v0.1 release does NOT depend on any of these.

## 12. Tool Authoring Paths

### Path A — `#[derive(Tool)]`

```rust
#[derive(Tool, JsonSchema, Deserialize)]
#[tool(name = "summarize", description = "Produce a terse summary")]
struct Summarize {
    text: String,
    #[serde(default = "default_max")] max_sentences: u32,
}
fn default_max() -> u32 { 5 }

impl Summarize {
    async fn run(self, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(format!("Summary: ...")))
    }
}
```

### Path B — manual `Tool` trait

```rust
struct WeatherTool { api_client: Arc<WeatherApi> }

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str { "get_weather" }
    fn schema(&self) -> &ToolSchema { /* cached */ }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: WeatherArgs = serde_json::from_value(input)?;
        let resp = self.api_client.fetch(&args.location).await?;
        Ok(ToolOutput::json(serde_json::to_value(resp)?))
    }
}
```

### Path C — MCP client

```rust
registry.add_mcp("http://localhost:8765/mcp").await?;
```

Or from SQL:
```sql
SELECT pg_synapse.tool_register(
    'github_search',
    $${"type":"object","properties":{...}}$$::jsonb,
    $${"type":"mcp","url":"http://localhost:8765/mcp"}$$::jsonb
);
```

All three paths register into the same `ToolRegistry`. MCP tools are indistinguishable from local ones at the agent level.

## 13. Observability

- `pg_synapse.executions` row per run with timing, tokens, cost, status, error.
- `pg_synapse.messages` row per LLM message and tool turn.
- `pg_synapse.traces` row per fine-grained event (`llm_call`, `tool_call`, `layer_event`, `error`), with payload + duration_ms.
- All trace writes go through SPI (extension) or libpq (sidecar). Best-effort; write failures are logged to stderr but never break the run.
- Kernel uses the `tracing` crate for in-process logging. Sidecar honors `RUST_LOG`. Extension routes via Postgres's `ereport`.

Users query observability data directly:
```sql
SELECT agent_name, status, duration_ms, cost_usd
  FROM pg_synapse.executions
 WHERE started_at > now() - interval '1 hour'
 ORDER BY started_at DESC;
```

## 14. Testing Strategy

- Kernel unit tests: every trait has a contract test suite. Mock providers, mock tools.
- `MockLlmProvider` in `pg-synapse-core::testing`: public, ergonomic, scriptable response queue.
- Integration tests for pgrx host: load extension into a pgrx-managed test Postgres; run end-to-end `pg_synapse.execute()`; assert envelope shape and `executions` / `messages` / `traces` rows.
- Integration tests for sidecar: spawn sidecar against a test Postgres; HTTP POST `/v1/execute`; assert same.
- CI matrix: Rust stable × {pg15, pg16, pg17} for `pg-synapse-pgrx`; Rust stable only for `pg-synapse-core` and `pg-synapse-sidecar`.
- Live-API tests: opt-in feature flag `--features live-tests`. Skipped by default in CI; run pre-release.

## 15. Roadmap

### v0.1 (this spec)

- Kernel: 6 traits, 3 executors, Runtime, tower integration, MCP client, mock LLM
- Hosts: pgrx extension + sidecar binary
- 5 plugin crates: openai, anthropic, llama-cpp, ort-embeddings, tools-http
- SQL surface: 8 tables, 16 functions, 10 GUCs
- 2 Postgres roles
- 3 worked examples + docs

### v0.2

- LISTEN/NOTIFY partial-result streaming
- WASM tool sandbox plugin (`pg-synapse-sandbox-wasm`)
- Pre-built Layer plugin crate (`pg-synapse-layers-essentials`) with cost / retry / tracing
- More executors: Plan-and-Solve, ReWOO (as plugin crates)
- Decorator-style Layers: interactive, auto-select, learning-recorder (all as plugins)
- Yonk-ecosystem integrations land as upstream tools stabilize

### v0.3+ (track only)

- Multi-agent crews
- Tree-of-Thoughts executor
- More LLM provider plugins (Gemini, Mistral cloud, Cohere)
- Background-worker / cron integration

## 16. Risk Register

| Risk | Severity | Mitigation |
|---|---|---|
| pgrx version churn at v0.x boundaries | High | Pin pgrx; document supported Postgres versions per release; CI matrix |
| Nested tokio runtime panic in extension | High | One shared runtime built in `_PG_init`; regression test for "SPI hook re-entered from a tokio context" |
| Plugin ecosystem stays empty | High | Ship integrations with stele/lede/chunkshop/pg-raggraph as exemplars; reach out to Rust agent-runtime community |
| Tower learning curve for operators | Medium | Docs ship 5 recipe-style Layer examples (cost / retry / tracing / MCP / dedup) |
| `pg_net` not available on niche managed Postgres | Medium | Document fallback paths: `plpython3u`, `plperlu`, `dblink` |
| MCP spec evolves; integration breaks | Medium | Pin to one MCP revision per pg_synapse release; soft-fail on mismatch |
| Yonk-ecosystem integration plugins land late | Medium | Kernel doesn't depend on them; v0.1 ships fully functional |
| Sidecar wire protocol grows incompatibly | Medium | Version the HTTP path (`/v1/execute`); semver the binary |
| Cost calculation NULL handling confusion | Medium | `executions.cost_usd` is nullable; NULL means "no pricing data," not zero |
| Cargo / GitHub namespace collision | Low | Verify at writing-plans phase; fallback names ready |
| Async-only kernel locks out future sync hosts | Low | Don't preemptively add sync trait variant; revisit if demand surfaces |

## 17. Glossary

- **Kernel** — `pg-synapse-core` crate. Trait definitions + 3 executors + `Runtime` facade. No I/O.
- **Host** — Binary or extension that embeds the kernel. Two ship in v0.1: `pg-synapse-pgrx`, `pg-synapse-sidecar`.
- **Executor** — Implementation of `Executor` trait. A strategy / control-flow shape.
- **Tool** — Anything callable from an agent. Behind `Tool` trait. Authored via derive macro, manual impl, or MCP.
- **Provider** — Implementation of `LlmProvider` or `EmbeddingProvider`. Connects to a model backend.
- **Profile** — Named, reusable configuration row (`llm_profiles` or `embedding_profiles`) binding a provider to model + URL + secret + params.
- **Plugin** — Crate implementing one or more kernel traits; exposes a `Plugin` bundle for one-call registration.
- **Layer** — `tower::Layer` wrapping an `Executor`'s `Service` for cross-cutting concerns.
- **Soul** — Hermes-style identity prefix in `agents.soul`, prepended to the system prompt.
- **Role** (LLM) — `main`, `small`, or `judge`. An LLM profile is bound to a role per-agent.
- **Role** (Postgres) — `pg_synapse_admin` or `pg_synapse_user`. Auth boundary.

## 18. References

### Research package (companion docs at brainstorm time)

- `skill-output/research-and-design/Research-Summary-rust-agent-runtime.md`
- `skill-output/research-and-design/Research-Report-rust-agent-runtime.md`
- `skill-output/research-and-design/_oss-landscape-rust.md`
- `skill-output/research-and-design/_commercial-landscape.md`
- `skill-output/research-and-design/_pattern-literature.md`
- `skill-output/research-and-design/_user-workflow-pain.md`

### Public pattern citations

- Yao et al, 2022. *ReAct: Synergizing Reasoning and Acting in Language Models.* arXiv:2210.03629.
- Shinn et al, 2023. *Reflexion: Language Agents with Verbal Reinforcement Learning.* arXiv:2303.11366.
- Wei et al, 2022. *Chain-of-Thought Prompting Elicits Reasoning.* arXiv:2201.11903.
- Turpin et al, NeurIPS 2023. *Language Models Don't Always Say What They Think.* arXiv:2305.04388.
- Lanham et al, 2023. *Measuring Faithfulness in Chain-of-Thought Reasoning.* arXiv:2307.13702.
- Model Context Protocol spec, modelcontextprotocol.io/specification/2025-11-25.

### Public ecosystem references

- Rig: github.com/0xPlaygrounds/rig
- Swiftide: github.com/bosun-ai/swiftide
- Hermes Agent (Nous Research): hermes-agent.nousresearch.com/docs
- LangGraph: github.com/langchain-ai/langgraph
- Anthropic Agent SDK: code.claude.com/docs/en/agent-sdk/overview
- OpenAI Agents SDK: openai.github.io/openai-agents-python
- tower crate: docs.rs/tower
- pgrx: github.com/pgcentralfoundation/pgrx
- pg_net: github.com/supabase/pg_net

### Yonk ecosystem (sibling tools the integration plugins will target)

- stele — Sovereign source-backed memory layer for LLM agents
- chunkshop / chunkshop-rs — Chunking + embedding + tagging pipeline
- lede — Extractive summarization
- pg-raggraph — Graph-style search in Postgres

## 19. Open Items for Writing-Plans Phase

- Cargo / crates.io name availability check for `pg-synapse-*`
- GitHub namespace `yonk-labs/pg-synapse` confirmation
- pgrx version pin (verify against Postgres 15-17 compatibility)
- Decide whether `pg-synapse-embeddings-ort` ships in v0.1 alongside the four other plugins or moves to v0.2
- Exact JSON Schema dialect for `tools.schema_json` (draft-2020-12 vs draft-07)
- Exact `tracing` crate integration shape for `pg_synapse.traces.payload` JSONB
- Sidecar HTTP API: enumerate every endpoint (`/v1/execute`, `/v1/execute_async`, `/v1/status/{id}`, `/v1/embed`, `/v1/tool_call`)
- Whether sidecar emits Postgres LISTEN/NOTIFY on startup failures for early detection (defer or include in v0.1?)
