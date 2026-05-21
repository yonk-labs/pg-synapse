//! `tower::Service` adapter for [`crate::Executor`] implementations.
//!
//! Spec reference: M2 plan Task 2.5. The kernel ships exactly one tower
//! integration point: this `ExecutorService<E>` (and a `BoxedExecutorService`
//! alias for dynamically-typed executors). Operators wrap it in their own
//! `tower::Layer` stack: timeout, retry, rate-limit, observability, and so
//! on. The kernel itself ships zero pre-built layers (spec G4).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tower::Service;

use crate::Executor;
use crate::error::ExecutorError;
use crate::types::{ExecutionContext, ExecutorOutcome};

/// `tower::Service` wrapper around an [`Executor`].
///
/// `ExecutorService<E>` is `Clone` and cheap to clone; each clone shares the
/// same underlying executor instance via `Arc`. Use [`ExecutorService::new`]
/// to wrap a value-typed executor, or [`ExecutorService::from_arc`] when you
/// already hold an `Arc<E>`. For dynamic dispatch (the host has many
/// executors keyed by name), see [`service_from_dyn`] and
/// [`BoxedExecutorService`].
pub struct ExecutorService<E: Executor + ?Sized + 'static> {
    executor: Arc<E>,
}

impl<E: Executor + 'static> ExecutorService<E> {
    /// Wrap a value-typed executor.
    pub fn new(executor: E) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    /// Wrap an `Arc<E>` directly.
    pub fn from_arc(executor: Arc<E>) -> Self {
        Self { executor }
    }
}

impl<E: Executor + ?Sized + 'static> Clone for ExecutorService<E> {
    fn clone(&self) -> Self {
        Self {
            executor: Arc::clone(&self.executor),
        }
    }
}

impl<E: Executor + ?Sized + 'static> Service<ExecutionContext> for ExecutorService<E> {
    type Response = ExecutorOutcome;
    type Error = ExecutorError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, ctx: ExecutionContext) -> Self::Future {
        let exec = Arc::clone(&self.executor);
        Box::pin(async move { exec.execute(ctx).await })
    }
}

/// Type-erased equivalent of [`ExecutorService`], for runtimes that pick the
/// executor at request time.
pub type BoxedExecutorService = ExecutorService<dyn Executor>;

/// Build a [`BoxedExecutorService`] from an `Arc<dyn Executor>`.
///
/// The runtime facade uses this when it has dispatched on the agent's
/// executor-name field and resolved a concrete `Arc<dyn Executor>` from the
/// executor registry.
pub fn service_from_dyn(executor: Arc<dyn Executor>) -> BoxedExecutorService {
    ExecutorService { executor }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecutorError;
    use crate::executor::ConversationExecutor;
    use crate::llm::LlmProvider;
    use crate::testing::MockLlmProvider;
    use crate::tool::ToolRegistry;
    use crate::types::OutcomeStatus;
    use crate::types::TraceLevel;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tower::ServiceExt;
    use tower::layer::Layer;
    use uuid::Uuid;

    fn ctx_with(llm: Arc<dyn LlmProvider>, max_iter: u32) -> ExecutionContext {
        ExecutionContext {
            execution_id: Uuid::nil(),
            agent_name: "agent".into(),
            system_prompt: "go".into(),
            soul: None,
            input: "hello".into(),
            executor_name: "conversation".into(),
            tools: Arc::new(ToolRegistry::new()),
            llm,
            judge_llm: None,
            small_llm: None,
            embeddings: None,
            memory: None,
            compressor: None,
            max_iterations: max_iter,
            timeout: Duration::from_millis(500),
            cost_cap_usd: None,
            caller_role: None,
            trace_level: TraceLevel::default(),
        }
    }

    #[tokio::test]
    async fn basic_call_returns_outcome() {
        let mock = MockLlmProvider::new("m");
        mock.push_text("ok");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let svc = ExecutorService::new(ConversationExecutor);
        let outcome = svc.oneshot(ctx_with(llm, 5)).await.unwrap();
        assert_eq!(outcome.status, OutcomeStatus::Completed);
        assert_eq!(outcome.output, "ok");
    }

    /// Executor that always errors. Used to exercise the service error path.
    struct ErrExec;
    #[async_trait]
    impl Executor for ErrExec {
        async fn execute(
            &self,
            _ctx: ExecutionContext,
        ) -> Result<crate::types::ExecutorOutcome, ExecutorError> {
            Err(ExecutorError::MaxIterationsReached(7))
        }
    }

    #[tokio::test]
    async fn error_propagates_through_service() {
        let mock = MockLlmProvider::new("m");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);
        let svc = ExecutorService::new(ErrExec);
        let err = svc.oneshot(ctx_with(llm, 5)).await.unwrap_err();
        match err {
            ExecutorError::MaxIterationsReached(n) => assert_eq!(n, 7),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Trivial passthrough layer used to verify layer composition compiles
    /// and works against `ExecutorService`.
    #[derive(Clone, Copy, Default)]
    struct PassThroughLayer;

    #[derive(Clone)]
    struct PassThrough<S>(S);

    impl<S> Layer<S> for PassThroughLayer {
        type Service = PassThrough<S>;
        fn layer(&self, svc: S) -> Self::Service {
            PassThrough(svc)
        }
    }

    impl<S> Service<ExecutionContext> for PassThrough<S>
    where
        S: Service<ExecutionContext> + Send + 'static,
        S::Future: Send + 'static,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }
        fn call(&mut self, req: ExecutionContext) -> Self::Future {
            self.0.call(req)
        }
    }

    #[tokio::test]
    async fn layer_composition_compiles_and_runs() {
        let mock = MockLlmProvider::new("m");
        mock.push_text("through-layer");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);

        let svc = tower::ServiceBuilder::new()
            .layer(PassThroughLayer)
            .service(ExecutorService::new(ConversationExecutor));
        let outcome = svc.oneshot(ctx_with(llm, 5)).await.unwrap();
        assert_eq!(outcome.output, "through-layer");
    }

    #[tokio::test]
    async fn service_from_dyn_executes() {
        let mock = MockLlmProvider::new("m");
        mock.push_text("dyn ok");
        let llm: Arc<dyn LlmProvider> = Arc::new(mock);

        let exec: Arc<dyn Executor> = Arc::new(ConversationExecutor);
        let svc = service_from_dyn(exec);
        let outcome = svc.oneshot(ctx_with(llm, 5)).await.unwrap();
        assert_eq!(outcome.output, "dyn ok");
    }

    #[tokio::test]
    async fn clone_shares_executor() {
        let svc1 = ExecutorService::new(ConversationExecutor);
        let svc2 = svc1.clone();
        assert!(Arc::ptr_eq(&svc1.executor, &svc2.executor));
    }
}
