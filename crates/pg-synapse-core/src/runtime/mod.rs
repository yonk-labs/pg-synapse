//! [`Runtime`] facade and supporting types.
//!
//! `Runtime` is the operator-facing API entry point: hosts (the pgrx extension
//! and the sidecar binary) build one of these at startup and call
//! [`Runtime::execute`] / [`Runtime::embed`] per SQL invocation or HTTP request.
//!
//! Construction goes through [`RuntimeBuilder`]: register plugins, hand it a
//! [`ProfileSource`] (or push profile rows inline for tests), and call
//! [`RuntimeBuilder::build`]. The builder hydrates every LLM and embedding
//! provider, indexes agents by name, and returns an immutable `Runtime`.

mod builder;
mod resolver;

pub use builder::RuntimeBuilder;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::EmbeddingProvider;
use crate::Executor;
use crate::LlmProvider;
use crate::error::{ProviderError, RuntimeError};
use crate::plugin::Registry;
use crate::tool::ToolRegistry;
use crate::types::{
    AgentRow, EmbeddingProfileRow, EmbeddingVector, ExecutionContext, ExecutorOutcome,
    LlmProfileRow,
};

/// Source of agent rows, LLM/embedding profile rows, and secret values.
///
/// Two production implementations are planned: a pgrx-backed one that reads
/// from `pg_synapse.*` tables inside the extension, and a `sqlx`-backed one
/// the sidecar binary uses. Tests use [`test_utils::MockProfileSource`].
#[async_trait]
pub trait ProfileSource: Send + Sync {
    /// Return every row of `pg_synapse.llm_profiles`.
    async fn llm_profiles(&self) -> Result<Vec<LlmProfileRow>, RuntimeError>;

    /// Return every row of `pg_synapse.embedding_profiles`.
    async fn embedding_profiles(&self) -> Result<Vec<EmbeddingProfileRow>, RuntimeError>;

    /// Return every row of `pg_synapse.agents`.
    async fn agents(&self) -> Result<Vec<AgentRow>, RuntimeError>;

    /// Resolve a batch of secret names to their cleartext values.
    ///
    /// Implementations must return an empty entry (or omit the key) for
    /// secret names they could not resolve. The [`RuntimeBuilder`] tolerates
    /// missing secrets; provider factories surface an error later if they
    /// actually need one.
    async fn secrets(&self, names: &[&str]) -> Result<HashMap<String, String>, RuntimeError>;
}

/// Operator-facing runtime facade.
///
/// Built via [`Runtime::builder`]. Internally an immutable bundle of:
///
/// - the [`Registry`] of tools, executors, factories, memory, compressor,
/// - the cache of `name -> Arc<dyn LlmProvider>` keyed by profile name,
/// - the cache of `name -> Arc<dyn EmbeddingProvider>`,
/// - the in-memory map of agent rows.
///
/// Cheap to clone (every field is `Arc` or a small map; the inner maps are
/// only mutated through the builder).
pub struct Runtime {
    registry: Arc<Registry>,
    llm_providers: HashMap<String, Arc<dyn LlmProvider>>,
    embedding_providers: HashMap<String, Arc<dyn EmbeddingProvider>>,
    agents: HashMap<String, AgentRow>,
    default_embedding_profile: Option<String>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field(
                "llm_profiles",
                &self.llm_providers.keys().collect::<Vec<_>>(),
            )
            .field(
                "embedding_profiles",
                &self.embedding_providers.keys().collect::<Vec<_>>(),
            )
            .field("agents", &self.agents.keys().collect::<Vec<_>>())
            .field("default_embedding_profile", &self.default_embedding_profile)
            .finish()
    }
}

impl Runtime {
    /// Start a fluent [`RuntimeBuilder`] with the built-in executors
    /// (`conversation`, `react`, `reflection`) pre-registered.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Execute the named agent against `input` and return the executor's
    /// outcome.
    ///
    /// Looks up the agent row, hydrates the [`ExecutionContext`] (LLMs,
    /// optional embeddings, tool sub-registry filtered to the agent's allow
    /// list, optional memory + compressor), and dispatches to the executor
    /// the agent row points at.
    pub async fn execute(
        &self,
        agent_name: &str,
        input: &str,
    ) -> Result<ExecutorOutcome, RuntimeError> {
        self.execute_inner(agent_name, input, None).await
    }

