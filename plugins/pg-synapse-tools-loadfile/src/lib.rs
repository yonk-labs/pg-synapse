//! `load_csv` / `load_json`: get a file's rows into a table without the model
//! writing them out.
//!
//! ## Why this exists
//!
//! Profiling a file-import agent showed the cost was not the database. A
//! 15-row CSV took 78 seconds across 15 model turns, and after batching the
//! writes it still took 59 seconds across 7. What remained was **generation**:
//! the model emitting every row's values as tokens inside a multi-row INSERT.
//! Output tokens barely moved, because the rows still had to be typed out.
//!
//! No prompt fixes that. The only way to stop paying per row is to stop asking
//! the model for the rows. These tools read the file in-process, infer a
//! column set, and insert the data directly. The agent's job becomes one
//! `INSERT ... SELECT` that transforms a staging table into its real schema:
//! one statement regardless of whether the file has ten rows or ten thousand.
//!
//! ## Why not COPY
//!
//! `COPY ... FROM '/path'` reads server-side and needs `pg_read_server_files`
//! or superuser. Agents deliberately run as a constrained role, so reaching for
//! COPY would mean handing back the privilege that was taken away on purpose.
//! Reading the file here and inserting through the ordinary SQL path keeps the
//! agent inside its own rights.
//!
//! ## Everything lands as text
//!
//! Staging columns are `text`. Guessing types from a sample is how an import
//! silently turns a product code into a number, or a date into something in
//! the wrong locale. The agent casts explicitly in its transform, where a bad
//! cast is a visible error rather than a quiet corruption.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use pg_synapse_tools_fs::FsSandbox;
use pg_synapse_tools_sql::SqlExecutor;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// Rows inserted per statement. Large enough that the round trips do not
/// dominate, small enough that one statement stays comfortably inside
/// Postgres' parameter limit and does not build a multi-megabyte string.
const BATCH_ROWS: usize = 500;

/// Postgres allows 65535 bind parameters per statement; staying well under it
/// matters for a wide file, so the batch shrinks as columns grow.
fn rows_per_batch(columns: usize) -> usize {
    if columns == 0 {
        return BATCH_ROWS;
    }
    BATCH_ROWS.min(60_000 / columns.max(1)).max(1)
}

