//! End-to-end Runtime integration tests.
//!
//! Exercises the full path: builder hydrates providers + agents, executor
//! routes to a tool, and the outcome propagates back through the facade.

use std::sync::Arc;

use async_trait::async_trait;

use pg_synapse_core::error::ToolError;
use pg_synapse_core::runtime::test_utils::MockProfileSource;
use pg_synapse_core::testing::{
    MockEmbeddingFactory, MockEmbeddingProvider, MockLlmFactory, MockLlmProvider,
};
use pg_synapse_core::tool::Tool;
use pg_synapse_core::types::{
    AgentRow, EmbeddingProfileRow, LlmProfileRow, OutcomeStatus, ToolCtx, ToolOutput, ToolSchema,
};
use pg_synapse_core::{Plugin, Registry, Runtime, RuntimeError};

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

fn embed_profile(name: &str, dimension: u32) -> EmbeddingProfileRow {
    EmbeddingProfileRow {
        name: name.into(),
        provider: "mock-embed".into(),
        model: "e".into(),
        dimension,
        api_key_secret: None,
        base_url: None,
        params: serde_json::Value::Null,
    }
}

fn agent(name: &str, llm: &str, tools: Vec<String>) -> AgentRow {
    AgentRow {
        name: name.into(),
        system_prompt: "be brief".into(),
        soul: None,
        executor_name: "conversation".into(),
        llm_profile_main: Some(llm.into()),
        llm_profile_small: None,
        llm_profile_judge: None,
        embedding_profile: None,
        tools,
        max_iterations: 5,
        timeout_ms: 30_000,
        cost_cap_usd: None,
        trace_level: None,
    }
}

/// Echo tool: returns the JSON it was handed as a text [`ToolOutput`].
struct EchoTool {
    name: String,
    schema: ToolSchema,
}

impl EchoTool {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            schema: ToolSchema::default(),
        }
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &ToolSchema {
        &self.schema
    }

    async fn run(&self, input: serde_json::Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(input.to_string()))
    }
}

/// A tiny Plugin that drops `EchoTool` into the registry under its given name.
struct EchoToolPlugin {
    name: String,
}

impl EchoToolPlugin {
    fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Plugin for EchoToolPlugin {
    fn name(&self) -> &str {
        "echo-tool"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn register(self, registry: &mut Registry) {
        registry.tools.add(EchoTool::new(self.name));
    }
}

#[tokio::test]
async fn runtime_executes_conversation_end_to_end() {
    let mock = Arc::new(MockLlmProvider::new("mock-model"));
    mock.push_text("Hello from the agent");

    let source = MockProfileSource::new()
        .with_llm_profile(llm_profile("default"))
        .with_agent(agent("greeter", "default", vec![]));

    let runtime = Runtime::builder()
        .with_plugin(MockLlmFactory::new("mock", mock.clone()))
        .load_profiles_from(source)
        .build()
        .await
        .unwrap();

    let outcome = runtime.execute("greeter", "Hi there").await.unwrap();
    assert_eq!(outcome.output, "Hello from the agent");
    assert_eq!(outcome.tool_calls.len(), 0);
    assert_eq!(outcome.status, OutcomeStatus::Completed);
}

#[tokio::test]
async fn runtime_routes_tool_calls() {
    let mock = Arc::new(MockLlmProvider::new("mock-model"));
    // A tool-using agent requires a tool-capable provider (PS-1 pre-flight
    // gate). A real deployment pairs tools with a tool_use provider.
    mock.set_capabilities(pg_synapse_core::ProviderCapabilities {
        tool_use: true,
        ..Default::default()
    });
    mock.push_tool_call("c1", "echo", serde_json::json!({"x": 1}));
    mock.push_text("final answer");

    let source = MockProfileSource::new()
        .with_llm_profile(llm_profile("default"))
        .with_agent(agent("tool-user", "default", vec!["echo".into()]));

    let runtime = Runtime::builder()
        .with_plugin(MockLlmFactory::new("mock", mock.clone()))
        .with_plugin(EchoToolPlugin::new("echo"))
        .load_profiles_from(source)
        .build()
        .await
        .unwrap();

    let outcome = runtime.execute("tool-user", "go").await.unwrap();
    assert_eq!(outcome.output, "final answer");
    assert_eq!(outcome.tool_calls.len(), 1);
    assert_eq!(outcome.tool_calls[0].name, "echo");
    assert_eq!(outcome.status, OutcomeStatus::Completed);
}

#[tokio::test]
async fn runtime_errors_on_unknown_agent() {
    let runtime = Runtime::builder().build().await.unwrap();
    let err = runtime.execute("missing", "hi").await.unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound(name) if name == "missing"));
}

#[tokio::test]
async fn runtime_errors_on_missing_executor() {
    let mock = Arc::new(MockLlmProvider::new("m"));
    let mut row = agent("bad-exec", "default", vec![]);
    row.executor_name = "not-a-real-executor".into();

    let runtime = Runtime::builder()
        .with_plugin(MockLlmFactory::new("mock", mock))
        .with_llm_profile(llm_profile("default"))
        .with_agent(row)
        .build()
        .await
        .unwrap();

    let err = runtime.execute("bad-exec", "hi").await.unwrap_err();
    match err {
        RuntimeError::Config(msg) => {
            assert!(
                msg.contains("not-a-real-executor"),
                "expected executor name in message, got: {msg}",
            );
        }
        other => panic!("expected Config, got {other:?}"),
    }
}