    /// Same as [`Self::execute`] but threads a Postgres `caller_role` through
    /// to the [`ExecutionContext`] for trace fidelity.
    pub async fn execute_with_caller(
        &self,
        agent_name: &str,
        input: &str,
        caller_role: Option<String>,
    ) -> Result<ExecutorOutcome, RuntimeError> {
        self.execute_inner(agent_name, input, caller_role).await
    }

    async fn execute_inner(
        &self,
        agent_name: &str,
        input: &str,
        caller_role: Option<String>,
    ) -> Result<ExecutorOutcome, RuntimeError> {
        let agent = self
            .agents
            .get(agent_name)
            .ok_or_else(|| RuntimeError::AgentNotFound(agent_name.to_owned()))?
            .clone();

        let llm = self.resolve_llm(agent.llm_profile_main.as_deref())?;
        let judge_llm = agent
            .llm_profile_judge
            .as_deref()
            .and_then(|p| self.llm_providers.get(p).cloned());
        let small_llm = agent
            .llm_profile_small
            .as_deref()
            .and_then(|p| self.llm_providers.get(p).cloned());
        let embeddings = agent
            .embedding_profile
            .as_deref()
            .and_then(|p| self.embedding_providers.get(p).cloned());

        let executor: Arc<dyn Executor> = self
            .registry
            .executors
            .get(&agent.executor_name)
            .ok_or_else(|| {
                RuntimeError::Config(format!(
                    "executor '{}' not registered for agent '{}'",
                    agent.executor_name, agent_name,
                ))
            })?;

        let tools = self.resolve_tools(&agent.tools);

        // Pre-flight: reject early if the agent needs tool_use but the
        // provider does not support it. Avoids wasting a full LLM call on
        // an executor loop that will fail on the first tool-call response.
        if !agent.tools.is_empty() && !llm.capabilities().tool_use {
            return Err(RuntimeError::Config(format!(
                "agent '{}' has tools but its LLM provider ('{}') does not support tool_use",
                agent_name,
                llm.model_name(),
            )));
        }

        let ctx = ExecutionContext {
            execution_id: Uuid::new_v4(),
            agent_name: agent.name.clone(),
            system_prompt: agent.system_prompt.clone(),
            soul: agent.soul.clone(),
            input: input.to_owned(),
            executor_name: agent.executor_name.clone(),
            tools: Arc::new(tools),
            llm,
            judge_llm,
            small_llm,
            embeddings,
            memory: self.registry.memory.clone(),
            compressor: self.registry.compressor.clone(),
            max_iterations: agent.max_iterations,
            timeout: std::time::Duration::from_millis(agent.timeout_ms),
            cost_cap_usd: agent.cost_cap_usd,
            caller_role,
            trace_level: agent
                .trace_level
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
        };

        executor.execute(ctx).await.map_err(RuntimeError::from)
    }

    /// Embed `text` with the named profile (or the default profile if
    /// `profile` is `None`).
    pub async fn embed(
        &self,
        text: &str,
        profile: Option<&str>,
    ) -> Result<EmbeddingVector, RuntimeError> {
        let key = profile
            .or(self.default_embedding_profile.as_deref())
            .ok_or_else(|| RuntimeError::Config("no embedding profile specified".to_owned()))?;
        let provider = self
            .embedding_providers
            .get(key)
            .ok_or_else(|| {
                RuntimeError::Config(format!("embedding profile '{key}' not registered"))
            })?
            .clone();
        let mut vectors = provider
            .embed(&[text])
            .await
            .map_err(|e| RuntimeError::Config(format!("embedding error: {e}")))?;
        Ok(vectors.pop().unwrap_or_default())
    }

