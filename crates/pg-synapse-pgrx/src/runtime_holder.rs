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
//!
//! ## Delegate tool wiring (tools-delegate feature)
//!
//! `call_agent` needs an `Arc<Kernel>` to re-enter the runtime. This creates
//! a bootstrapping cycle: the plugin must be registered BEFORE `build()`, but
//! the kernel Arc only exists AFTER `build()`. We resolve it with a two-phase
//! pattern:
//!
//! 1. During `build_kernel_from_db`, create `Arc<CallAgentTool>` (the shell)
//!    and register it via `DelegateToolsPlugin`. Save the shell Arc in the
//!    `DELEGATE_TOOL_PENDING` static.
//! 2. In `kernel_handle`, after `Arc::new(built)` produces the final kernel
//!    Arc, call `tool.inject(Arc::downgrade(&arc))` to wire the Weak handle.
//!    Clear `DELEGATE_TOOL_PENDING` so subsequent rebuilds work cleanly.

use std::sync::Arc;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use tokio::runtime::Runtime as TokioRuntime;

use pg_synapse_core::Runtime as Kernel;

#[cfg(feature = "tools-delegate")]
use pg_synapse_tools_delegate::CallAgentTool;

static TOKIO: OnceCell<TokioRuntime> = OnceCell::new();
static KERNEL: OnceCell<Mutex<Option<Arc<Kernel>>>> = OnceCell::new();

/// Holds the delegate tool shell between `build_kernel_from_db` (phase 1)
/// and the `Arc::new(built)` moment in `kernel_handle` (phase 2). Protected
/// by the same KERNEL Mutex so no extra synchronisation is needed.
#[cfg(feature = "tools-delegate")]
static DELEGATE_TOOL_PENDING: OnceCell<Mutex<Option<Arc<CallAgentTool>>>> = OnceCell::new();

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
    #[cfg(feature = "tools-delegate")]
    let _ = DELEGATE_TOOL_PENDING.set(Mutex::new(None));
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
        let arc = Arc::new(built);

        // Phase 2: inject the Weak handle into the pending delegate tool shell
        // now that we have the final Arc<Kernel>.
        #[cfg(feature = "tools-delegate")]
        if let Some(pending_slot) = DELEGATE_TOOL_PENDING.get() {
            let mut pending = pending_slot.lock();
            if let Some(tool) = pending.take() {
                tool.inject(Arc::downgrade(&arc));
            }
        }

        *guard = Some(arc);
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

    // Sandboxed filesystem tools (read_file, write_file, edit_file, list_files, grep).
    // Root defaults to /tmp/pg_synapse_fs; set pg_synapse.fs_tools_root GUC to override.
    // TODO: plumb as a proper GUC once pgrx GUC registration is added (see NOTES.md B10).
    #[cfg(feature = "tools-fs")]
    let builder = {
        let fs_root = "/tmp/pg_synapse_fs";
        if let Err(e) = std::fs::create_dir_all(fs_root) {
            tracing::warn!("could not create fs_tools root {fs_root}: {e}");
        }
        match pg_synapse_tools_fs::FsToolsPlugin::new(fs_root) {
            Ok(plugin) => builder.with_plugin(plugin),
            Err(e) => {
                tracing::warn!("FsToolsPlugin init failed, fs tools disabled: {e}");
                builder
            }
        }
    };

    // Lede compression tool (lede_compress). Shim: uses lede CLI if on PATH,
    // otherwise falls back to deterministic extractive compression.
    #[cfg(feature = "tools-lede")]
    let builder = builder.with_plugin(pg_synapse_tools_lede::LedeToolsPlugin::new());

    // Calculator tool (add/sub/mul/div).
    #[cfg(feature = "tools-calc")]
    let builder = builder.with_plugin(pg_synapse_tools_calc::CalcToolsPlugin::new());

    // Clock tool (get_current_time).
    #[cfg(feature = "tools-clock")]
    let builder = builder.with_plugin(pg_synapse_tools_clock::ClockToolsPlugin::new());

    // Delegation tool (call_agent) -- two-phase wiring.
    // Phase 1: create the shell, register it, park the ref in DELEGATE_TOOL_PENDING.
    // Phase 2 happens in kernel_handle() after Arc::new(built) is available.
    #[cfg(feature = "tools-delegate")]
    let builder = {
        let tool = Arc::new(CallAgentTool::empty());
        if let Some(pending_slot) = DELEGATE_TOOL_PENDING.get() {
            *pending_slot.lock() = Some(tool.clone());
        }
        builder.with_plugin(
            pg_synapse_tools_delegate::DelegateToolsPlugin::with_tool(tool),
        )
    };

    builder
        .load_profiles_from(source)
        .build()
        .await
        .map_err(|e| format!("kernel build failed: {e}"))
}
