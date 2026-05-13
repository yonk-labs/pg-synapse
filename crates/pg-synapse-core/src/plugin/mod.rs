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
use crate::executor::Executor;
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

    /// Look up an executor by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Executor>> {
        self.executors.get(name).cloned()
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

    /// Look up a factory by provider name.
    pub fn get(&self, provider: &str) -> Option<Arc<dyn LlmProviderFactory>> {
        self.factories.get(provider).cloned()
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

    /// Look up a factory by provider name.
    pub fn get(&self, provider: &str) -> Option<Arc<dyn EmbeddingProviderFactory>> {
        self.factories.get(provider).cloned()
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
}