    /// Invoke a registered tool directly, bypassing the agent loop.
    ///
    /// For testing and operator introspection: resolves `name` in the shared
    /// tool registry and runs it with a [`crate::types::ToolCtx`] carrying a
    /// fresh execution id and the supplied `caller_role`. Returns the tool's
    /// output as a `serde_json::Value` (`Text` -> JSON string, `Json` -> the
    /// value as-is, `Empty` -> `null`).
    ///
    /// Errors with [`RuntimeError::Config`] when the tool is not registered,
    /// and propagates [`RuntimeError`] from a tool failure.
    pub async fn call_tool(
        &self,
        name: &str,
        input: serde_json::Value,
        caller_role: Option<String>,
    ) -> Result<serde_json::Value, RuntimeError> {
        let tool = self
            .registry
            .tools
            .get(name)
            .ok_or_else(|| RuntimeError::Config(format!("tool '{name}' not registered")))?;
        let ctx = crate::types::ToolCtx {
            execution_id: Uuid::new_v4(),
            caller_role,
            agent_name: None,
        };
        let out = tool
            .run(input, &ctx)
            .await
            .map_err(|e| RuntimeError::Config(format!("tool '{name}' failed: {e}")))?;
        Ok(match out {
            crate::types::ToolOutput::Text(s) => serde_json::Value::String(s),
            crate::types::ToolOutput::Json(v) => v,
            crate::types::ToolOutput::Empty => serde_json::Value::Null,
        })
    }

    /// Borrow the underlying registry. Hosts use this to enumerate registered
    /// tools, executors, factories, or to attach observability layers.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Iterate every registered agent row.
    pub fn agents(&self) -> impl Iterator<Item = &AgentRow> {
        self.agents.values()
    }

    /// Iterate the names of every registered LLM profile.
    pub fn llm_profile_names(&self) -> impl Iterator<Item = &String> {
        self.llm_providers.keys()
    }

    /// Iterate the names of every registered embedding profile.
    pub fn embedding_profile_names(&self) -> impl Iterator<Item = &String> {
        self.embedding_providers.keys()
    }

    /// True when the runtime knows about the named agent.
    pub fn has_agent(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }

    /// Return the capabilities advertised by the named LLM profile's
    /// provider, or `None` if the profile is not registered.
    pub fn provider_capabilities(
        &self,
        profile_name: &str,
    ) -> Option<crate::llm::ProviderCapabilities> {
        self.llm_providers
            .get(profile_name)
            .map(|p| p.capabilities())
    }

    fn resolve_llm(&self, name: Option<&str>) -> Result<Arc<dyn LlmProvider>, RuntimeError> {
        let key =
            name.ok_or_else(|| RuntimeError::Config("agent missing llm_profile_main".to_owned()))?;
        self.llm_providers.get(key).cloned().ok_or_else(|| {
            RuntimeError::Provider(ProviderError::NotRegistered(format!("llm profile '{key}'")))
        })
    }

    fn resolve_tools(&self, allowed: &[String]) -> ToolRegistry {
        let mut sub = ToolRegistry::new();
        for name in allowed {
            if let Some(t) = self.registry.tools.get(name) {
                sub.add_arc(name.clone(), t);
            }
        }
        sub
    }
}

/// Test helpers: an in-memory [`ProfileSource`].
///
/// Always compiled (not feature-gated) so integration tests under `tests/`
/// can `use pg_synapse_core::runtime::test_utils::MockProfileSource`. The
/// surface area is tiny and the dependency it carries (a few `Vec`s) is
/// negligible for production callers.
pub mod test_utils {
    use super::*;

    /// Hand-rolled [`ProfileSource`] used by unit and integration tests.
    #[derive(Default, Clone)]
    pub struct MockProfileSource {
        /// LLM profile rows the source will hand out.
        pub llm: Vec<LlmProfileRow>,
        /// Embedding profile rows the source will hand out.
        pub embeddings: Vec<EmbeddingProfileRow>,
        /// Agent rows the source will hand out.
        pub agents: Vec<AgentRow>,
        /// Secret name -> value map.
        pub secrets: HashMap<String, String>,
        /// If `Some`, every read method returns this error instead of data.
        pub force_error: Option<String>,
    }

    impl MockProfileSource {
        /// Construct an empty source.
        pub fn new() -> Self {
            Self::default()
        }

