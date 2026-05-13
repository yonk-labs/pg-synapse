//! The [`Executor`] trait. Implementations are agent control-flow strategies
//! (conversation, ReAct, reflection, plan-and-solve, ...).

use async_trait::async_trait;

use crate::error::ExecutorError;
use crate::types::{ExecutionContext, ExecutorOutcome};

pub(crate) mod loop_harness;

pub mod conversation;

pub use conversation::ConversationExecutor;

/// Run one agent turn-set to completion.
///
/// Implementations must be `Send + Sync` (so they can be shared across
/// requests and held inside `Arc<dyn Executor>`). They must consume the
/// [`ExecutionContext`] by value; cloning if they need to share it with
/// helper tasks is the implementor's responsibility.
///
/// ## Example
///
/// ```
/// use async_trait::async_trait;
/// use pg_synapse_core::{Executor, ExecutorError};
/// use pg_synapse_core::types::{ExecutionContext, ExecutorOutcome, OutcomeStatus};
///
/// struct EchoExecutor;
///
/// #[async_trait]
/// impl Executor for EchoExecutor {
///     async fn execute(
///         &self,
///         ctx: ExecutionContext,
///     ) -> Result<ExecutorOutcome, ExecutorError> {
///         Ok(ExecutorOutcome {
///             output: ctx.input.clone(),
///             status: OutcomeStatus::Completed,
///             ..Default::default()
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait Executor: Send + Sync {
    /// Drive the agent loop to a terminal state.
    async fn execute(&self, ctx: ExecutionContext) -> Result<ExecutorOutcome, ExecutorError>;
}
