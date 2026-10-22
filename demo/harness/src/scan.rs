//! Brownfield: scanning an existing database, and the gate that stands in front
//! of it.
//!
//! Two jobs that look like one and are not.
//!
//! **Context for the machine.** In greenfield the agent knows the schema
//! because it invented it. In brownfield it knows nothing, and an LLM writing
//! SQL against an unknown database invents column names. The scan is what a
//! generated agent reads so it does not.
//!
//! **A gate for the human.** A readable account of what was found in the
//! customer's database, presented for confirmation before any agent is pointed
//! at it. For an enterprise buyer this is the more important of the two: it is
//! the moment a DBA gets to say no.
//!
//! The scan runs through `remote_query`, which opens its session read only and
//! has that enforced by the remote server. Looking at someone's production
//! database cannot modify it.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db;
use crate::error::HarnessError;
use crate::AppState;

/// Walk a connected database's catalog and store the result on the connection.
///
/// One query, not one per table: a schema with two hundred tables should cost
/// the same round trip as one with two.
pub async fn scan(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;

    let catalog_sql = "SELECT json_agg(t)::text FROM ( \
        SELECT c.table_schema, c.table_name, \
               json_agg(json_build_object( \
                 'column', c.column_name, 'type', c.data_type, \
                 'nullable', c.is_nullable = 'YES') ORDER BY c.ordinal_position) AS columns \
        FROM information_schema.columns c \
        WHERE c.table_schema NOT IN ('pg_catalog','information_schema') \
        GROUP BY c.table_schema, c.table_name \
        ORDER BY c.table_schema, c.table_name) t";

    let fk_sql = "SELECT json_agg(f)::text FROM ( \
        SELECT tc.table_schema, tc.table_name AS child, kcu.column_name AS fk_column, \
               ccu.table_name AS parent \
        FROM information_schema.table_constraints tc \
        JOIN information_schema.key_column_usage kcu ON tc.constraint_name = kcu.constraint_name \
        JOIN information_schema.constraint_column_usage ccu ON tc.constraint_name = ccu.constraint_name \
        WHERE tc.constraint_type = 'FOREIGN KEY' \
          AND tc.table_schema NOT IN ('pg_catalog','information_schema')) f";

    let tables = remote_json(&client, &name, catalog_sql).await?;
    let foreign_keys = remote_json(&client, &name, fk_sql).await?;

    let table_count = tables.as_array().map(Vec::len).unwrap_or(0);
    let scan = json!({
        "tables": tables,
        "foreign_keys": foreign_keys,
        "table_count": table_count,
    });

    client
        .execute(
            "UPDATE synapse.connections \
             SET scan_json = $2::text::jsonb, scanned_at = now(), \
                 reviewed_at = NULL, reviewed_by = NULL \
             WHERE name = $1",
            &[&name, &scan.to_string()],
        )
        .await?;

    // A re-scan clears the previous confirmation on purpose. The thing a human
    // approved was a particular picture of the database; if that picture has
    // changed, so has what they agreed to.
    Ok(Json(json!({
        "ok": true,
        "connection": name,
        "table_count": table_count,
        "scan": scan,
        "reviewed": false,
        "note": "review required before an agent may use this connection"
    })))
}

/// Run one read-only statement against a named remote connection by going
/// through `remote_query`, so the read-only enforcement and the credential
/// handling are the ones already tested rather than a second copy.
async fn remote_json(
    client: &tokio_postgres::Client,
    connection: &str,
    sql: &str,
) -> Result<Value, HarnessError> {
    let args = json!({"connection": connection, "query": sql});
    let row = client
        .query_one(
            "SELECT synapse.tool_call('remote_query', $1::text::jsonb)::text",
            &[&args.to_string()],
        )
        .await?;
    let raw: String = row.get(0);
    let out: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if let Some(err) = out.get("error").and_then(Value::as_str) {
        return Err(HarnessError::BadRequest(format!("scan failed: {err}")));
    }
    // remote_query returns an array of rows; each row here has one json column.
    let inner = out
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|r| r.as_object())
        .and_then(|o| o.values().next())
        .cloned()
        .unwrap_or(Value::Null);
    // The column is a json_agg rendered as text by ::text.
    Ok(match inner {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
        other => other,
    })
}

#[derive(Deserialize)]
pub struct ReviewReq {
    #[serde(default)]
    pub reviewed_by: Option<String>,
}

/// Record that a human has read the scan and accepts it.
pub async fn review(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<ReviewReq>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let n = client
        .execute(
            "UPDATE synapse.connections SET reviewed_at = now(), reviewed_by = $2 \
             WHERE name = $1 AND scan_json IS NOT NULL",
            &[
                &name,
                &req.reviewed_by.clone().unwrap_or_else(|| "operator".into()),
            ],
        )
        .await?;
    if n == 0 {
        return Err(HarnessError::BadRequest(format!(
            "connection \"{name}\" has no scan to review. Scan it first"
        )));
    }
    Ok(Json(
        json!({"ok": true, "connection": name, "reviewed": true}),
    ))
}

/// The gate itself: may an agent be pointed at this connection yet?
///
/// Separate from `connection_list` so the answer is one call and one meaning,
/// and so a caller cannot accidentally treat "a scan exists" as "a human
/// approved it". Those are different facts and conflating them is the whole
/// failure this prevents.
pub async fn gate(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let rows = client
        .query(
            "SELECT (scan_json IS NOT NULL) AS scanned, (reviewed_at IS NOT NULL) AS reviewed, \
                    reviewed_by, scanned_at \
             FROM synapse.connections WHERE name = $1",
            &[&name],
        )
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| HarnessError::NotFound(format!("no connection named \"{name}\"")))?;
    let scanned: bool = row.get(0);
    let reviewed: bool = row.get(1);
    let reason = if !scanned {
        "not scanned yet"
    } else if !reviewed {
        "scanned, but nobody has confirmed the scan"
    } else {
        "reviewed"
    };
    Ok(Json(json!({
        "ok": true,
        "connection": name,
        "scanned": scanned,
        "reviewed": reviewed,
        "may_attach_agent": reviewed,
        "reason": reason
    })))
}