        /// Append an LLM profile row.
        pub fn with_llm_profile(mut self, p: LlmProfileRow) -> Self {
            self.llm.push(p);
            self
        }

        /// Append an embedding profile row.
        pub fn with_embedding_profile(mut self, p: EmbeddingProfileRow) -> Self {
            self.embeddings.push(p);
            self
        }

        /// Append an agent row.
        pub fn with_agent(mut self, a: AgentRow) -> Self {
            self.agents.push(a);
            self
        }

        /// Set a secret name -> value mapping.
        pub fn with_secret(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
            self.secrets.insert(name.into(), value.into());
            self
        }

        /// Force every method to return [`RuntimeError::Config`] with this
        /// message; used to test error propagation.
        pub fn force_error(mut self, reason: impl Into<String>) -> Self {
            self.force_error = Some(reason.into());
            self
        }
    }

    #[async_trait]
    impl ProfileSource for MockProfileSource {
        async fn llm_profiles(&self) -> Result<Vec<LlmProfileRow>, RuntimeError> {
            if let Some(reason) = &self.force_error {
                return Err(RuntimeError::Config(reason.clone()));
            }
            Ok(self.llm.clone())
        }

        async fn embedding_profiles(&self) -> Result<Vec<EmbeddingProfileRow>, RuntimeError> {
            if let Some(reason) = &self.force_error {
                return Err(RuntimeError::Config(reason.clone()));
            }
            Ok(self.embeddings.clone())
        }

        async fn agents(&self) -> Result<Vec<AgentRow>, RuntimeError> {
            if let Some(reason) = &self.force_error {
                return Err(RuntimeError::Config(reason.clone()));
            }
            Ok(self.agents.clone())
        }

        async fn secrets(&self, names: &[&str]) -> Result<HashMap<String, String>, RuntimeError> {
            if let Some(reason) = &self.force_error {
                return Err(RuntimeError::Config(reason.clone()));
            }
            let mut out = HashMap::new();
            for n in names {
                if let Some(v) = self.secrets.get(*n) {
                    out.insert((*n).to_owned(), v.clone());
                }
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MockEmbeddingProvider, MockLlmFactory, MockLlmProvider};
    use test_utils::MockProfileSource;

    fn llm_profile(name: &str) -> LlmProfileRow {
        LlmProfileRow {
            name: name.into(),
            provider: "mock".into(),
            model: "mock-model".into(),
            api_key_secret: None,
            base_url: None,
            params: serde_json::Value::Null,
        }
    }

    fn agent(name: &str, llm: &str) -> AgentRow {
        AgentRow {
            name: name.into(),
            system_prompt: "be brief".into(),
            soul: None,
            executor_name: "conversation".into(),
            llm_profile_main: Some(llm.into()),
            llm_profile_small: None,
            llm_profile_judge: None,
            embedding_profile: None,
            tools: vec![],
            max_iterations: 5,
            timeout_ms: 30_000,
            cost_cap_usd: None,
            trace_level: None,
        }
    }

    #[tokio::test]
    async fn builder_default_has_three_executors() {
        let runtime = Runtime::builder().build().await.unwrap();
        assert!(runtime.registry.executors.contains("conversation"));
        assert!(runtime.registry.executors.contains("react"));
        assert!(runtime.registry.executors.contains("reflection"));
    }

    #[tokio::test]
    async fn builder_loads_inline_profiles_and_agents() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .with_agent(agent("a1", "default"))
            .build()
            .await
            .unwrap();

        assert!(runtime.has_agent("a1"));
        assert_eq!(
            runtime.llm_profile_names().collect::<Vec<_>>(),
            vec![&"default".to_string()]
        );
    }

    #[tokio::test]
    async fn builder_loads_profiles_from_source() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        let source = MockProfileSource::new()
            .with_llm_profile(llm_profile("default"))
            .with_agent(agent("a1", "default"));

        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .load_profiles_from(source)
            .build()
            .await
            .unwrap();

        assert!(runtime.has_agent("a1"));
    }

