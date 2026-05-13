//! Integration test for the MCP HTTP client against a wiremock-backed
//! mock server. Verifies the full handshake -> list -> call flow used by
//! [`pg_synapse_core::ToolRegistry::add_mcp`].

use pg_synapse_core::tool::{McpClient, McpTool};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use pg_synapse_core::{Tool, ToolError, ToolRegistry};
use serde_json::{Value, json};
use std::sync::Arc;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn rpc_ok(id: u64, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

async fn mount_initialize(server: &MockServer) {
    let body = rpc_ok(
        1,
        json!({
            "serverInfo": { "name": "mock-mcp", "version": "0.1.0" },
            "protocolVersion": "2025-11-25",
            "capabilities": {}
        }),
    );
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({"method": "initialize"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_list_one_tool(server: &MockServer) {
    let body = rpc_ok(
        2,
        json!({
            "tools": [
                {
                    "name": "echo",
                    "description": "echo a string",
                    "inputSchema": {
                        "title": "EchoInput",
                        "type": "object",
                        "properties": { "message": { "type": "string" } }
                    }
                }
            ]
        }),
    );
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({"method": "tools/list"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_call_echo(server: &MockServer) {
    let body = rpc_ok(
        3,
        json!({
            "content": [
                { "type": "text", "text": "echo: hello" }
            ],
            "isError": false
        }),
    );
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({"method": "tools/call"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn mcp_client_handshake_lists_and_calls_one_tool() {
    let server = MockServer::start().await;
    mount_initialize(&server).await;
    mount_list_one_tool(&server).await;
    mount_call_echo(&server).await;

    let client = McpClient::connect(&server.uri())
        .await
        .expect("connect succeeds");
    assert_eq!(client.server_info().name, "mock-mcp");
    assert_eq!(client.server_info().version, "0.1.0");

    let defs = client.list_tools().await.expect("list_tools");
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "echo");

    let out = client
        .call_tool("echo", json!({"message": "hello"}))
        .await
        .expect("call_tool");
    match out {
        ToolOutput::Text(t) => assert_eq!(t, "echo: hello"),
        other => panic!("unexpected output: {other:?}"),
    }
}

#[tokio::test]
async fn tool_registry_add_mcp_registers_each_advertised_tool() {
    let server = MockServer::start().await;
    mount_initialize(&server).await;
    mount_list_one_tool(&server).await;
    mount_call_echo(&server).await;

    let mut reg = ToolRegistry::new();
    reg.add_mcp(&server.uri()).await.expect("add_mcp");
    assert_eq!(reg.len(), 1);
    let echo = reg.get("echo").expect("echo present");
    let out = echo
        .run(json!({"message": "hello"}), &ToolCtx::default())
        .await
        .expect("tool runs");
    match out {
        ToolOutput::Text(t) => assert_eq!(t, "echo: hello"),
        other => panic!("unexpected output: {other:?}"),
    }
}

#[tokio::test]
async fn tool_call_is_error_true_maps_to_execution_error() {
    let server = MockServer::start().await;
    mount_initialize(&server).await;
    mount_list_one_tool(&server).await;

    let err_body = rpc_ok(
        3,
        json!({
            "content": [ { "type": "text", "text": "boom" } ],
            "isError": true
        }),
    );
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({"method": "tools/call"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(err_body))
        .mount(&server)
        .await;

    let client = Arc::new(McpClient::connect(&server.uri()).await.unwrap());
    let schema = ToolSchema::from_json_value(json!({
        "title": "EchoInput", "type": "object",
        "properties": { "message": { "type": "string" } }
    }))
    .unwrap();
    let tool = McpTool::new(client, "echo".into(), schema);
    let err = tool
        .run(json!({"message": "x"}), &ToolCtx::default())
        .await
        .unwrap_err();
    match err {
        ToolError::Execution { name, reason } => {
            assert_eq!(name, "echo");
            assert!(reason.contains("boom"));
        }
        other => panic!("expected Execution error, got {other:?}"),
    }
}

#[tokio::test]
async fn rpc_level_error_maps_to_mcp_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({"method": "initialize"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "method not found" }
        })))
        .mount(&server)
        .await;
    let err = McpClient::connect(&server.uri()).await.unwrap_err();
    match err {
        ToolError::Mcp(msg) => assert!(msg.contains("method not found")),
        other => panic!("expected Mcp error, got {other:?}"),
    }
}

#[tokio::test]
async fn http_500_maps_to_mcp_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let err = McpClient::connect(&server.uri()).await.unwrap_err();
    match err {
        ToolError::Mcp(msg) => assert!(msg.contains("500")),
        other => panic!("expected Mcp error, got {other:?}"),
    }
}
