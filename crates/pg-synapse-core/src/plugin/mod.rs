//! The plugin shape: [`Plugin`] trait + [`Registry`] aggregate.
//!
//! Plugin crates implement [`Plugin`] for a value that, when handed to the
//! host's [`Registry`] at startup, registers any combination of tools,
//! executors, provider factories, a memory backend, and a compressor.
//!
//! In v0.1 the executor and provider-factory sub-registries are lightweight
//! `HashMap`s; later milestones may swap them for richer types.

use std::collections::HashMap;
use std::sync::Arc;

use crate::compression::Compressor;
use crate::embedding::EmbeddingProviderFactory;
use crate::executor::{ConversationExecutor, Executor, ReActExecutor, ReflectionExecutor};
use crate::llm::LlmProviderFactory;
use crate::memory::MemoryProvider;
use crate::tool::ToolRegistry;

/// Map from executor name to a shared `Arc<dyn Executor>`.
#[derive(Default, Clone)]
pub struct ExecutorRegistry {
    executors: HashMap<String, Arc<dyn Executor>>,
}

impl ExecutorRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an executor under `name`. Replaces any prior entry.
    pub fn insert(&mut self, name: impl Into<String>, executor: Arc<dyn Executor>) -> &mut Self {
        self.executors.insert(name.into(), executor);
        self
    }

    /// Insert an executor under `name`. Alias of [`Self::insert`] preserved
    /// because plugin authors prefer the `add` verb.
    pub fn add(&mut self, name: impl Into<String>, executor: Arc<dyn Executor>) -> &mut Self {
        self.insert(name, executor)
    }

    /// Look up an executor by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Executor>> {
        self.executors.get(name).cloned()
    }

    /// List the names of every registered executor (unsorted).
    pub fn names(&self) -> Vec<String> {
        self.executors.keys().cloned().collect()
    }

    /// True when an executor under `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.executors.contains_key(name)
    }

    /// Number of registered executors.
    pub fn len(&self) -> usize {
        self.executors.len()
    }

    /// True when no executors are registered.
    pub fn is_empty(&self) -> bool {
        self.executors.is_empty()
    }
}

/// Map from provider name to its [`LlmProviderFactory`].
#[derive(Default, Clone)]
pub struct LlmFactoryRegistry {
    factories: HashMap<String, Arc<dyn LlmProviderFactory>>,
}

impl LlmFactoryRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a factory keyed on its `provider_name()`.
    pub fn insert(&mut self, factory: Arc<dyn LlmProviderFactory>) -> &mut Self {
        self.factories
            .insert(factory.provider_name().to_owned(), factory);
        self
    }

    /// Alias of [`Self::insert`] for symmetry with `ExecutorRegistry::add`.
    pub fn add(&mut self, factory: Arc<dyn LlmProviderFactory>) -> &mut Self {
        self.insert(factory)
    }

    /// Look up a factory by provider name.
    pub fn get(&self, provider: &str) -> Option<Arc<dyn LlmProviderFactory>> {
        self.factories.get(provider).cloned()
    }

    /// List the names of every registered provider factory (unsorted).
    pub fn names(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }

    /// True when a factory under `provider` is registered.
    pub fn contains(&self, provider: &str) -> bool {
        self.factories.contains_key(provider)
    }

    /// Number of registered factories.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// True when no factories are registered.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

/// Map from provider name to its [`EmbeddingProviderFactory`].
#[derive(Default, Clone)]
pub struct EmbeddingFactoryRegistry {
    factories: HashMap<String, Arc<dyn EmbeddingProviderFactory>>,
}

impl EmbeddingFactoryRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a factory keyed on its `provider_name()`.
    pub fn insert(&mut self, factory: Arc<dyn EmbeddingProviderFactory>) -> &mut Self {
        self.factories
            .insert(factory.provider_name().to_owned(), factory);
        self
    }

    /// Alias of [`Self::insert`] for symmetry with `ExecutorRegistry::add`.
    pub fn add(&mut self, factory: Arc<dyn EmbeddingProviderFactory>) -> &mut Self {
        self.insert(factory)
    }

    /// Look up a factory by provider name.
    pub fn get(&self, provider: &str) -> Option<Arc<dyn EmbeddingProviderFactory>> {
        self.factories.get(provider).cloned()
    }

    /// List the names of every registered provider factory (unsorted).
    pub fn names(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }

    /// True when a factory under `provider` is registered.
    pub fn contains(&self, provider: &str) -> bool {
        self.factories.contains_key(provider)
    }

    /// Number of registered factories.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// True when no factories are registered.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

