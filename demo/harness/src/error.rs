//! Typed error surface for the demo harness. Every handler returns
//! `Result<_, HarnessError>`; the `IntoResponse` impl renders a JSON envelope
//! `{"ok": false, "error": "..."}` with an appropriate status code.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// A Postgres query failed.
    #[error("database error: {0}")]
    Db(#[from] tokio_postgres::Error),

    /// The request was malformed or referenced something unknown.
    #[error("{0}")]
    BadRequest(String),

    /// The requested resource does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The user-supplied LLM endpoint could not be reached.
    #[error("llm endpoint error: {0}")]
    Upstream(#[from] reqwest::Error),
}

impl IntoResponse for HarnessError {
    fn into_response(self) -> Response {
        let code = match &self {
            HarnessError::BadRequest(_) => StatusCode::BAD_REQUEST,
            HarnessError::NotFound(_) => StatusCode::NOT_FOUND,
            HarnessError::Upstream(_) => StatusCode::BAD_GATEWAY,
            HarnessError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = axum::Json(serde_json::json!({
            "ok": false,
            "error": self.to_string(),
        }));
        (code, body).into_response()
    }
}
