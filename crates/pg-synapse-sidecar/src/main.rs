//! pg-synapse-sidecar binary entry point.
//!
//! Boots an axum HTTP server that exposes the 12 v1 endpoints defined in
//! decisions D7. Configuration is accepted via CLI flags or environment
//! variables (see [`Cli`]).
//!
//! Startup sequence:
//! 1. Parse CLI / env.
//! 2. Init tracing-subscriber.
//! 3. Open a sqlx PgPool.
//! 4. Build the kernel Runtime (OpenAI provider + SQL tools).
//! 5. Bind the axum router and serve.
//!
//! Failures are logged to stderr (D8: LISTEN/NOTIFY deferred to v0.2).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod api;
mod db;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use pg_synapse_core::Runtime;
use pg_synapse_provider_openai::OpenAiProviderFactory;
use pg_synapse_tools_sql::SqlToolsPlugin;
use sqlx::PgPool;
use tracing::info;

use db::{SqlxProfileSource, SqlxSqlExecutor};

/// Sidecar HTTP host for pg_synapse.
///
/// Starts an axum server that exposes the same agent-loop capabilities as the
/// pgrx extension, addressable from any Postgres instance that can reach the
/// sidecar over HTTP. Configure the sidecar URL in `sidecar-install.sql`.
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Cli {
    /// TCP port the HTTP server listens on.
    #[arg(long, env = "PG_SYNAPSE_PORT", default_value = "8088")]
    pub port: u16,

    /// PostgreSQL connection string (e.g. postgres://user:pass@host/db).
    /// Required. The sidecar reads synapse.* tables from this database.
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Shared-secret token for /v1/admin/* endpoints.
    /// If not set, admin endpoints return 503. Pass via env to avoid
    /// leaking the token in process listings.
    #[arg(long, env = "PG_SYNAPSE_ADMIN_TOKEN")]
    pub admin_token: Option<String>,
}

/// Shared application state threaded into every axum handler via [`axum::extract::State`].
pub struct AppState {
    /// The kernel runtime (agent executor, LLM/embedding providers, tools).
    pub runtime: Runtime,
    /// Live database pool used by admin endpoints and async status queries.
    pub pool: PgPool,
    /// Admin token. None means admin endpoints are disabled.
    pub admin_token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pg_synapse_sidecar=info,warn".into()),
        )
        .init();

    let pool = PgPool::connect(&cli.database_url)
        .await
        .with_context(|| format!("connecting to database: {}", cli.database_url))?;

    let executor = Arc::new(SqlxSqlExecutor::new(pool.clone()));
    let source = SqlxProfileSource::new(pool.clone());

    let runtime = Runtime::builder()
        .with_plugin(OpenAiProviderFactory)
        .with_plugin(SqlToolsPlugin::new(executor))
        .load_profiles_from(source)
        .build()
        .await
        .context("building kernel Runtime")?;

    let state = Arc::new(AppState {
        runtime,
        pool,
        admin_token: cli.admin_token,
    });

    let router = api::router(state);
    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    eprintln!("pg-synapse-sidecar listening on {addr}");
    info!("pg-synapse-sidecar listening on {}", addr);

    axum::serve(listener, router).await.context("axum serve")?;

    Ok(())
}
