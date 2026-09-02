//! The file lifecycle: list, process, re-process, delete.
//!
//! Uploading worked and then stopped: a file landed in the sandbox and nothing
//! could see it, run against it again, or remove it. An upload you cannot list
//! is an upload you have lost, and a processing run you cannot repeat is a
//! one-shot import pretending to be a pipeline.
//!
//! Files live in the directory shared with the database container at the same
//! absolute path, so a path returned here is directly usable by an agent's
//! `read_file` tool with no translation.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::HarnessError;
use crate::AppState;

/// Reject anything that is not a plain name in the uploads directory.
///
/// The directory is joined with a caller-supplied name, so this is the only
/// thing standing between a filename and the rest of the filesystem. Rejecting
/// separators and dot segments outright is simpler to reason about than
/// canonicalising afterwards and hoping the comparison is right.
fn safe_name(name: &str) -> Result<(), HarnessError> {
    let bad = name.is_empty()
        || name.len() > 200
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.');
    if bad {
        return Err(HarnessError::BadRequest(
            "file name must be a plain name, with no path separators or dot segments".to_owned(),
        ));
    }
    Ok(())
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, HarnessError> {
    let dir = std::path::Path::new(&state.upload_dir);
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push(json!({
                "filename": name,
                "path": format!("uploads/{}", entry.file_name().to_string_lossy()),
                "bytes": meta.len(),
                "modified_epoch": modified,
            }));
        }
    }
    // Newest first: the file someone just uploaded is the one they want.
    files.sort_by_key(|f| {
        std::cmp::Reverse(f.get("modified_epoch").and_then(Value::as_u64).unwrap_or(0))
    });
    Ok(Json(json!({"ok": true, "files": files})))
}

#[derive(Deserialize)]
pub struct ProcessReq {
    /// Agent to hand the file to. Defaults to the app named in `app`.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub app: Option<String>,
    /// Extra instruction appended to the standard one.
    #[serde(default)]
    pub instruction: Option<String>,
}

/// Hand a file to an agent, and do it the same way every time.
///
/// This exists so re-processing is a repeat of the same call rather than a
/// human retyping a prompt and getting a subtly different one. The instruction
/// names the exact path because an agent cannot list the uploads directory: it
/// can only read a path it was given.
pub async fn process(
    State(state): State<AppState>,
    Path(filename): Path<String>,
    Json(req): Json<ProcessReq>,
) -> Result<Json<Value>, HarnessError> {
    safe_name(&filename)?;
    let path = std::path::Path::new(&state.upload_dir).join(&filename);
    if !path.is_file() {
        return Err(HarnessError::NotFound(format!(
            "no uploaded file {filename}"
        )));
    }
    let agent = req
        .agent
        .or(req.app)
        .ok_or_else(|| HarnessError::BadRequest("name an agent or an app to process it".into()))?;

    let mut input = format!(
        "Process the uploaded file at path \"uploads/{filename}\". \
         Read it with read_file, then insert its rows into your own schema's tables, \
         using as few sql_exec calls as you can: one multi-row INSERT beats one call per row. \
         Finish with one line saying how many rows you added."
    );
    if let Some(extra) = req
        .instruction
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        input.push_str("\n\nAlso: ");
        input.push_str(extra);
    }

    // Reuses the same run registry the Run tab uses, so a processing run is
    // visible and cancellable exactly like any other run rather than being a
    // second, differently-shaped notion of "a job".
    crate::api::execute(State(state), Json(crate::api::ExecuteReq { agent, input })).await
}

pub async fn delete(
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<Json<Value>, HarnessError> {
    safe_name(&filename)?;
    let path = std::path::Path::new(&state.upload_dir).join(&filename);
    if !path.is_file() {
        return Err(HarnessError::NotFound(format!(
            "no uploaded file {filename}"
        )));
    }
    std::fs::remove_file(&path)
        .map_err(|e| HarnessError::BadRequest(format!("could not delete: {e}")))?;
    Ok(Json(json!({"ok": true, "deleted": filename})))
}
