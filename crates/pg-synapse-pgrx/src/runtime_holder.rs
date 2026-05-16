//! Singleton holder for the shared tokio runtime and the kernel `Runtime`.
//!
//! There's exactly one tokio runtime per Postgres backend (`_PG_init` builds
//! it). The kernel is wrapped in a `Mutex<Option<Arc<Kernel>>>` so admin
//! functions can invalidate it via [`rebuild_kernel`] and the next
//! `execute()` will rehydrate from the configuration tables.
//!
//! The tokio runtime is `current_thread` so `block_on` runs futures inline on
//! the calling Postgres backend thread. That's required because SPI calls
//! (used inside both [`crate::spi_executor::SpiSqlExecutor`] and
//! [`crate::spi_executor::SpiProfileSource`]) must execute on the backend
//! thread that owns the transaction; a multi-thread tokio runtime would hand
//! polling to a worker thread and break SPI.

use std::sync::Arc;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use tokio::runtime::Runtime as TokioRuntime;

use pg_synapse_core::Runtime as Kernel;

static TOKIO: OnceCell<TokioRuntime> = OnceCell::new();
static KERNEL: OnceCell<Mutex<Option<Arc<Kernel>>>> = OnceCell::new();

/// Build the shared tokio runtime. Called exactly once from `_PG_init`.
pub fn initialize_tokio_runtime() {
    let _ = TOKIO.set(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("pg_synapse_tokio")
            .build()
            .expect("build tokio runtime"),
    );
    let _ = KERNEL.set(Mutex::new(None));
}

/// Borrow the shared tokio runtime.
pub fn tokio() -> &'static TokioRuntime {
    TOKIO.get().expect("_PG_init must have been called")
}

/// Get the current kernel, building it on first access (or after a rebuild).
pub fn kernel_handle() -> Result<Arc<Kernel>, String> {
    let slot = KERNEL
        .get()
        .ok_or("kernel slot not initialized; _PG_init not called")?;
    let mut guard = slot.lock();
    if guard.is_none() {
        let built = tokio().block_on(build_kernel_from_db())?;
        *guard = Some(Arc::new(built));
    }
    Ok(guard.as_ref().unwrap().clone())
}

/// Mark the kernel cache as stale; the next `kernel_handle()` rebuilds.
pub fn rebuild_kernel() {
    if let Some(slot) = KERNEL.get() {
        *slot.lock() = None;
    }
}

async fn build_kernel_from_db() -> Result<Kernel, String> {
    use pg_synapse_core::Runtime;

    let source = crate::spi_executor::SpiProfileSource;
    let spi_exec: Arc<dyn pg_synapse_tools_sql::SqlExecutor> =
        Arc::new(crate::spi_executor::SpiSqlExecutor);

    let builder = Runtime::builder()
        .with_plugin(pg_synapse_provider_openai::OpenAiProviderFactory)
        .with_plugin(pg_synapse_tools_sql::SqlToolsPlugin::new(spi_exec));

    #[cfg(feature = "provider-llama-cpp")]
    let builder = builder
        .with_plugin(pg_synapse_provider_llama_cpp::LlamaCppProviderFactory)
        .with_plugin(pg_synapse_provider_llama_cpp::LlamaCppEmbeddingFactory);

    #[cfg(feature = "provider-anthropic")]
    let builder = builder.with_plugin(pg_synapse_provider_anthropic::AnthropicProviderFactory);

    #[cfg(feature = "embed-ort")]
    let builder = builder.with_plugin(pg_synapse_embeddings_ort::OrtEmbeddingFactory);

    builder
        .load_profiles_from(source)
        .build()
        .await
        .map_err(|e| format!("kernel build failed: {e}"))
}
