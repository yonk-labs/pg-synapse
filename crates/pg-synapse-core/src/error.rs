//! Error taxonomy for the kernel.
//!
//! Every trait boundary in `pg-synapse-core` returns a typed error from this
//! module. All error enums implement
//! `std::error::Error + Send + Sync + 'static` and `serde::Serialize +
//! serde::Deserialize`, so they round-trip cleanly through SQL trace rows and
//! HTTP error responses without lossy stringification.

use thiserror::Error;

/// Failures that surface from [`crate::Executor::execute`].
///
/// `Tool`, `Llm`, `Provider`, `Memory`, and `Compression` variants are reached
/// via `#[from]` conversion so executors can propagate sub-system errors with
/// the `?` operator.
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum ExecutorError {
    /// The executor hit its iteration cap without producing a final answer.
    #[error("max iterations ({0}) reached without termination")]
    MaxIterationsReached(u32),

    /// The execution exceeded its wall-clock budget.
    #[error("timeout after {0}ms")]
    Timeout(u64),

    /// The execution would exceed its configured USD cost cap.
    #[error("cost cap ${cap} exceeded at ${spent}")]
    CostCapExceeded {
        /// Cap value supplied to the execution, in USD.
        cap: f64,
        /// Cumulative spend at the moment the cap was tripped, in USD.
        spent: f64,
    },

    /// A tool invocation failed.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// An LLM call failed.
    #[error("llm error: {0}")]
    Llm(#[from] LlmError),

    /// A provider could not be hydrated from its profile row.
    #[error("provider hydration failed: {0}")]
    Provider(#[from] ProviderError),

    /// A memory backend call failed.
    #[error("memory error: {0}")]
    Memory(#[from] MemoryError),

    /// A compressor call failed.
    #[error("compression error: {0}")]
    Compression(#[from] CompressionError),

    /// Catch-all for invariant violations inside the executor.
    #[error("internal: {0}")]
    Internal(String),
}

/// Failures from [`crate::Tool::run`] or tool-registry lookup.
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum ToolError {
    /// The registry has no tool registered under this name.
    #[error("tool '{name}' not found in registry")]
    NotFound {
        /// Name the agent (or caller) asked for.
        name: String,
    },

    /// The supplied JSON did not match the tool's schema.
    #[error("invalid input for tool '{name}': {reason}")]
    InvalidInput {
        /// Tool name.
        name: String,
        /// Reason the input was rejected.
        reason: String,
    },

    /// The tool ran but returned an error.
    #[error("tool '{name}' execution failed: {reason}")]
    Execution {
        /// Tool name.
        name: String,
        /// Reason for the failure (from the tool's own logic).
        reason: String,
    },

    /// The tool exceeded its allotted runtime.
    #[error("tool '{name}' timed out after {timeout_ms}ms")]
    Timeout {
        /// Tool name.
        name: String,
        /// Configured timeout in milliseconds.
        timeout_ms: u64,
    },

    /// MCP-protocol-level failure during tool dispatch (handshake, transport, framing).
    #[error("mcp protocol error: {0}")]
    Mcp(String),
}

/// Failures from [`crate::LlmProvider::complete`] or `stream`.
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum LlmError {
    /// Provider-specific error not covered by the other variants.
    #[error("provider error from {provider}: {reason}")]
    Provider {
        /// Provider name (e.g. "openai").
        provider: String,
        /// Reason as reported by the provider.
        reason: String,
    },

    /// Authentication or authorization failure.
    #[error("authentication failed for provider {0}")]
    Auth(String),

    /// Provider rate limit hit; `retry_after_ms` is the provider's hint when known.
    #[error("rate limited by provider {provider}; retry after {retry_after_ms:?}ms")]
    RateLimited {
        /// Provider name.
        provider: String,
        /// Provider's retry-after hint in milliseconds, if any.
        retry_after_ms: Option<u64>,
    },

    /// Request exceeded the model's context window.
    #[error("context length exceeded for model {model}: {tokens} > {limit}")]
    ContextLengthExceeded {
        /// Model name.
        model: String,
        /// Tokens in the request.
        tokens: u32,
        /// Model's context length limit.
        limit: u32,
    },

    /// Underlying network failure (DNS, TCP, TLS, HTTP).
    #[error("network error: {0}")]
    Network(String),
}

/// Failures from [`crate::EmbeddingProvider::embed`].
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum EmbeddingError {
    /// Provider-specific embedding failure.
    #[error("provider error: {0}")]
    Provider(String),

    /// Returned vector dimension did not match the profile's declared dimension.
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch {
        /// Dimension declared on the profile.
        expected: usize,
        /// Dimension actually returned.
        actual: usize,
    },

    /// Underlying network failure.
    #[error("network error: {0}")]
    Network(String),
}

/// Failures from [`crate::MemoryProvider`] backends.
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum MemoryError {
    /// Backend-specific failure.
    #[error("backend error: {0}")]
    Backend(String),

    /// The supplied scope is not understood by the backend.
    #[error("scope invalid: {0}")]
    InvalidScope(String),

    /// A requested memory entry could not be found.
    #[error("entry not found")]
    NotFound,
}

/// Failures from [`crate::Compressor::compress`].
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum CompressionError {
    /// The compression budget cannot accommodate the input.
    #[error("budget too small: needed {needed}, had {available}")]
    BudgetTooSmall {
        /// Tokens the compressor needed to make progress.
        needed: u32,
        /// Tokens the budget actually allowed.
        available: u32,
    },

    /// Backend-specific failure.
    #[error("backend error: {0}")]
    Backend(String),
}

/// Failures during provider construction from a profile row.
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum ProviderError {
    /// No factory matched the profile's `provider` string.
    #[error("provider '{0}' not registered")]
    NotRegistered(String),

    /// The factory rejected the profile (missing field, bad URL, etc.).
    #[error("invalid profile for provider '{provider}': {reason}")]
    InvalidProfile {
        /// Provider name.
        provider: String,
        /// Reason the profile was rejected.
        reason: String,
    },

    /// Profile referenced a secret name that does not exist.
    #[error("secret '{0}' not found")]
    SecretNotFound(String),
}

/// Failures from the `Runtime` facade.
#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
pub enum RuntimeError {
    /// The agent name was not registered with the runtime.
    #[error("agent '{0}' not found")]
    AgentNotFound(String),

    /// An executor returned an error during the run.
    #[error("executor: {0}")]
    Executor(#[from] ExecutorError),

    /// Configuration was invalid or missing a required field.
    #[error("config: {0}")]
    Config(String),

    /// A provider could not be hydrated.
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_implement_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<ExecutorError>();
        assert_bounds::<ToolError>();
        assert_bounds::<LlmError>();
        assert_bounds::<EmbeddingError>();
        assert_bounds::<MemoryError>();
        assert_bounds::<CompressionError>();
        assert_bounds::<ProviderError>();
        assert_bounds::<RuntimeError>();
    }

    #[test]
    fn errors_roundtrip_json() {
        let e = ExecutorError::CostCapExceeded {
            cap: 10.0,
            spent: 12.5,
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: ExecutorError = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            ExecutorError::CostCapExceeded {
                cap,
                spent
            } if cap == 10.0 && spent == 12.5
        ));
    }

    #[test]
    fn tool_error_displays_with_name() {
        let e = ToolError::NotFound {
            name: "weather".into(),
        };
        assert_eq!(e.to_string(), "tool 'weather' not found in registry");
    }
}
