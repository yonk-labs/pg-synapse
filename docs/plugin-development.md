# Plugin Development

A plugin is a crate that contributes capabilities into a host's `Registry` at
startup. The kernel stays I/O-free; plugins bring the concrete providers,
tools, executors, and backends.

Read `plugins/pg-synapse-provider-openai/` and `plugins/pg-synapse-tools-http/`
first; they are the canonical shapes.

## The `Plugin` trait

```rust
pub trait Plugin {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn register(self, registry: &mut Registry);
}
```

`register` consumes `self` so the plugin can move owned state into the
registry. Most plugins are zero-size structs that build their state inside
`register`. `version` typically returns `env!("CARGO_PKG_VERSION")`.

`Registry` aggregates everything a host wires up:

```rust
pub struct Registry {
    pub tools: ToolRegistry,
    pub executors: ExecutorRegistry,
    pub llm_factories: LlmFactoryRegistry,
    pub embedding_factories: EmbeddingFactoryRegistry,
    pub memory: Option<Arc<dyn MemoryProvider>>,
    pub compressor: Option<Arc<dyn Compressor>>,
}
```

## The four plugin shapes

### LLM provider

Implement `LlmProviderFactory` (`provider_name`, `build(LlmProfileRow) ->
Result<Arc<dyn LlmProvider>, ProviderError>`) and register it. From
`pg-synapse-provider-openai`:

```rust
impl LlmProviderFactory for OpenAiProviderFactory {
    fn provider_name(&self) -> &str { "openai" }
    fn build(&self, profile: LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError> {
        // base_url precedence: profile column > params.base_url > OpenAI default.
        let base_url = profile.base_url.clone()
            .or_else(|| profile.params.get("base_url")
                .and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        // ... build the provider, attach cost params, extra headers ...
        Ok(Arc::new(OpenAiProvider::new(profile.model.clone(), base_url)))
    }
}

impl Plugin for OpenAiProviderFactory {
    fn name(&self) -> &str { "pg-synapse-provider-openai" }
    fn version(&self) -> &str { env!("CARGO_PKG_VERSION") }
    fn register(self, registry: &mut Registry) {
        registry.llm_factories.add(Arc::new(self));
    }
}
```

The factory is keyed by `provider_name()`; the host calls `build` once per
`synapse.llm_profiles` row whose `provider` matches.

### Embedding provider

Same shape with `EmbeddingProviderFactory` (`provider_name`,
`build(EmbeddingProfileRow) -> Result<Arc<dyn EmbeddingProvider>,
ProviderError>`), registered via `registry.embedding_factories.add(...)`. See
`pg-synapse-embeddings-ort` (`provider_name()` is `"ort"`).

### Tool

Add `Arc<dyn Tool>` instances to `registry.tools`. The HTTP plugin builds one
template instance per tool and inserts each by its `TOOL_NAME`:

```rust
fn register(self, registry: &mut Registry) {
    let getter = HttpGet { url: String::new(), headers: BTreeMap::new() };
    registry.tools.add_arc(HttpGet::TOOL_NAME.to_string(), Arc::new(getter));
    // ... http_post, http_head ...
}
```

See [tool-authoring.md](./tool-authoring.md) for the three ways to write the
tool itself.

### Executor

Insert `Arc<dyn Executor>` into `registry.executors` under a name agents
reference via `agents.executor_name`:

```rust
fn register(self, registry: &mut Registry) {
    registry.executors.add("my-strategy", Arc::new(MyExecutor::default()));
}
```

The kernel pre-registers `conversation`, `react`, and `reflection` via
`register_builtin_executors` (and `Runtime::builder` does this for you).

## Where state lives

A factory holds shared state (an HTTP client, a connection pool, a loaded
model) behind `Arc` inside the factory struct, constructed in `build` or in
the factory's constructor. The HTTP plugin shares one lazily-initialized
`reqwest::Client` (a `OnceLock<Client>`) across all three tools. Per-call
inputs are deserialized fresh on every `run`; never store request state in the
tool struct.

## Testing patterns

- HTTP-backed providers/tools: `wiremock` (already a `pg-synapse-core`
  dev-dependency) to stub the upstream and assert request shape.
- In-memory: the kernel's `pg_synapse_core::testing` module ships
  `MockLlmProvider`, `MockEmbeddingProvider`, `MockTool`, and
  `runtime::test_utils::MockProfileSource`. Drive a real `Runtime` end to end
  with `Runtime::builder().with_plugin(MockLlmFactory::new(...))`.
- Plugin wiring: construct an empty `Registry::new()`, call your
  `Plugin::register`, then assert `registry.tools.names()` /
  `registry.llm_factories.contains("...")`. Every shipped plugin has such a
  test.
- `LlmProvider` conformance: every wired provider plugin runs the shared
  cassette conformance suite. If you are authoring a new `LlmProvider`
  plugin, follow the recipe in `docs/provider-conformance.md` to wire
  yours into the suite (static, golden, and optional live record/replay
  tests).

## Workspace layout

In-workspace v0.1 plugins live under `plugins/<crate>/`; the workspace root
`Cargo.toml` lists them as members. Yonk-ecosystem integration plugins live in
their target tool's own repo and depend on `pg-synapse-core` as a normal
crates.io dependency. A new plugin needs `Cargo.toml` (depend on
`pg-synapse-core`, plus `pg-synapse-macros` if you use `#[derive(Tool)]`), a
`src/lib.rs` with the `Plugin` impl, and `#![forbid(unsafe_code)]` /
`#![warn(missing_docs)]` to match the workspace lint posture.

## crates.io publication order

Publish bottom-up so each crate's dependencies already exist on the registry:

1. `pg-synapse-macros` (the proc-macro crate, no internal deps).
2. `pg-synapse-core` (depends on the published macros crate).
3. The plugin crates (`pg-synapse-provider-openai`,
   `pg-synapse-tools-http`, `pg-synapse-embeddings-ort`, ...), each depending
   on the published core.

Host crates (`pg-synapse-pgrx`, and later `pg-synapse-sidecar`) publish last
or are distributed as build artifacts.