/// Turn an arbitrary header into a safe, lowercase column name.
///
/// Files in the wild have headers like "Order #", "" and "Total (USD)".
/// Rejecting those would make the tool useless on real data, so they are
/// normalized and the mapping is reported back rather than silently applied.
fn to_column_name(raw: &str, index: usize) -> String {
    // Runs of punctuation collapse to a single underscore: "Total (USD)" has
    // both a space and a bracket, and "total__usd" is a worse name than
    // "total_usd" for no reason.
    let mut cleaned = String::with_capacity(raw.len());
    for c in raw.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            cleaned.push(c);
        } else if !cleaned.ends_with('_') {
            cleaned.push('_');
        }
    }
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() || !trimmed.starts_with(|c: char| c.is_ascii_alphabetic()) {
        format!("col_{}", index + 1)
    } else {
        trimmed.chars().take(63).collect()
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Split a possibly schema-qualified table name and quote both halves.
fn quote_table(raw: &str, tool: &str) -> Result<String, ToolError> {
    let parts: Vec<&str> = raw.split('.').collect();
    let bad = parts.is_empty() || parts.len() > 2 || parts.iter().any(|p| p.trim().is_empty());
    if bad {
        return Err(ToolError::InvalidInput {
            name: tool.into(),
            reason: format!("table must be \"name\" or \"schema.name\", got \"{raw}\""),
        });
    }
    Ok(parts
        .iter()
        .map(|p| quote_ident(p.trim()))
        .collect::<Vec<_>>()
        .join("."))
}

#[derive(JsonSchema, Deserialize)]
struct LoadArgs {
    /// Path of an uploaded file, relative to the sandbox.
    #[serde(alias = "file", alias = "filename")]
    path: String,
    /// Where to put the rows, as "table" or "schema.table". Created if absent.
    #[serde(alias = "target", alias = "into")]
    table: String,
    /// Empty the table before loading. Default false, so a second load appends
    /// rather than silently discarding what was there.
    #[serde(default)]
    replace: bool,
}

/// Create the staging table and insert `rows`, returning how many landed.
async fn stage_rows(
    sql: &dyn SqlExecutor,
    ctx: &ToolCtx,
    tool: &str,
    table: &str,
    columns: &[String],
    rows: &[Vec<Option<String>>],
    replace: bool,
) -> Result<u64, ToolError> {
    let qualified = quote_table(table, tool)?;
    let exec_id = ctx.execution_id.to_string();

    let col_defs = columns
        .iter()
        .map(|c| format!("{} text", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");
    sql.execute(
        &format!("CREATE TABLE IF NOT EXISTS {qualified} ({col_defs})"),
        &[],
        ctx.caller_role.as_deref(),
        Some(&exec_id),
    )
    .await?;

    if replace {
        sql.execute(
            &format!("DELETE FROM {qualified}"),
            &[],
            ctx.caller_role.as_deref(),
            Some(&exec_id),
        )
        .await?;
    }

    let col_list = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let per_batch = rows_per_batch(columns.len());
    let mut total = 0u64;

    for chunk in rows.chunks(per_batch) {
        let mut placeholders = Vec::with_capacity(chunk.len());
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * columns.len());
        let mut n = 1;
        for row in chunk {
            let mut slots = Vec::with_capacity(columns.len());
            for i in 0..columns.len() {
                slots.push(format!("${n}"));
                n += 1;
                // Values are bound, never interpolated: a cell containing a
                // quote or a semicolon is data, and this is the one place a
                // file's contents could otherwise reach the parser.
                params.push(match row.get(i).and_then(|v| v.clone()) {
                    Some(v) => Value::String(v),
                    None => Value::Null,
                });
            }
            placeholders.push(format!("({})", slots.join(", ")));
        }
        total += sql
            .execute(
                &format!(
                    "INSERT INTO {qualified} ({col_list}) VALUES {}",
                    placeholders.join(", ")
                ),
                &params,
                ctx.caller_role.as_deref(),
                Some(&exec_id),
            )
            .await?;
    }
    Ok(total)
}

fn describe(table: &str, columns: &[String], rows: u64) -> ToolOutput {
    ToolOutput::Json(json!({
        "table": table,
        "columns": columns,
        "rows_loaded": rows,
        // Told explicitly, because the whole point is that the agent now writes
        // one transforming statement rather than the rows themselves.
        "next": format!(
            "Every column is text. Transform with a single INSERT ... SELECT ... FROM {table}, \
             casting as needed. Do not re-read the file."
        )
    }))
}

/// `load_csv`: header row becomes the columns, every value stays text.
pub struct LoadCsvTool {
    pub sandbox: Arc<FsSandbox>,
    pub sql: Arc<dyn SqlExecutor>,
}

#[async_trait]
impl Tool for LoadCsvTool {
    fn name(&self) -> &str {
        "load_csv"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| ToolSchema::from_root(schemars::schema_for!(LoadArgs)))
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: LoadArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "load_csv".into(),
                reason: e.to_string(),
            })?;
        let path = self.sandbox.resolve(&args.path, "load_csv")?;
        let bytes = std::fs::read(&path).map_err(|e| ToolError::Execution {
            name: "load_csv".into(),
            reason: format!("could not read {}: {e}", args.path),
        })?;

        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(bytes.as_slice());
        let headers = reader
            .headers()
            .map_err(|e| ToolError::Execution {
                name: "load_csv".into(),
                reason: format!("could not read the header row: {e}"),
            })?
            .clone();
        let columns: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| to_column_name(h, i))
            .collect();
        if columns.is_empty() {
            return Err(ToolError::Execution {
                name: "load_csv".into(),
                reason: "the file has no header row".into(),
            });
        }

        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        for rec in reader.records() {
            let rec = rec.map_err(|e| ToolError::Execution {
                name: "load_csv".into(),
                reason: format!("malformed row: {e}"),
            })?;
            // A short row is padded rather than rejected. Real exports have
            // ragged tails, and losing the file over one is not a kindness.
            let mut row: Vec<Option<String>> = rec
                .iter()
                .map(|v| {
                    let v = v.trim();
                    if v.is_empty() {
                        None
                    } else {
                        Some(v.to_string())
                    }
                })
                .collect();
            row.resize(columns.len(), None);
            row.truncate(columns.len());
            rows.push(row);
        }

        let n = stage_rows(
            self.sql.as_ref(),
            ctx,
            "load_csv",
            &args.table,
            &columns,
            &rows,
            args.replace,
        )
        .await?;
        Ok(describe(&args.table, &columns, n))
    }
}

/// `load_json`: a JSON array of objects, or one object per line (JSONL).
///
/// Columns are the union of every object's keys, so a record missing a field
/// gets a NULL rather than shifting the others along.
pub struct LoadJsonTool {
    pub sandbox: Arc<FsSandbox>,
    pub sql: Arc<dyn SqlExecutor>,
}