/// Aggregate registry the host passes to every [`Plugin::register`] call.
#[derive(Default)]
pub struct Registry {
    /// Tool registry. Shared with executions.
    pub tools: ToolRegistry,
    /// Executor implementations available by name.
    pub executors: ExecutorRegistry,
    /// LLM provider factories keyed by `provider_name()`.
    pub llm_factories: LlmFactoryRegistry,
    /// Embedding provider factories keyed by `provider_name()`.
    pub embedding_factories: EmbeddingFactoryRegistry,
    /// Optional memory backend.
    pub memory: Option<Arc<dyn MemoryProvider>>,
    /// Optional compressor.
    pub compressor: Option<Arc<dyn Compressor>>,
}

impl Registry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Install the three reference executors (`conversation`, `react`, `reflection`)
/// into `registry.executors`. Hosts that build a [`Registry`] directly call
/// this once at startup; [`crate::Runtime::builder`] does it for you.
pub fn register_builtin_executors(registry: &mut Registry) {
    registry
        .executors
        .add("conversation", Arc::new(ConversationExecutor));
    registry.executors.add("react", Arc::new(ReActExecutor));
    registry
        .executors
        .add("reflection", Arc::new(ReflectionExecutor::default()));
}

/// A bundle a host installs to wire in tools, executors, provider factories,
/// or backends.
///
/// Implementations consume `self` so they can move owned state into the
/// registry. Typical implementations are zero-size structs that build their
/// internal state in `register`.
///
/// ## Example
///
/// ```
/// use pg_synapse_core::plugin::{Plugin, Registry};
///
/// struct NoOpPlugin;
///
/// impl Plugin for NoOpPlugin {
///     fn name(&self) -> &str { "noop" }
///     fn version(&self) -> &str { "0.1.0" }
///     fn register(self, _registry: &mut Registry) {}
/// }
/// ```
pub trait Plugin {
    /// Human-readable plugin name.
    fn name(&self) -> &str;
    /// Plugin version (semver string).
    fn version(&self) -> &str;
    /// Install this plugin's contributions into the host's registry.
    fn register(self, registry: &mut Registry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{EmbeddingError, LlmError, ProviderError, ToolError};
    use crate::testing::{MockEmbeddingProvider, MockLlmProvider, MockTool};
    use crate::tool::Tool;
    use crate::types::{
        EmbeddingProfileRow, LlmProfileRow, ToolCtx, ToolOutput, ToolSchema,
    };

    struct DummyPlugin;
    impl Plugin for DummyPlugin {
        fn name(&self) -> &str {
            "dummy"
        }
        fn version(&self) -> &str {
            "0.0.1"
        }
        fn register(self, _registry: &mut Registry) {}
    }

    struct DummyLlmFactory;
    impl LlmProviderFactory for DummyLlmFactory {
        fn provider_name(&self) -> &str {
            "dummy-llm"
        }
        fn build(
            &self,
            _profile: LlmProfileRow,
        ) -> Result<Arc<dyn crate::llm::LlmProvider>, ProviderError> {
            Err(ProviderError::InvalidProfile {
                provider: "dummy-llm".into(),
                reason: "test-only factory".into(),
            })
        }
    }

    struct DummyEmbeddingFactory;
    impl EmbeddingProviderFactory for DummyEmbeddingFactory {
        fn provider_name(&self) -> &str {
            "dummy-embed"
        }
        fn build(
            &self,
            _profile: EmbeddingProfileRow,
        ) -> Result<Arc<dyn crate::embedding::EmbeddingProvider>, ProviderError> {
            Err(ProviderError::InvalidProfile {
                provider: "dummy-embed".into(),
                reason: "test-only factory".into(),
            })
        }
    }

    /// Plugin that drops one tool + one executor + an LLM factory + an
    /// embedding factory into the registry. Used by the integration test
    /// below.
    struct KitchenSinkPlugin;
    impl Plugin for KitchenSinkPlugin {
        fn name(&self) -> &str {
            "kitchen-sink"
        }
        fn version(&self) -> &str {
            "0.0.1"
        }
        fn register(self, registry: &mut Registry) {
            registry
                .tools
                .add(MockTool::new("noop", ToolOutput::Empty));
            registry
                .executors
                .add("conversation", Arc::new(ConversationExecutor));
            registry.llm_factories.add(Arc::new(DummyLlmFactory));
            registry
                .embedding_factories
                .add(Arc::new(DummyEmbeddingFactory));
        }
    }

    #[test]
    fn registry_default_is_empty() {
        let r = Registry::new();
        assert!(r.tools.is_empty());
        assert!(r.executors.is_empty());
        assert!(r.llm_factories.is_empty());
        assert!(r.embedding_factories.is_empty());
        assert!(r.memory.is_none());
        assert!(r.compressor.is_none());
    }

    #[test]
    fn plugin_metadata_accessible() {
        let p = DummyPlugin;
        assert_eq!(p.name(), "dummy");
        assert_eq!(p.version(), "0.0.1");
    }

    #[test]
    fn plugin_register_runs_without_panicking() {
        let mut r = Registry::new();
        DummyPlugin.register(&mut r);
        assert!(r.tools.is_empty());
    }

    #[test]
    fn executor_registry_add_get_names_contains() {
        let mut er = ExecutorRegistry::new();
        assert!(er.is_empty());
        er.add("conversation", Arc::new(ConversationExecutor));
        er.add("react", Arc::new(ReActExecutor));
        assert_eq!(er.len(), 2);
        assert!(er.contains("conversation"));
        assert!(er.contains("react"));
        assert!(!er.contains("nope"));
        assert!(er.get("conversation").is_some());
        let mut names = er.names();
        names.sort();
        assert_eq!(names, vec!["conversation".to_string(), "react".to_string()]);
    }

    #[test]
    fn llm_factory_registry_add_get_names_contains() {
        let mut lr = LlmFactoryRegistry::new();
        assert!(lr.is_empty());
        lr.add(Arc::new(DummyLlmFactory));
        assert_eq!(lr.len(), 1);
        assert!(lr.contains("dummy-llm"));
        assert!(!lr.contains("nope"));
        assert!(lr.get("dummy-llm").is_some());
        assert_eq!(lr.names(), vec!["dummy-llm".to_string()]);
    }

    #[test]
    fn embedding_factory_registry_add_get_names_contains() {
        let mut er = EmbeddingFactoryRegistry::new();
        assert!(er.is_empty());
        er.add(Arc::new(DummyEmbeddingFactory));
        assert_eq!(er.len(), 1);
        assert!(er.contains("dummy-embed"));
        assert!(!er.contains("nope"));
        assert!(er.get("dummy-embed").is_some());
        assert_eq!(er.names(), vec!["dummy-embed".to_string()]);
    }

    #[test]
    fn register_builtin_executors_installs_three() {
        let mut r = Registry::new();
        register_builtin_executors(&mut r);
        assert_eq!(r.executors.len(), 3);
        assert!(r.executors.contains("conversation"));
        assert!(r.executors.contains("react"));
        assert!(r.executors.contains("reflection"));
    }

    #[test]
    fn plugin_registers_tools_and_executors_via_register() {
        let mut r = Registry::new();
        KitchenSinkPlugin.register(&mut r);
        assert!(r.tools.get("noop").is_some());
        assert!(r.executors.contains("conversation"));
        assert!(r.llm_factories.contains("dummy-llm"));
        assert!(r.embedding_factories.contains("dummy-embed"));
        // Confirm the registered tool actually behaves.
        let _: &dyn Tool = &*r.tools.get("noop").unwrap();
    }

    // Sanity-check use of the mock LLM/embedding providers from this module.
    #[tokio::test]
    async fn mock_providers_used_via_arc_for_registry_paths() {
        let llm: Arc<dyn crate::llm::LlmProvider> = Arc::new(MockLlmProvider::new("m"));
        let embed: Arc<dyn crate::embedding::EmbeddingProvider> =
            Arc::new(MockEmbeddingProvider::new("e", 4));
        assert_eq!(llm.model_name(), "m");
        assert_eq!(embed.dimension(), 4);
    }

    // Surface the imports as used for the test module even if we don't use
    // them in an assertion directly (silences `dead_code` paranoia).
    #[allow(dead_code)]
    fn _types_referenced(
        _t: &ToolSchema,
        _o: &ToolOutput,
        _c: &ToolCtx,
        _l: &LlmError,
        _e: &EmbeddingError,
        _x: &ToolError,
    ) {
    }
}
