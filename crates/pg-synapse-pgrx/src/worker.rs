//! The background worker: agent runs that outlive the session that asked.
//!
//! Three problems share one cause, and this is the cause. Every audit row is
//! written by the backend that ran the agent, inside that backend's
//! transaction, which makes the record exactly as durable as the caller's
//! session and no more:
//!
//! - An inline trigger agent that **rejects** a write takes its own audit row
//!   down with the rollback. Measured: zero rows in `synapse.executions` after
//!   a rejection. The runs an auditor most wants to read are the ones that
//!   leave no trace.
//! - A run interrupted by **failover** never reaches its audit write, so it
//!   leaves not a failed row but no row at all.
//! - `execute_async` is **synchronous under the hood**. SPI is only legal on
//!   the backend thread that owns the transaction, so a spawned tokio task
//!   cannot record anything; the async contract (return a uuid, poll it) was
//!   preserved by running inline and returning the id afterwards.
//!
//! A background worker is a separate process with its own transactions, so it
//! can record work whose originating session has rolled back, gone away, or
//! never waited in the first place.
//!
//! This module is the first slice: real background execution for the queue.
//! It does not yet solve the inline-rejection case, which additionally needs a
//! handoff that survives the caller's rollback.
//!
//! **Requires `shared_preload_libraries`.** Postgres only accepts a worker
//! registered from a preloaded library; loaded any other way it logs
//! "must be registered in shared_preload_libraries" and carries on without
//! one. That is a LOG rather than an error, so a lazily loaded session is
//! unaffected either way. `pg_synapse.master_key` already requires preloading
//! for its own reasons, so this asks for nothing new of an operator who was
//! configuring secrets.

use std::time::Duration;

use pgrx::bgworkers::{BackgroundWorker, BackgroundWorkerBuilder, SignalWakeFlags};
use pgrx::prelude::*;

/// Register the worker, if an operator asked for one.
///
/// Called from `_PG_init`. Off unless `pg_synapse.worker_database` names a
/// database, because a worker has to connect to exactly one and there is no
/// safe guess: the extension may be installed in several databases or in none
/// that matter, and picking wrong means an idle process holding a connection
/// against the wrong catalog.
pub fn register() {
    let Some(db) = crate::schema_guc::worker_database() else {
        return;
    };
    BackgroundWorkerBuilder::new("pg_synapse queue worker")
        .set_function("pg_synapse_worker_main")
        .set_library("pg_synapse_pgrx")
        // The database name travels as `extra` rather than being read from the
        // GUC in the worker: the worker starts before it can read settings,
        // and this way what it connects to is fixed at registration.
        .set_extra(&db)
        .enable_spi_access()
        // Restart rather than stay dead. A worker that exits on a transient
        // database error and never returns is a queue that silently stops
        // draining, which looks exactly like an empty queue.
        .set_restart_time(Some(Duration::from_secs(10)))
        .load();
}

/// The worker's main loop.
///
/// Deliberately thin: `synapse.drain_queue` already claims jobs with
/// `FOR UPDATE SKIP LOCKED`, runs them, and writes the results back, and it is
/// already concurrency safe because it was written for several callers polling
/// at once. The worker is a clock and a transaction around it, not a second
/// implementation of the same logic.
// The second `unsafe` in this crate, and the narrowest kind. Postgres finds a
// worker's entry point with dlsym by the name given to `set_function`, so the
// symbol has to survive name mangling; verified by `nm -D`, which finds
// nothing without this. `no_mangle` is unsafe only because two crates
// exporting one name is undefined at link time, hence the deliberately
// specific `pg_synapse_` prefix. No unsafe block, nothing dereferenced.
// pgrx's own documented example omits it and does not link as written.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
#[pg_guard]
pub extern "C-unwind" fn pg_synapse_worker_main(_arg: pg_sys::Datum) {
    // SIGTERM so a shutdown is prompt, SIGHUP so a reloaded configuration is
    // picked up at the next tick rather than at the next restart.
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    let db = BackgroundWorker::get_extra().to_owned();
    BackgroundWorker::connect_worker_to_spi(Some(&db), None);
    log!("pg_synapse queue worker started against database '{db}'");

    while BackgroundWorker::wait_latch(Some(Duration::from_millis(
        crate::schema_guc::WORKER_INTERVAL_MS.get().max(1) as u64,
    ))) {
        // One transaction per tick, owned by this process. This is the whole
        // point: what it records does not depend on any caller's session
        // still existing, or on that session having committed.
        //
        // A tick that fails is logged and the loop continues. Draining a queue
        // is inherently retryable, and a worker that dies on one bad job stops
        // draining every good one behind it.
        let result = std::panic::catch_unwind(|| {
            BackgroundWorker::transaction(|| {
                Spi::get_one::<i32>("SELECT synapse.drain_queue(1)").unwrap_or(Some(0))
            })
        });
        match result {
            Ok(Some(n)) if n > 0 => log!("pg_synapse queue worker ran {n} job(s)"),
            Ok(_) => {}
            Err(_) => {
                log!("pg_synapse queue worker: a tick failed, continuing");
            }
        }
    }
    log!("pg_synapse queue worker shutting down");
}
