//! Scriptable [`crate::LlmProvider`] for unit and integration tests.

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use std::collections::VecDeque;
use std::sync::Mutex;

use crate::error::LlmError;
use crate::llm::{LlmProvider, ProviderCapabilities};
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse, ToolCall, Usage};

/// One scripted response in a [`MockLlmProvider`]'s queue.
pub enum MockResponse {
    /// Return a text completion.
    Text(String),
    /// Return a text completion with a specific `Usage`.
    TextWithUsage(String, Usage),
    /// Return a tool-call completion.
    ToolCall {
        /// Provider-issued call ID.
        id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// Return a tool-call completion with a specific `Usage`.
    ToolCallWithUsage {
        /// Provider-issued call ID.
        id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
        /// Token + cost accounting attached to this turn.
        usage: Usage,
    },
    /// Surface an error.
    Error(LlmError),
}

/// In-memory LLM provider that returns pre-scripted responses in FIFO order.
///
/// Use [`MockLlmProvider::push_text`], [`MockLlmProvider::push_tool_call`], and
/// [`MockLlmProvider::push_error`] to enqueue responses before exercising the
/// code under test. Each call to [`MockLlmProvider::complete`] pops one entry.
/// Calling `complete` on an empty queue returns [`LlmError::Provider`].
///
/// The [`MockLlmProvider::stream`] implementation returns an empty stream;
/// tests should drive the mock via [`MockLlmProvider::complete`].
///
/// ## Example
///
/// ```
/// use pg_synapse_core::testing::MockLlmProvider;
/// use pg_synapse_core::types::CompletionRequest;
/// use pg_synapse_core::LlmProvider;
///
/// # tokio_test::block_on(async {
/// let mock = MockLlmProvider::new("test-model");
/// mock.push_text("first").push_text("second");
/// let r1 = mock.complete(CompletionRequest::default()).await.unwrap();
/// assert_eq!(r1.content.as_deref(), Some("first"));
/// # });
/// ```
pub struct MockLlmProvider {
    queue: Mutex<VecDeque<MockResponse>>,
    model: String,
    capabilities: Mutex<ProviderCapabilities>,
}

impl MockLlmProvider {
    /// Construct a mock that reports the given `model` name.
    ///
    /// The default capabilities are all-false (matching the trait default).
    /// Use [`MockLlmProvider::with_capabilities`] to override.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            model: model.into(),
            capabilities: Mutex::new(ProviderCapabilities::default()),
        }
    }

    /// Replace the capabilities this mock advertises.
    pub fn set_capabilities(&self, caps: ProviderCapabilities) -> &Self {
        *self.capabilities.lock().unwrap() = caps;
        self
    }

    /// Enqueue a text response.
    pub fn push_text(&self, text: impl Into<String>) -> &Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::Text(text.into()));
        self
    }

    /// Enqueue a text response with a specific [`Usage`] attached.
    pub fn push_text_with_usage(&self, text: impl Into<String>, usage: Usage) -> &Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::TextWithUsage(text.into(), usage));
        self
    }

    /// Enqueue a tool-call response.
    pub fn push_tool_call(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> &Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::ToolCall {
                id: id.into(),
                name: name.into(),
                args,
            });
        self
    }

    /// Enqueue a tool-call response with a specific [`Usage`] attached.
    pub fn push_tool_call_with_usage(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        args: serde_json::Value,
        usage: Usage,
    ) -> &Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::ToolCallWithUsage {
                id: id.into(),
                name: name.into(),
                args,
                usage,
            });
        self
    }

    /// Enqueue an error.
    pub fn push_error(&self, err: LlmError) -> &Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::Error(err));
        self
    }

    /// Number of queued responses remaining.
    pub fn queued(&self) -> usize {
        self.queue.lock().unwrap().len()
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let next = self.queue.lock().unwrap().pop_front();
        match next {
            Some(MockResponse::Text(text)) => Ok(CompletionResponse {
                content: Some(text),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: Usage::default(),
            }),
            Some(MockResponse::TextWithUsage(text, usage)) => Ok(CompletionResponse {
                content: Some(text),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage,
            }),
            Some(MockResponse::ToolCall { id, name, args }) => Ok(CompletionResponse {
                content: None,
                tool_calls: vec![ToolCall { id, name, args }],
                finish_reason: "tool_calls".into(),
                usage: Usage::default(),
            }),
            Some(MockResponse::ToolCallWithUsage {
                id,
                name,
                args,
                usage,
            }) => Ok(CompletionResponse {
                content: None,
                tool_calls: vec![ToolCall { id, name, args }],
                finish_reason: "tool_calls".into(),
                usage,
            }),
            Some(MockResponse::Error(e)) => Err(e),
            None => Err(LlmError::Provider {
                provider: "mock".into(),
                reason: "queue exhausted".into(),
            }),
        }
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        Ok(Box::pin(stream::empty()))
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_pushed_text_in_order() {
        let m = MockLlmProvider::new("mock");
        m.push_text("hello").push_text("world");
        let req = CompletionRequest::default();
        assert_eq!(
            m.complete(req.clone()).await.unwrap().content.as_deref(),
            Some("hello")
        );
        assert_eq!(
            m.complete(req.clone()).await.unwrap().content.as_deref(),
            Some("world")
        );
    }

    #[tokio::test]
    async fn returns_tool_call_response() {
        let m = MockLlmProvider::new("mock");
        m.push_tool_call("c1", "weather", serde_json::json!({"loc": "SF"}));
        let resp = m.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "weather");
        assert_eq!(resp.finish_reason, "tool_calls");
    }

    #[tokio::test]
    async fn empty_queue_returns_provider_error() {
        let m = MockLlmProvider::new("mock");
        let err = m.complete(CompletionRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Provider { .. }));
    }

    #[tokio::test]
    async fn pushed_error_is_returned() {
        let m = MockLlmProvider::new("mock");
        m.push_error(LlmError::Auth("bad key".into()));
        let err = m.complete(CompletionRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Auth(_)));
    }

    #[tokio::test]
    async fn stream_returns_empty_stream() {
        use futures::StreamExt;
        let m = MockLlmProvider::new("mock");
        let mut s = m.stream(CompletionRequest::default()).await.unwrap();
        assert!(s.next().await.is_none());
    }

    #[test]
    fn model_name_reflects_constructor() {
        let m = MockLlmProvider::new("gpt-test");
        assert_eq!(m.model_name(), "gpt-test");
    }
}