    #[tokio::test]
    async fn builder_errors_when_provider_factory_missing() {
        let runtime = Runtime::builder()
            .with_llm_profile(llm_profile("default"))
            .with_agent(agent("a1", "default"))
            .build()
            .await;

        match runtime {
            Err(RuntimeError::Provider(ProviderError::NotRegistered(p))) => {
                assert_eq!(p, "mock");
            }
            other => panic!("expected NotRegistered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn builder_propagates_profile_source_error() {
        let source = MockProfileSource::new().force_error("db unreachable");
        let result = Runtime::builder().load_profiles_from(source).build().await;
        match result {
            Err(RuntimeError::Config(msg)) => assert_eq!(msg, "db unreachable"),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn builder_resolves_secrets_from_source() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        let mut profile = llm_profile("default");
        profile.api_key_secret = Some("OPENAI_KEY".into());
        let source = MockProfileSource::new()
            .with_llm_profile(profile)
            .with_secret("OPENAI_KEY", "sk-test")
            .with_agent(agent("a1", "default"));

        // Confirm build succeeds: secret resolution path executes without panic.
        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .load_profiles_from(source)
            .build()
            .await
            .unwrap();
        assert!(runtime.has_agent("a1"));
    }

    #[tokio::test]
    async fn execute_errors_on_unknown_agent() {
        let runtime = Runtime::builder().build().await.unwrap();
        let err = runtime.execute("missing", "hi").await.unwrap_err();
        match err {
            RuntimeError::AgentNotFound(name) => assert_eq!(name, "missing"),
            other => panic!("expected AgentNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_errors_on_missing_executor() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.push_text("ignored");
        let mut a = agent("a1", "default");
        a.executor_name = "no-such-executor".into();
        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .with_agent(a)
            .build()
            .await
            .unwrap();

        let err = runtime.execute("a1", "hi").await.unwrap_err();
        match err {
            RuntimeError::Config(msg) => {
                assert!(msg.contains("no-such-executor"), "msg: {msg}");
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_errors_when_agent_missing_main_profile() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        let mut a = agent("a1", "default");
        a.llm_profile_main = None;
        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .with_agent(a)
            .build()
            .await
            .unwrap();

        let err = runtime.execute("a1", "hi").await.unwrap_err();
        match err {
            RuntimeError::Config(msg) => assert!(msg.contains("llm_profile_main"), "msg: {msg}"),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn embed_uses_default_profile_when_none_given() {
        let embed_provider = Arc::new(MockEmbeddingProvider::new("e", 8));
        let factory = crate::testing::MockEmbeddingFactory::new("mock", embed_provider);
        let profile = EmbeddingProfileRow {
            name: "default".into(),
            provider: "mock".into(),
            model: "e".into(),
            dimension: 8,
            ..Default::default()
        };

        let runtime = Runtime::builder()
            .with_plugin(factory)
            .with_embedding_profile(profile)
            .with_default_embedding_profile("default")
            .build()
            .await
            .unwrap();

        let v = runtime.embed("hello", None).await.unwrap();
        assert_eq!(v.dimension(), 8);
    }

    #[tokio::test]
    async fn embed_errors_when_no_profile_resolvable() {
        let runtime = Runtime::builder().build().await.unwrap();
        let err = runtime.embed("hello", None).await.unwrap_err();
        match err {
            RuntimeError::Config(msg) => {
                assert!(msg.contains("no embedding profile"), "msg: {msg}")
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_profile_source_returns_only_requested_secrets() {
        let source = MockProfileSource::new()
            .with_secret("A", "1")
            .with_secret("B", "2");
        let got = source.secrets(&["A"]).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got.get("A").map(String::as_str), Some("1"));
    }

    #[tokio::test]
    async fn registry_borrow_exposes_three_executors() {
        let runtime = Runtime::builder().build().await.unwrap();
        let reg = runtime.registry();
        assert_eq!(reg.executors.len(), 3);
    }

    #[tokio::test]
    async fn call_tool_errors_when_tool_not_registered() {
        let runtime = Runtime::builder().build().await.unwrap();
        let err = runtime
            .call_tool("nope", serde_json::Value::Null, None)
            .await
            .unwrap_err();
        match err {
            RuntimeError::Config(msg) => assert!(msg.contains("not registered"), "msg: {msg}"),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_tool_runs_a_registered_tool() {
        use crate::plugin::{Plugin, Registry};
        use crate::tool::Tool;
        use crate::types::{ToolCtx, ToolOutput, ToolSchema};
        use async_trait::async_trait;
        use std::sync::OnceLock;

        struct EchoTool;
        #[async_trait]
        impl Tool for EchoTool {
            fn name(&self) -> &str {
                "echo"
            }
            fn schema(&self) -> &ToolSchema {
                static S: OnceLock<ToolSchema> = OnceLock::new();
                S.get_or_init(ToolSchema::default)
            }
            async fn run(
                &self,
                input: serde_json::Value,
                _ctx: &ToolCtx,
            ) -> Result<ToolOutput, crate::error::ToolError> {
                Ok(ToolOutput::json(input))
            }
        }

        struct EchoPlugin;
        impl Plugin for EchoPlugin {
            fn name(&self) -> &str {
                "echo-plugin"
            }
            fn version(&self) -> &str {
                "0"
            }
            fn register(self, reg: &mut Registry) {
                reg.tools.add(EchoTool);
            }
        }

        let runtime = Runtime::builder()
            .with_plugin(EchoPlugin)
            .build()
            .await
            .unwrap();
        let out = runtime
            .call_tool("echo", serde_json::json!({"x": 1}), Some("alice".into()))
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!({"x": 1}));
    }

    #[tokio::test]
    async fn preflight_rejects_tools_without_tool_use_capability() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.push_text("ignored");
        // Default capabilities: tool_use = false.

        let mut a = agent("a1", "default");
        a.tools = vec!["some_tool".into()];

        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .with_agent(a)
            .build()
            .await
            .unwrap();

        let err = runtime.execute("a1", "hi").await.unwrap_err();
        match err {
            RuntimeError::Config(msg) => {
                assert!(msg.contains("tool_use"), "msg: {msg}");
                assert!(msg.contains("a1"), "msg should mention agent name: {msg}");
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_passes_when_capabilities_match() {
        use crate::llm::ProviderCapabilities;

        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.set_capabilities(ProviderCapabilities {
            tool_use: true,
            ..Default::default()
        });
        mock.push_text("ok");

        let mut a = agent("a1", "default");
        a.tools = vec!["some_tool".into()];

        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .with_agent(a)
            .build()
            .await
            .unwrap();

        // Should not error on the pre-flight check. It will reach the
        // executor, which runs through the conversation loop. The tool
        // "some_tool" is not registered so the executor will not find it, but
        // the mock provider returns a text response so the loop completes.
        let outcome = runtime.execute("a1", "hi").await.unwrap();
        assert_eq!(outcome.output, "ok");
    }

    #[tokio::test]
    async fn preflight_passes_for_toolless_agent_on_no_tool_use_provider() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.push_text("fine");
        // Default capabilities: tool_use = false, but agent has no tools.

        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .with_agent(agent("a1", "default"))
            .build()
            .await
            .unwrap();

        let outcome = runtime.execute("a1", "hi").await.unwrap();
        assert_eq!(outcome.output, "fine");
    }

    #[tokio::test]
    async fn provider_capabilities_returns_none_for_unknown_profile() {
        let runtime = Runtime::builder().build().await.unwrap();
        assert!(runtime.provider_capabilities("nonexistent").is_none());
    }

    #[tokio::test]
    async fn provider_capabilities_returns_caps_for_known_profile() {
        use crate::llm::ProviderCapabilities;

        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.set_capabilities(ProviderCapabilities {
            tool_use: true,
            vision: true,
            ..Default::default()
        });

        let runtime = Runtime::builder()
            .with_plugin(MockLlmFactory::new("mock", mock))
            .with_llm_profile(llm_profile("default"))
            .build()
            .await
            .unwrap();

        let caps = runtime.provider_capabilities("default").unwrap();
        assert!(caps.tool_use);
        assert!(caps.vision);
        assert!(!caps.streaming);
        assert!(!caps.json_mode);
    }
}
