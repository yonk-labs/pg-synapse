# pg_synapse substrate boundary matrix

Companion to ADR D14 (`decisions.md`). Each generic agent-runtime
capability category is assigned to exactly one of:

- **core** - belongs in `pg-synapse-core` (it is substrate: a trait,
  the loop, the Runtime facade, or a host concern).
- **plugin** - a crate built against the kernel traits, opt-in.
- **product facade** - a higher product layer built ON pg-synapse; not
  this repo's concern at all.

Clean-room: categories are generic. This doc names no private codebase
and reads none; the list is the project's own design spec + backlog.

| Capability category | Placement | Rationale |
|---|---|---|
| Agent loop (LLM call to tool dispatch to continue/stop) | **core** | This IS the substrate. `Executor` trait + three reference executors. |
| Tool invocation + registry | **core** | `Tool` trait + `ToolRegistry`. The universal seam. |
| LLM / embedding provider abstraction | **core** (trait) / **plugin** (each provider) | Traits are substrate; every concrete provider (openai, anthropic, llama-cpp) is a plugin crate. No provider is a core dep (ADR D14). |
| Runtime facade + agent/profile resolution | **core** | `Runtime` + `ProfileSource`. Host-agnostic wiring. |
| Postgres deployment (extension + sidecar) | **core** (host crates) | The reason the project exists; both hosts wrap the same kernel. |
| `tracing` to `synapse.traces` event persistence | **core** (writer) | Schema decided (D6); the writer is a host concern, not a redesign. Persisted + pollable only; live push is v0.2 (D8). |
| Reliability: jittered retry, Retry-After, overflow to compact to retry | **plugin / opt-in wrapper** | G4: zero prebuilt tower layers in core. Ships feature-gated or as an explicit composable wrapper, never a silent core default. |
| Context compaction / compression | **plugin** | G8. `DefaultCompressor` is `plugins/pg-synapse-compaction`, not core. Core only defines the `Compressor` trait. |
| Durable agent memory (facts, sourcing, supersession) | **plugin** | G8. `MemoryProvider` trait in core; implementations are plugin crates. |
| RAG / chunking / retrieval | **plugin** | Domain-specific; a retrieval plugin. |
| Vector storage | **product facade / external** | pgvector or any vector store; not the runtime's job. |
| Graph / DAG workflow orchestration | **plugin** (executor) or **product facade** | A graph is a custom `Executor` plugin; a full workflow engine is a product layer. ADR D14: never core. |
| Multi-agent crews / handoff / orchestrator | **plugin** | The agent-as-tool delegation tool (`call_agent`) is the seam; crews are a downstream plugin. |
| NL2SQL pipeline | **product facade** | Domain-specific product capability built ON `synapse.execute` + a SQL tool; not substrate. |
| Semantic schema catalog | **product facade** | Product knowledge layer; consumed by an agent via tools, not core. |
| Output guardrails / structured-output validation | **core** (generic schema sanitizer) / **plugin** (policy) | A generalized provider-agnostic schema sanitizer is substrate-adjacent (PS-6); domain guardrail policy is plugin/product. |
| Model fallback / routing policy | **plugin / config** | Policy over the provider trait; not a core default. |
| Tool search / discovery index | **product facade** | Scales with a tool catalog; a product concern, not the minimal registry. |
| Record / replay provider conformance | **plugin / dev-tooling** | A cassette test harness (PS-5); supports the substrate, ships as test/dev tooling, not runtime core. |
| Plugin manifest + capability/version metadata | **core** (trait metadata) / **v0.2** (introspection SQL) | Metadata on the `Plugin` trait is substrate; the `synapse.plugin_*` introspection SQL is frozen by N2.2 until v0.2. |
| Reactive triggers (DB trigger to agent, queue + inline) | **core host capability** | Operator-approved 2026-05-17 (overrides N2.2 for this surface). Trigger -> enqueue/inline is a Postgres-native substrate feature; the self-draining bgworker is v0.2. |
| Desktop / GUI / TUI shells | **out of scope** | psql is the surface; observability via SQL. |
| Unsandboxed code execution / CodeAct | **excluded until designed** | Only after a sandbox + rollback design exists (ADR D14). |
| Inference engine | **external** | Candle / llama.cpp; provider plugins consume an engine, core never embeds one. |

## How to use this

When a change proposes adding capability X to `pg-synapse-core`: find
X's category here. If it is not marked **core**, the change is wrong by
default. Raise an ADR amendment to move a row only with explicit
substrate justification, not convenience.