#[async_trait]
impl Tool for LoadJsonTool {
    fn name(&self) -> &str {
        "load_json"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| ToolSchema::from_root(schemars::schema_for!(LoadArgs)))
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: LoadArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "load_json".into(),
                reason: e.to_string(),
            })?;
        let path = self.sandbox.resolve(&args.path, "load_json")?;
        let text = std::fs::read_to_string(&path).map_err(|e| ToolError::Execution {
            name: "load_json".into(),
            reason: format!("could not read {}: {e}", args.path),
        })?;

        let records = parse_records(&text)?;
        if records.is_empty() {
            return Err(ToolError::Execution {
                name: "load_json".into(),
                reason: "no JSON objects found in the file".into(),
            });
        }

        // Union of keys, in first-seen order, so the column order matches how
        // the file reads rather than an arbitrary hash order.
        let mut columns: Vec<String> = Vec::new();
        for rec in &records {
            for key in rec.keys() {
                if !columns.iter().any(|c| c == key) {
                    columns.push(key.clone());
                }
            }
        }
        let col_names: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, k)| to_column_name(k, i))
            .collect();

        let rows: Vec<Vec<Option<String>>> = records
            .iter()
            .map(|rec| {
                columns
                    .iter()
                    .map(|k| match rec.get(k) {
                        None | Some(Value::Null) => None,
                        // A nested object or array is stored as its JSON text
                        // rather than flattened. Flattening invents a naming
                        // convention the agent then has to guess at.
                        Some(Value::String(s)) => Some(s.clone()),
                        Some(other) => Some(other.to_string()),
                    })
                    .collect()
            })
            .collect();

        let n = stage_rows(
            self.sql.as_ref(),
            ctx,
            "load_json",
            &args.table,
            &col_names,
            &rows,
            args.replace,
        )
        .await?;
        Ok(describe(&args.table, &col_names, n))
    }
}

/// Accept both a JSON array and JSONL, because exports come as both and making
/// the caller know which is a question the file can answer itself.
fn parse_records(text: &str) -> Result<Vec<serde_json::Map<String, Value>>, ToolError> {
    let trimmed = text.trim_start();
    let err = |reason: String| ToolError::Execution {
        name: "load_json".into(),
        reason,
    };

    if trimmed.starts_with('[') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|e| err(format!("invalid JSON: {e}")))?;
        let arr = parsed
            .as_array()
            .ok_or_else(|| err("expected an array of objects".into()))?;
        return arr
            .iter()
            .map(|v| {
                v.as_object()
                    .cloned()
                    .ok_or_else(|| err("every array element must be an object".into()))
            })
            .collect();
    }

    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| err(format!("invalid JSON on line {}: {e}", i + 1)))?;
        out.push(
            v.as_object()
                .cloned()
                .ok_or_else(|| err(format!("line {} is not an object", i + 1)))?,
        );
    }
    Ok(out)
}

/// Registers `load_csv` and `load_json`.
pub struct LoadFileToolsPlugin {
    sandbox: Arc<FsSandbox>,
    sql: Arc<dyn SqlExecutor>,
}

impl LoadFileToolsPlugin {
    pub fn new(sandbox: Arc<FsSandbox>, sql: Arc<dyn SqlExecutor>) -> Self {
        Self { sandbox, sql }
    }
}

impl Plugin for LoadFileToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-loadfile"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn register(self, registry: &mut Registry) {
        registry.tools.add_arc(
            "load_csv",
            Arc::new(LoadCsvTool {
                sandbox: self.sandbox.clone(),
                sql: self.sql.clone(),
            }),
        );
        registry.tools.add_arc(
            "load_json",
            Arc::new(LoadJsonTool {
                sandbox: self.sandbox.clone(),
                sql: self.sql.clone(),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_names_are_made_safe_and_stable() {
        assert_eq!(to_column_name("Order #", 0), "order");
        assert_eq!(to_column_name("Total (USD)", 1), "total_usd");
        assert_eq!(to_column_name("review_text", 2), "review_text");
        // Empty and numeric-leading headers fall back to a position, because a
        // real export has both and neither should lose the column.
        assert_eq!(to_column_name("", 3), "col_4");
        assert_eq!(to_column_name("2026", 4), "col_5");
    }

    #[test]
    fn identifiers_are_quoted_not_escaped_by_hand() {
        assert_eq!(quote_ident("plain"), "\"plain\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_table("t", "x").unwrap(), "\"t\"");
        assert_eq!(quote_table("s.t", "x").unwrap(), "\"s\".\"t\"");
        assert!(quote_table("a.b.c", "x").is_err());
        assert!(quote_table("", "x").is_err());
    }

    #[test]
    fn batch_size_shrinks_as_columns_grow() {
        assert_eq!(rows_per_batch(1), 500);
        // 60000 / 200 = 300, under the 500 cap, so a wide file batches smaller
        // and stays inside Postgres' parameter limit.
        assert_eq!(rows_per_batch(200), 300);
        assert_eq!(rows_per_batch(100_000), 1);
    }

    #[test]
    fn json_accepts_an_array_or_one_object_per_line() {
        let arr = parse_records(r#"[{"a":1},{"a":2}]"#).unwrap();
        assert_eq!(arr.len(), 2);
        let lines = parse_records("{\"a\":1}\n\n{\"a\":2}\n").unwrap();
        assert_eq!(lines.len(), 2);
        assert!(parse_records("[1,2]").is_err(), "scalars are not rows");
        assert!(parse_records("{bad").is_err());
    }
}
