//! Compiling Tower `Layer` recipes for the kernel's `ExecutorService`.
//!
//! `ExecutorService<E>` implements
//! `tower::Service<ExecutionContext, Response = ExecutorOutcome, Error =
//! ExecutorError>`. Every recipe here wraps that service shape. The kernel
//! ships zero pre-built layers on purpose (design.md G4); operators compose
//! their own stack.
//!
//! Three self-contained recipes are demonstrated end to end:
//!
//! 1. `CostCapLayer`   - reject runs once accumulated USD spend crosses a cap.
//! 2. `RetryLayer`     - retry transient `LlmError::Network` /
//!    `LlmError::RateLimited` with exponential backoff.
//! 3. `DedupLayer`     - memoize identical inputs within a short TTL.
//!
//! Run with: `cargo run --example layer_recipes -p pg-synapse-core`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use pg_synapse_core::error::{ExecutorError, LlmError};
use pg_synapse_core::executor::ConversationExecutor;
use pg_synapse_core::executor::ExecutorService;
use pg_synapse_core::llm::LlmProvider;
use pg_synapse_core::testing::MockLlmProvider;
use pg_synapse_core::tool::ToolRegistry;
use pg_synapse_core::types::{ExecutionContext, ExecutorOutcome};
use tower::layer::Layer;
use tower::{Service, ServiceExt};
use uuid::Uuid;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

// ---------------------------------------------------------------------------
// Recipe 1: CostCapLayer
// ---------------------------------------------------------------------------

/// Rejects the run when cumulative spend (tracked across calls in a shared
/// counter) would exceed `cap_usd`. Real deployments would source the running
/// total from `synapse.executions`; here a process-lifetime counter keeps the
/// recipe self-contained.
#[derive(Clone)]
struct CostCapLayer {
    cap_usd: f64,
    spent_micros: Arc<AtomicU64>,
}

