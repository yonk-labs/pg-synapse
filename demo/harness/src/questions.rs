//! Named questions: save, list, and run.
//!
//! A question is compiled to SQL once and executed by Postgres on every later
//! invocation, so no model runs at query time. That is what makes a saved
//! question a *metric*: an answer that changes each time it is asked is not one,
//! and paying a model to recompute `count(*)` is indefensible.
//!
//! This module is a transport. It resolves a stored question and runs its SQL.
//! It makes no decisions about content, and no LLM call happens here.
//!
//! ## On executing stored SQL
//!
//! `run` interpolates `sql_text` into an outer `SELECT ... FROM (<sql>) r`.
//! That is safe only because `sql_text` is reviewed before use: `confirmed_at`
//! records that approval, and `run` refuses a question that lacks it. When an
//! agent starts authoring `sql_text`, the row is written with `confirmed_at`
//! NULL and stays unrunnable until a human approves it.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db;
use crate::error::HarnessError;
use crate::AppState;

/// App and question names are validated so a bad name fails with a clear
/// message rather than as a confusing lookup miss.
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Deserialize)]
pub struct SaveReq {
    pub app: String,
    pub name: String,
    pub nl_text: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub sql_text: Option<String>,
    /// Whether the SQL has been reviewed by a human. Defaults to false so
    /// anything written programmatically is unrunnable until approved.
    #[serde(default)]
    pub confirmed: bool,
}

fn default_kind() -> String {
    "sql".to_owned()
}

pub async fn save(
    State(state): State<AppState>,
    Json(req): Json<SaveReq>,
) -> Result<Json<Value>, HarnessError> {
    if !valid_ident(&req.app) || !valid_ident(&req.name) {
        return Err(HarnessError::BadRequest(
            "app and name must be lowercase letters, digits, and underscores, \
             starting with a letter, 63 characters or fewer"
                .to_owned(),
        ));
    }
    if req.kind != "sql" && req.kind != "agent" {
        return Err(HarnessError::BadRequest(
            "kind must be \"sql\" or \"agent\"".to_owned(),
        ));
    }
    if req.kind == "sql" && req.sql_text.as_deref().unwrap_or("").trim().is_empty() {
        return Err(HarnessError::BadRequest(
            "a sql question needs sql_text".to_owned(),
        ));
    }

    let client = db::connect(&state.db_url).await?;
    client
        .execute(
            "INSERT INTO synapse.questions (app, name, nl_text, kind, sql_text, confirmed_at) \
             VALUES ($1, $2, $3, $4, $5, CASE WHEN $6 THEN now() ELSE NULL END) \
             ON CONFLICT (app, name) DO UPDATE SET \
               nl_text = EXCLUDED.nl_text, kind = EXCLUDED.kind, \
               sql_text = EXCLUDED.sql_text, confirmed_at = EXCLUDED.confirmed_at",
            &[
                &req.app,
                &req.name,
                &req.nl_text,
                &req.kind,
                &req.sql_text,
                &req.confirmed,
            ],
        )
        .await?;

    Ok(Json(json!({
        "ok": true,
        "app": req.app,
        "name": req.name,
        "confirmed": req.confirmed
    })))
}

pub async fn list(
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let questions = db::jsonb_rows(
        &client,
        "SELECT to_jsonb(q)::text FROM ( \
           SELECT name, nl_text, kind, sql_text, \
                  (confirmed_at IS NOT NULL) AS confirmed \
           FROM synapse.questions WHERE app = $1 ORDER BY name) q",
        &[&app],
    )
    .await?;
    Ok(Json(
        json!({"ok": true, "app": app, "questions": questions}),
    ))
}

pub async fn run(
    State(state): State<AppState>,
    Path((app, name)): Path<(String, String)>,
) -> Result<Json<Value>, HarnessError> {
    let client = db::connect(&state.db_url).await?;
    let rows = client
        .query(
            "SELECT kind, sql_text, (confirmed_at IS NOT NULL) AS confirmed \
             FROM synapse.questions WHERE app = $1 AND name = $2",
            &[&app, &name],
        )
        .await?;
    let row = rows.first().ok_or_else(|| {
        HarnessError::NotFound(format!("no question named \"{name}\" for app \"{app}\""))
    })?;
    let kind: String = row.get(0);
    let sql_text: Option<String> = row.get(1);
    let confirmed: bool = row.get(2);

    if kind != "sql" {
        return Err(HarnessError::BadRequest(format!(
            "question \"{name}\" has kind \"{kind}\"; only sql questions run without an agent"
        )));
    }
    if !confirmed {
        return Err(HarnessError::BadRequest(format!(
            "question \"{name}\" has not been reviewed. Its SQL must be approved before it runs"
        )));
    }
    let sql = sql_text
        .ok_or_else(|| HarnessError::BadRequest(format!("question \"{name}\" has no sql_text")))?;

    let result = db::jsonb_rows(
        &client,
        &format!("SELECT to_jsonb(r)::text FROM ({sql}) r"),
        &[],
    )
    .await?;

    Ok(Json(json!({
        "ok": true,
        "app": app,
        "name": name,
        "row_count": result.len(),
        "rows": result
    })))
}

#[cfg(test)]
mod tests {
    use super::valid_ident;

    /// Names reach a SQL identifier position nowhere, but a rejected name is a
    /// clear error while an accepted bad one is a confusing lookup miss.
    #[test]
    fn identifier_validation() {
        assert!(valid_ident("postgres_news_tracker"));
        assert!(valid_ident("top_sources"));
        assert!(!valid_ident(""));
        assert!(!valid_ident("9lives"), "must start with a letter");
        assert!(!valid_ident("Top_Sources"), "uppercase rejected");
        assert!(!valid_ident("drop table"), "spaces rejected");
        assert!(!valid_ident("a\"b"), "quotes rejected");
        assert!(!valid_ident(&"a".repeat(64)), "over 63 chars rejected");
    }
}