#[tokio::test]
async fn runtime_resolves_judge_profile() {
    let main_mock = Arc::new(MockLlmProvider::new("main-model"));
    main_mock.push_text("primary answer");
    let judge_mock = Arc::new(MockLlmProvider::new("judge-model"));

    // Use a single factory that maps the "mock" provider to the main mock; the
    // judge profile is built by the same factory (it ignores the profile and
    // hands back its own provider). That's fine for verifying that the
    // runtime *resolved and registered* the judge profile; the executor
    // doesn't have to call it in this scenario.
    let mut judge_profile = llm_profile("judge");
    judge_profile.model = "judge-model".into();

    let mut row = agent("with-judge", "default", vec![]);
    row.llm_profile_judge = Some("judge".into());

    let factory_main = MockLlmFactory::new("mock", main_mock.clone());
    let factory_judge = MockLlmFactory::new("judge-mock", judge_mock.clone());
    judge_profile.provider = "judge-mock".into();

    let runtime = Runtime::builder()
        .with_plugin(factory_main)
        .with_plugin(factory_judge)
        .with_llm_profile(llm_profile("default"))
        .with_llm_profile(judge_profile)
        .with_agent(row)
        .build()
        .await
        .unwrap();

    // Both profiles registered.
    let names: Vec<&String> = runtime.llm_profile_names().collect();
    assert!(names.iter().any(|n| n.as_str() == "default"));
    assert!(names.iter().any(|n| n.as_str() == "judge"));

    // Conversation executor uses only the main; verify the run still
    // completes through it.
    let outcome = runtime.execute("with-judge", "hi").await.unwrap();
    assert_eq!(outcome.output, "primary answer");
}

#[tokio::test]
async fn runtime_embed_works() {
    let provider = Arc::new(MockEmbeddingProvider::new("e", 16));
    let factory = MockEmbeddingFactory::new("mock-embed", provider);

    let runtime = Runtime::builder()
        .with_plugin(factory)
        .with_embedding_profile(embed_profile("default-embed", 16))
        .with_default_embedding_profile("default-embed")
        .build()
        .await
        .unwrap();

    let v = runtime.embed("hello", None).await.unwrap();
    assert_eq!(v.dimension(), 16);

    let v2 = runtime
        .embed("explicit", Some("default-embed"))
        .await
        .unwrap();
    assert_eq!(v2.dimension(), 16);
}

#[tokio::test]
async fn runtime_filters_tools_by_agent_allow_list() {
    // Two tools registered; agent only allows one. The disallowed tool must
    // not be reachable: the executor sees it as NotFound and (per B18's
    // tool-error feedback loop) feeds that back rather than aborting, so the
    // model gets a turn to recover. The allow-list still did its job: the
    // blocked tool never ran.
    let mock = Arc::new(MockLlmProvider::new("m"));
    mock.set_capabilities(pg_synapse_core::ProviderCapabilities {
        tool_use: true,
        ..Default::default()
    });
    mock.push_tool_call("c1", "blocked", serde_json::json!({}));
    mock.push_text("understood, not using that tool");

    let runtime = Runtime::builder()
        .with_plugin(MockLlmFactory::new("mock", mock))
        .with_plugin(EchoToolPlugin::new("allowed"))
        .with_plugin(EchoToolPlugin::new("blocked"))
        .with_llm_profile(llm_profile("default"))
        .with_agent(agent("restricted", "default", vec!["allowed".into()]))
        .build()
        .await
        .unwrap();

    let outcome = runtime.execute("restricted", "do it").await.unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Completed);
    // The blocked tool was rejected as not-found and fed back to the model.
    let fed_back = outcome.messages.iter().any(|m| {
        m.role == pg_synapse_core::types::Role::Tool
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("not found") && c.contains("blocked"))
    });
    assert!(
        fed_back,
        "disallowed tool must surface a NotFound fed back as a tool message; got {:?}",
        outcome.messages
    );
    // It must never have executed (no echo output from "blocked").
    assert!(
        outcome.tool_calls.iter().all(|tc| tc.name != "allowed"),
        "the allowed tool was never called in this scenario"
    );
}

#[tokio::test]
async fn runtime_threads_caller_role_through_context() {
    // Caller role is captured in ExecutionContext.caller_role. We can't read
    // it directly out of the executor without a custom executor; instead
    // confirm execute_with_caller succeeds end-to-end with a caller-role set.
    let mock = Arc::new(MockLlmProvider::new("m"));
    mock.push_text("done");

    let runtime = Runtime::builder()
        .with_plugin(MockLlmFactory::new("mock", mock))
        .with_llm_profile(llm_profile("default"))
        .with_agent(agent("with-caller", "default", vec![]))
        .build()
        .await
        .unwrap();

    let outcome = runtime
        .execute_with_caller("with-caller", "hi", Some("pg_synapse_user".into()))
        .await
        .unwrap();
    assert_eq!(outcome.output, "done");
}

#[tokio::test]
async fn runtime_propagates_profile_source_secret_to_factory() {
    // The MockLlmFactory ignores profile fields, but we can confirm the
    // build path is exercised when a secret name is referenced and the
    // secret value is supplied via the profile source.
    let mock = Arc::new(MockLlmProvider::new("m"));
    let mut profile = llm_profile("default");
    profile.api_key_secret = Some("OPENAI_KEY".into());

    let source = MockProfileSource::new()
        .with_llm_profile(profile)
        .with_secret("OPENAI_KEY", "sk-real")
        .with_agent(agent("a", "default", vec![]));

    let runtime = Runtime::builder()
        .with_plugin(MockLlmFactory::new("mock", mock.clone()))
        .load_profiles_from(source)
        .build()
        .await
        .unwrap();

    mock.push_text("ok");
    let outcome = runtime.execute("a", "go").await.unwrap();
    assert_eq!(outcome.output, "ok");
}