impl CostCapLayer {
    fn new(cap_usd: f64) -> Self {
        Self {
            cap_usd,
            spent_micros: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<S> Layer<S> for CostCapLayer {
    type Service = CostCap<S>;
    fn layer(&self, inner: S) -> Self::Service {
        CostCap {
            inner,
            cap_usd: self.cap_usd,
            spent_micros: Arc::clone(&self.spent_micros),
        }
    }
}

#[derive(Clone)]
struct CostCap<S> {
    inner: S,
    cap_usd: f64,
    spent_micros: Arc<AtomicU64>,
}

impl<S> Service<ExecutionContext> for CostCap<S>
where
    S: Service<ExecutionContext, Response = ExecutorOutcome, Error = ExecutorError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = ExecutorOutcome;
    type Error = ExecutorError;
    type Future = BoxFuture<Result<ExecutorOutcome, ExecutorError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, ctx: ExecutionContext) -> Self::Future {
        let cap = self.cap_usd;
        let spent = Arc::clone(&self.spent_micros);
        let already = spent.load(Ordering::SeqCst) as f64 / 1_000_000.0;
        let mut inner = self.inner.clone();
        Box::pin(async move {
            if already >= cap {
                return Err(ExecutorError::CostCapExceeded {
                    cap,
                    spent: already,
                });
            }
            let outcome = inner.call(ctx).await?;
            if let Some(c) = outcome.cost_usd {
                spent.fetch_add((c * 1_000_000.0) as u64, Ordering::SeqCst);
            }
            Ok(outcome)
        })
    }
}

// ---------------------------------------------------------------------------
// Recipe 2: RetryLayer
// ---------------------------------------------------------------------------

/// Retries when the executor surfaces a transient LLM error
/// (`LlmError::Network` or `LlmError::RateLimited`). Backoff doubles each
/// attempt starting from `base`. `tower-retry` is not enabled in the kernel's
/// feature set, so this is a hand-rolled `Service` rather than
/// `tower::retry::Retry`.
#[derive(Clone)]
struct RetryLayer {
    max_attempts: u32,
    base: Duration,
}

impl RetryLayer {
    fn new(max_attempts: u32, base: Duration) -> Self {
        Self { max_attempts, base }
    }
}

impl<S> Layer<S> for RetryLayer {
    type Service = Retry<S>;
    fn layer(&self, inner: S) -> Self::Service {
        Retry {
            inner,
            max_attempts: self.max_attempts,
            base: self.base,
        }
    }
}

#[derive(Clone)]
struct Retry<S> {
    inner: S,
    max_attempts: u32,
    base: Duration,
}

fn is_transient(err: &ExecutorError) -> bool {
    matches!(
        err,
        ExecutorError::Llm(LlmError::Network(_)) | ExecutorError::Llm(LlmError::RateLimited { .. })
    )
}

impl<S> Service<ExecutionContext> for Retry<S>
where
    S: Service<ExecutionContext, Response = ExecutorOutcome, Error = ExecutorError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = ExecutorOutcome;
    type Error = ExecutorError;
    type Future = BoxFuture<Result<ExecutorOutcome, ExecutorError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, ctx: ExecutionContext) -> Self::Future {
        let max = self.max_attempts;
        let base = self.base;
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let mut delay = base;
            let mut attempt = 0;
            loop {
                attempt += 1;
                match inner.call(ctx.shallow_clone()).await {
                    Ok(o) => return Ok(o),
                    Err(e) if attempt < max && is_transient(&e) => {
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                    }
                    Err(e) => return Err(e),
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Recipe 3: DedupLayer
// ---------------------------------------------------------------------------

/// Memoizes outcomes keyed by `(agent_name, input)` for `ttl`. A second
/// identical request inside the window returns the cached outcome without
/// re-running the executor. The cache is a `Mutex<HashMap>`; swap for a
/// bounded LRU in production.
#[derive(Clone)]
struct DedupLayer {
    ttl: Duration,
    cache: Arc<Mutex<HashMap<String, (Instant, ExecutorOutcome)>>>,
}

impl DedupLayer {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<S> Layer<S> for DedupLayer {
    type Service = Dedup<S>;
    fn layer(&self, inner: S) -> Self::Service {
        Dedup {
            inner,
            ttl: self.ttl,
            cache: Arc::clone(&self.cache),
        }
    }
}

#[derive(Clone)]
struct Dedup<S> {
    inner: S,
    ttl: Duration,
    cache: Arc<Mutex<HashMap<String, (Instant, ExecutorOutcome)>>>,
}

impl<S> Service<ExecutionContext> for Dedup<S>
where
    S: Service<ExecutionContext, Response = ExecutorOutcome, Error = ExecutorError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = ExecutorOutcome;
    type Error = ExecutorError;
    type Future = BoxFuture<Result<ExecutorOutcome, ExecutorError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, ctx: ExecutionContext) -> Self::Future {
        let key = format!("{}\u{1f}{}", ctx.agent_name, ctx.input);
        let ttl = self.ttl;
        let cache = Arc::clone(&self.cache);
        let mut inner = self.inner.clone();
        Box::pin(async move {
            if let Some((at, hit)) = cache.lock().unwrap().get(&key).cloned() {
                if at.elapsed() < ttl {
                    return Ok(hit);
                }
            }
            let outcome = inner.call(ctx).await?;
            cache
                .lock()
                .unwrap()
                .insert(key, (Instant::now(), outcome.clone()));
            Ok(outcome)
        })
    }
}

// ---------------------------------------------------------------------------
// Demonstration
// ---------------------------------------------------------------------------

fn ctx(llm: Arc<dyn LlmProvider>, input: &str) -> ExecutionContext {
    ExecutionContext {
        execution_id: Uuid::new_v4(),
        agent_name: "demo".into(),
        system_prompt: "be brief".into(),
        soul: None,
        input: input.into(),
        executor_name: "conversation".into(),
        tools: Arc::new(ToolRegistry::new()),
        llm,
        judge_llm: None,
        small_llm: None,
        embeddings: None,
        memory: None,
        compressor: None,
        max_iterations: 4,
        timeout: Duration::from_millis(500),
        cost_cap_usd: None,
        caller_role: None,
        trace_level: pg_synapse_core::types::TraceLevel::default(),
    }
}

#[tokio::main]
async fn main() {
    // Stack: CostCap -> Retry -> Dedup -> ExecutorService(conversation).
    let mock = MockLlmProvider::new("mock");
    mock.push_text("first answer");
    mock.push_text("second answer");
    let llm: Arc<dyn LlmProvider> = Arc::new(mock);

    let stack = tower::ServiceBuilder::new()
        .layer(CostCapLayer::new(1.00))
        .layer(RetryLayer::new(3, Duration::from_millis(10)))
        .layer(DedupLayer::new(Duration::from_secs(30)))
        .service(ExecutorService::new(ConversationExecutor));

    let out1 = stack
        .clone()
        .oneshot(ctx(Arc::clone(&llm), "hello"))
        .await
        .expect("first run succeeds");
    println!("run 1 output: {}", out1.output);

    // Same (agent, input): served from the dedup cache, mock not consumed.
    let out2 = stack
        .clone()
        .oneshot(ctx(Arc::clone(&llm), "hello"))
        .await
        .expect("second run served from cache");
    println!("run 2 output (cached): {}", out2.output);

    assert_eq!(out1.output, out2.output);
    println!("layer_recipes example OK");
}
