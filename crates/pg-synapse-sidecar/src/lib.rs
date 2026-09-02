//! Library surface for `pg-synapse-sidecar`.
//!
//! Exists so other crates in the workspace can reuse the sidecar's
//! sqlx-backed [`db::SqlxSqlExecutor`] to open an ad-hoc connection to a
//! remote Postgres and run `sql_query` / `sql_exec`-shaped statements
//! against it, the same way the sidecar binary connects to a managed
//! database it cannot extend. `pg-synapse-tools-remotedb` is the first
//! consumer: it looks up a named connection from `synapse.connections` and
//! hands the resolved credentials to a fresh [`db::SqlxSqlExecutor`].
//!
//! The `api` module (the sidecar's own HTTP surface) stays a binary-only
//! module in `main.rs`: it depends on `AppState`, which only exists there.

#![forbid(unsafe_code)]

pub mod db;
