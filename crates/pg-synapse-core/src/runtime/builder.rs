//! [`RuntimeBuilder`]: fluent constructor for [`super::Runtime`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::embedding::EmbeddingProvider;
use crate::error::{ProviderError, RuntimeError};
use crate::llm::LlmProvider;
use crate::llm::retry_layer::{RetryConfig, RetryProvider};
use crate::plugin::{Plugin, Registry, register_builtin_executors};
use crate::types::{AgentRow, EmbeddingProfileRow, LlmProfileRow};

use super::resolver::{collect_secret_names, inject_resolved_key};
use super::{ProfileSource, Runtime};

/// Fluent builder for [`Runtime`].
///
/// Built-in executors (`conversation`, `react`, `reflection`) are pre-installed
/// at construction time; plugin crates contribute additional executors,
/// provider factories, tools, memory, or compressor via
/// [`RuntimeBuilder::with_plugin`].
///
/// Profile rows and agent rows reach the runtime either via inline builders
/// ([`Self::with_llm_profile`], [`Self::with_agent`], etc.) for tests, or via
/// a [`ProfileSource`] for production hosts that read from Postgres.
pub struct RuntimeBuilder {
    registry: Registry,
    profile_source: Option<Box<dyn ProfileSource>>,
    default_embedding_profile: Option<String>,
    inline_llm_profiles: Vec<LlmProfileRow>,
    inline_embedding_profiles: Vec<EmbeddingProfileRow>,
    inline_agents: Vec<AgentRow>,
    inline_secrets: HashMap<String, String>,
    retry_config: Option<RetryConfig>,
    interrupt_check: Option<crate::types::InterruptCheck>,
}

impl RuntimeBuilder {
    /// Construct a builder with the three reference executors pre-registered.
    pub fn new() -> Self {
        let mut registry = Registry::new();
        register_builtin_executors(&mut registry);
        Self {
            registry,
            profile_source: None,
            default_embedding_profile: None,
            inline_llm_profiles: vec![],
            inline_embedding_profiles: vec![],
            inline_agents: vec![],
            inline_secrets: HashMap::new(),
            retry_config: None,
            interrupt_check: None,
        }
    }

    /// Install a [`Plugin`] into the registry. Plugins consume themselves to
    /// move owned state into the registry, so `with_plugin` consumes the
    /// builder by value and returns it.
    pub fn with_plugin<P: Plugin + 'static>(mut self, plugin: P) -> Self {
        plugin.register(&mut self.registry);
        self
    }

    /// Pre-register an LLM profile row (test / no-DB use).
    pub fn with_llm_profile(mut self, profile: LlmProfileRow) -> Self {
        self.inline_llm_profiles.push(profile);
        self
    }

    /// Pre-register an embedding profile row.
    pub fn with_embedding_profile(mut self, profile: EmbeddingProfileRow) -> Self {
        self.inline_embedding_profiles.push(profile);
        self
    }

    /// Pre-register an agent row.
    pub fn with_agent(mut self, agent: AgentRow) -> Self {
        self.inline_agents.push(agent);
        self
    }

    /// Pre-register a secret value (resolved at build time and injected into
    /// the matching profile's `params` as `_resolved_api_key`).
    pub fn with_secret(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.inline_secrets.insert(name.into(), value.into());
        self
    }

    /// Set the default embedding profile used when [`Runtime::embed`] is
    /// called without an explicit profile name.
    pub fn with_default_embedding_profile(mut self, name: impl Into<String>) -> Self {
        self.default_embedding_profile = Some(name.into());
        self
    }

    /// Install a [`ProfileSource`]. Profile rows fetched from the source are
    /// concatenated with any rows already supplied via the inline builders.
    pub fn load_profiles_from<S: ProfileSource + 'static>(mut self, source: S) -> Self {
        self.profile_source = Some(Box::new(source));
        self
    }

    /// Enable retry-on-transient-error for all LLM providers hydrated by this
    /// builder. Each provider is wrapped in a [`RetryProvider`] at build time.
    ///
    /// Opt-in per kernel guideline G4: no retry wrapping happens unless the
    /// caller explicitly supplies a config here.
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = Some(config);
        self
    }

    /// Supply a host cancellation probe, checked by the executor loop between
    /// LLM turns. The pgrx host wires this to the backend's pending-interrupt
    /// flags so a statement cancel can stop a runaway agent. Threaded into
    /// every [`crate::types::ExecutionContext`] this runtime builds.
    pub fn with_interrupt_check(mut self, check: crate::types::InterruptCheck) -> Self {
        self.interrupt_check = Some(check);
        self
    }

    /// Build the runtime: pull rows from the source, hydrate every provider,
    /// index agents, and return the finished facade.
    pub async fn build(mut self) -> Result<Runtime, RuntimeError> {
        // 1. Gather profiles + agents from inline source + ProfileSource.
        let mut llm_profiles = std::mem::take(&mut self.inline_llm_profiles);
        let mut embedding_profiles = std::mem::take(&mut self.inline_embedding_profiles);
        let mut agents = std::mem::take(&mut self.inline_agents);
        let mut secrets = std::mem::take(&mut self.inline_secrets);

        if let Some(src) = self.profile_source.as_ref() {
            llm_profiles.extend(src.llm_profiles().await?);
            embedding_profiles.extend(src.embedding_profiles().await?);
            agents.extend(src.agents().await?);

            let needed = collect_secret_names(&llm_profiles, &embedding_profiles);
            if !needed.is_empty() {
                let name_refs: Vec<&str> = needed.iter().map(String::as_str).collect();
                let fetched = src.secrets(&name_refs).await?;
                for (k, v) in fetched {
                    secrets.entry(k).or_insert(v);
                }
            }
        }

        // 2. Hydrate LLM providers.
        let mut llm_providers: HashMap<String, Arc<dyn LlmProvider>> = HashMap::new();
        for profile in &llm_profiles {
            let factory = self
                .registry
                .llm_factories
                .get(&profile.provider)
                .ok_or_else(|| {
                    RuntimeError::Provider(ProviderError::NotRegistered(profile.provider.clone()))
                })?;
            let mut p = profile.clone();
            p.params = inject_resolved_key(p.params, p.api_key_secret.as_deref(), &secrets);
            let provider = factory.build(p)?;
            // Wrap with retry logic when configured (opt-in per G4).
            let provider: Arc<dyn LlmProvider> = match &self.retry_config {
                Some(cfg) => Arc::new(RetryProvider::new(provider, cfg.clone())),
                None => provider,
            };
            llm_providers.insert(profile.name.clone(), provider);
        }

        // 3. Hydrate embedding providers.
        let mut embedding_providers: HashMap<String, Arc<dyn EmbeddingProvider>> = HashMap::new();
        for profile in &embedding_profiles {
            let factory = self
                .registry
                .embedding_factories
                .get(&profile.provider)
                .ok_or_else(|| {
                    RuntimeError::Provider(ProviderError::NotRegistered(profile.provider.clone()))
                })?;
            let mut p = profile.clone();
            p.params = inject_resolved_key(p.params, p.api_key_secret.as_deref(), &secrets);
            let provider = factory.build(p)?;
            embedding_providers.insert(profile.name.clone(), provider);
        }

        // 4. Index agents.
        let mut agent_index: HashMap<String, AgentRow> = HashMap::new();
        for a in agents {
            agent_index.insert(a.name.clone(), a);
        }

        Ok(Runtime {
            registry: Arc::new(self.registry),
            llm_providers,
            embedding_providers,
            agents: agent_index,
            default_embedding_profile: self.default_embedding_profile,
            interrupt_check: self.interrupt_check,
        })
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}
