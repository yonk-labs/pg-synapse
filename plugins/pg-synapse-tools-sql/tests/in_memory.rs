//! Integration tests for `sql_query` / `sql_exec` against the in-memory
//! `SqlExecutor`. The pgrx and sidecar hosts will reuse the same trait with
//! a real database; these tests cover the tool layer only.

use std::sync::Arc;

use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_tools_sql::testing::{MemorySqlExecutor, RecordingSqlExecutor};
use pg_synapse_tools_sql::{SqlExecTool, SqlExecutor, SqlQueryTool};
use serde_json::json;

fn json_of(out: ToolOutput) -> serde_json::Value {
    match out {
        ToolOutput::Json(v) => v,
        other => panic!("expected Json output, got {:?}", other),
    }
}

#[tokio::test]
async fn query_returns_existing_rows_as_json_array() {
    let mem = Arc::new(MemorySqlExecutor::new());
    mem.insert_row("notes", json!({"id": 1, "text": "hello"}));
    mem.insert_row("notes", json!({"id": 2, "text": "world"}));
    let tool = SqlQueryTool {
        executor: mem.clone(),
    };
    let out = tool
        .run(
            json!({"query": "SELECT * FROM notes", "params": []}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    let arr = json_of(out);
    let rows = arr.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["text"], "hello");
    assert_eq!(rows[1]["text"], "world");
}

#[tokio::test]
async fn query_with_omitted_params_uses_default_empty_list() {
    let mem = Arc::new(MemorySqlExecutor::new());
    mem.insert_row("notes", json!({"id": 1}));
    let tool = SqlQueryTool { executor: mem };
    // No "params" key in input: serde default to empty Vec.
    let out = tool
        .run(json!({"query": "SELECT * FROM notes"}), &ToolCtx::default())
        .await
        .unwrap();
    let arr = json_of(out);
    assert_eq!(arr.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn exec_inserts_and_returns_rows_affected() {
    let mem = Arc::new(MemorySqlExecutor::new());
    let tool = SqlExecTool {
        executor: mem.clone(),
    };
    let out = tool
        .run(
            json!({
                "statement": "INSERT INTO notes VALUES ($1)",
                "params": [{"id": 1, "text": "first"}]
            }),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    let v = json_of(out);
    assert_eq!(v["rows_affected"], 1);
    assert_eq!(mem.rows("notes").len(), 1);
}

#[tokio::test]
async fn query_invalid_input_missing_query_field_returns_invalid_input() {
    let mem: Arc<dyn SqlExecutor> = Arc::new(MemorySqlExecutor::new());
    let tool = SqlQueryTool { executor: mem };
    let err = tool
        .run(json!({}), &ToolCtx::default())
        .await
        .expect_err("missing 'query' field must error");
    match err {
        ToolError::InvalidInput { name, .. } => assert_eq!(name, "sql_query"),
        other => panic!("expected InvalidInput, got {:?}", other),
    }
}

#[tokio::test]
async fn exec_invalid_input_missing_statement_field_returns_invalid_input() {
    let mem: Arc<dyn SqlExecutor> = Arc::new(MemorySqlExecutor::new());
    let tool = SqlExecTool { executor: mem };
    let err = tool
        .run(json!({}), &ToolCtx::default())
        .await
        .expect_err("missing 'statement' field must error");
    match err {
        ToolError::InvalidInput { name, .. } => assert_eq!(name, "sql_exec"),
        other => panic!("expected InvalidInput, got {:?}", other),
    }
}

#[tokio::test]
async fn caller_role_flows_through_to_executor() {
    let rec = Arc::new(RecordingSqlExecutor::new());
    let q = SqlQueryTool {
        executor: rec.clone(),
    };
    let e = SqlExecTool {
        executor: rec.clone(),
    };
    let ctx = ToolCtx {
        execution_id: uuid::Uuid::nil(),
        caller_role: Some("agent_role".into()),
        agent_name: Some("a1".into()),
    };
    q.run(json!({"query": "SELECT * FROM t", "params": [1]}), &ctx)
        .await
        .unwrap();
    e.run(
        json!({"statement": "DELETE FROM t WHERE id = $1", "params": [2]}),
        &ctx,
    )
    .await
    .unwrap();

    let qc = rec.query_calls.lock().unwrap();
    assert_eq!(qc.len(), 1);
    assert_eq!(qc[0].0, "SELECT * FROM t");
    assert_eq!(qc[0].1, vec![json!(1)]);
    assert_eq!(qc[0].2.as_deref(), Some("agent_role"));

    let ec = rec.execute_calls.lock().unwrap();
    assert_eq!(ec.len(), 1);
    assert_eq!(ec[0].0, "DELETE FROM t WHERE id = $1");
    assert_eq!(ec[0].1, vec![json!(2)]);
    assert_eq!(ec[0].2.as_deref(), Some("agent_role"));
}

#[tokio::test]
async fn unsupported_sql_returns_execution_error() {
    let mem: Arc<dyn SqlExecutor> = Arc::new(MemorySqlExecutor::new());
    let tool = SqlQueryTool { executor: mem };
    let err = tool
        .run(
            json!({"query": "SELECT col FROM t WHERE id = $1", "params": [1]}),
            &ToolCtx::default(),
        )
        .await
        .expect_err("MemorySqlExecutor rejects non-trivial SQL");
    match err {
        ToolError::Execution { name, .. } => assert_eq!(name, "sql_query"),
        other => panic!("expected Execution, got {:?}", other),
    }
}
