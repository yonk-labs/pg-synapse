//! Thin tokio-postgres helpers. The harness is single-user, so it opens a
//! short-lived connection per request instead of holding a pool. Long-running
//! agent executions get their own dedicated connection whose backend PID is
//! recorded so the UI can cancel it with `pg_cancel_backend`.

use tokio_postgres::{Client, NoTls};

use crate::error::HarnessError;

/// Open a new connection and drive it on a background task.
pub async fn connect(db_url: &str) -> Result<Client, HarnessError> {
    let (client, conn) = tokio_postgres::connect(db_url, NoTls).await?;
    tokio::spawn(async move {
        // Connection errors surface through the client side; nothing to do.
        let _ = conn.await;
    });
    Ok(client)
}

/// Run a query whose rows each contain a single `to_jsonb(...)::text` column
/// and collect them as parsed JSON values.
pub async fn jsonb_rows(
    client: &Client,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> Result<Vec<serde_json::Value>, HarnessError> {
    let rows = client.query(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let text: String = row.get(0);
        out.push(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null));
    }
    Ok(out)
}

/// Run a query returning a single `jsonb ... ::text` cell and parse it.
pub async fn jsonb_one(
    client: &Client,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> Result<serde_json::Value, HarnessError> {
    let row = client.query_one(sql, params).await?;
    let text: String = row.get(0);
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}
