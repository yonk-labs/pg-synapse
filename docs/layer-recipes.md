# Layer Recipes

The kernel ships exactly one Tower integration point: `ExecutorService<E>`. It
ships **zero** pre-built layers (design.md G4). You compose your own stack.

`ExecutorService<E>` implements:

```rust
impl<E: Executor + ?Sized + 'static> Service<ExecutionContext> for ExecutorService<E> {
    type Response = ExecutorOutcome;
    type Error    = ExecutorError;
    type Future   = Pin<Box<dyn Future<Output = Result<ExecutorOutcome, ExecutorError>> + Send>>;
}
```

It is `Clone` (each clone shares the inner executor via `Arc`). Wrap it with
`tower::ServiceBuilder`. Layers apply outermost-first: the first `.layer(...)`
sees the request before the ones below it.

`BoxedExecutorService` (`ExecutorService<dyn Executor>`) and
`service_from_dyn(Arc<dyn Executor>)` cover the dynamic-dispatch case where the
host picks the executor by name at request time.

Recipes 1, 2, and 5 below are compiled end to end in
`crates/pg-synapse-core/examples/layer_recipes.rs`. Run them with
`cargo run --example layer_recipes -p pg-synapse-core`. Recipe 3 (TracingLayer)
compiles against the same signature; recipe 4 (McpClientLayer) is an
illustrative sketch.

## 1. CostCapLayer

Reject the run when accumulated USD spend would cross a cap. A production
deployment would source the running total from `synapse.executions`; the
self-contained version uses a shared atomic counter.

```rust
#[derive(Clone)]
struct CostCapLayer {
    cap_usd: f64,
    spent_micros: Arc<AtomicU64>,
}

impl CostCapLayer {
    fn new(cap_usd: f64) -> Self {
        Self { cap_usd, spent_micros: Arc::new(AtomicU64::new(0)) }
    }
}

impl<S> Layer<S> for CostCapLayer {
    type Service = CostCap<S>;
    fn layer(&self, inner: S) -> Self::Service {
        CostCap { inner, cap_usd: self.cap_usd, spent_micros: Arc::clone(&self.spent_micros) }
    }
}

#[derive(Clone)]
struct CostCap<S> { inner: S, cap_usd: f64, spent_micros: Arc<AtomicU64> }

impl<S> Service<ExecutionContext> for CostCap<S>
where
    S: Service<ExecutionContext, Response = ExecutorOutcome, Error = ExecutorError>
        + Clone + Send + 'static,
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
                return Err(ExecutorError::CostCapExceeded { cap, spent: already });
            }
            let outcome = inner.call(ctx).await?;
            if let Some(c) = outcome.cost_usd {
                spent.fetch_add((c * 1_000_000.0) as u64, Ordering::SeqCst);
            }
            Ok(outcome)
        })
    }
}
```

## 2. RetryLayer

Retry transient LLM failures (`LlmError::Network`, `LlmError::RateLimited`)
with exponential backoff. `tower-retry` is not in the kernel feature set, so
this is a hand-rolled `Service`. Note `ctx.shallow_clone()`: `ExecutionContext`
is not `Clone` (it holds `Arc<dyn ...>` trait objects), so retries clone it
shallowly, sharing providers.

```rust
#[derive(Clone)]
struct RetryLayer { max_attempts: u32, base: Duration }

impl<S> Layer<S> for RetryLayer {
    type Service = Retry<S>;
    fn layer(&self, inner: S) -> Self::Service {
        Retry { inner, max_attempts: self.max_attempts, base: self.base }
    }
}

#[derive(Clone)]
struct Retry<S> { inner: S, max_attempts: u32, base: Duration }

fn is_transient(err: &ExecutorError) -> bool {
    matches!(
        err,
        ExecutorError::Llm(LlmError::Network(_))
            | ExecutorError::Llm(LlmError::RateLimited { .. })
    )
}

impl<S> Service<ExecutionContext> for Retry<S>
where
    S: Service<ExecutionContext, Response = ExecutorOutcome, Error = ExecutorError>
        + Clone + Send + 'static,
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
```

## 3. TracingLayer

One `tracing` span per execute, recording agent name, token totals, and
duration. The kernel already depends on `tracing`; the pgrx and sidecar hosts
attach a subscriber that lands structured events in `synapse.traces` (D6). This
layer compiles against the same `Service` signature as the recipes above.

```rust
#[derive(Clone)]
struct TracingLayer;

impl<S> Layer<S> for TracingLayer {
    type Service = Tracing<S>;
    fn layer(&self, inner: S) -> Self::Service { Tracing { inner } }
}

#[derive(Clone)]
struct Tracing<S> { inner: S }

impl<S> Service<ExecutionContext> for Tracing<S>
where
    S: Service<ExecutionContext, Response = ExecutorOutcome, Error = ExecutorError>
        + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = ExecutorOutcome;
    type Error = ExecutorError;
    type Future = BoxFuture<Result<ExecutorOutcome, ExecutorError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, ctx: ExecutionContext) -> Self::Future {
        let agent = ctx.agent_name.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let span = tracing::info_span!("execute", agent = %agent);
            let _g = span.enter();
            let result = inner.call(ctx).await;
            match &result {
                Ok(o) => tracing::info!(
                    tokens_in = o.tokens_in,
                    tokens_out = o.tokens_out,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "execute completed"
                ),
                Err(e) => tracing::warn!(error = %e, "execute failed"),
            }
            result
        })
    }
}
```

## 4. McpClientLayer (illustrative)

> Illustrative pseudo-code. It does not compile as-is: the kernel does not
> expose an MCP-call hook on `ExecutionContext`, so per-tool-call tracing for
> MCP tools belongs in a wrapping `Tool`, not a `Service` layer. Shown to
> document intent.

```rust
// ILLUSTRATIVE ONLY - does not compile as-is.
#[derive(Clone)]
struct McpClientLayer;

impl<S> Service<ExecutionContext> for McpClient<S> {
    fn call(&mut self, ctx: ExecutionContext) -> Self::Future {
        // Wrap each McpTool so every tools/call gets a child span recording
        // the MCP server URL, tool name, and round-trip latency. In practice
        // wrap the Tool (Path 3 in tool-authoring.md), not the executor.
        Box::pin(async move { self.inner.call(ctx).await })
    }
}
```

## 5. DedupLayer

Memoize outcomes keyed by `(agent_name, input)` for a short TTL. A second
identical request inside the window skips the executor.

```rust
#[derive(Clone)]
struct DedupLayer {
    ttl: Duration,
    cache: Arc<Mutex<HashMap<String, (Instant, ExecutorOutcome)>>>,
}

impl<S> Layer<S> for DedupLayer {
    type Service = Dedup<S>;
    fn layer(&self, inner: S) -> Self::Service {
        Dedup { inner, ttl: self.ttl, cache: Arc::clone(&self.cache) }
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
        + Clone + Send + 'static,
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
            cache.lock().unwrap().insert(key, (Instant::now(), outcome.clone()));
            Ok(outcome)
        })
    }
}
```

## Composing the stack

```rust
let stack = tower::ServiceBuilder::new()
    .layer(CostCapLayer::new(1.00))
    .layer(RetryLayer::new(3, Duration::from_millis(10)))
    .layer(DedupLayer::new(Duration::from_secs(30)))
    .service(ExecutorService::new(ConversationExecutor));

let outcome = stack.oneshot(ctx).await?;
```

Order matters: above, CostCap is checked first, then Retry wraps the inner
calls, then Dedup short-circuits before the executor runs. See the runnable
`examples/layer_recipes.rs` for the full, compiling program.
