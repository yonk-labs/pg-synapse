//! Integration tests for the HTTP tool plugin, driven through `wiremock`.
//!
//! Each test spins up an ephemeral mock server, points the corresponding tool
//! at it, and inspects the returned `ToolOutput::Json` envelope.

use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_tools_http::{HttpGet, HttpHead, HttpPost};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn json(out: ToolOutput) -> serde_json::Value {
    match out {
        ToolOutput::Json(v) => v,
        other => panic!("expected Json output, got {:?}", other),
    }
}

#[tokio::test]
async fn http_get_returns_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("world"))
        .mount(&server)
        .await;

    let template = HttpGet {
        url: String::new(),
        headers: Default::default(),
    };
    let url = format!("{}/hello", server.uri());
    let out = template
        .run(serde_json::json!({"url": url}), &ToolCtx::default())
        .await
        .unwrap();
    let v = json(out);
    assert_eq!(v["status"], 200);
    assert_eq!(v["body"], "world");
}

#[tokio::test]
async fn http_get_passes_headers_through() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/h"))
        .and(header("x-test", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let template = HttpGet {
        url: String::new(),
        headers: Default::default(),
    };
    let url = format!("{}/h", server.uri());
    let out = template
        .run(
            serde_json::json!({"url": url, "headers": {"x-test": "1"}}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    let v = json(out);
    assert_eq!(v["status"], 200);
}

#[tokio::test]
async fn http_get_returns_404_as_data_not_error() {
    // Non-2xx responses are not errors: they're real data the agent must see.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;

    let template = HttpGet {
        url: String::new(),
        headers: Default::default(),
    };
    let url = format!("{}/missing", server.uri());
    let out = template
        .run(serde_json::json!({"url": url}), &ToolCtx::default())
        .await
        .unwrap();
    let v = json(out);
    assert_eq!(v["status"], 404);
    assert_eq!(v["body"], "nope");
}

#[tokio::test]
async fn http_post_sends_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/echo"))
        .and(body_json(serde_json::json!({"k": "v"})))
        .respond_with(ResponseTemplate::new(201).set_body_string("created"))
        .mount(&server)
        .await;

    let template = HttpPost {
        url: String::new(),
        body: serde_json::Value::Null,
        headers: Default::default(),
    };
    let url = format!("{}/echo", server.uri());
    let out = template
        .run(
            serde_json::json!({"url": url, "body": {"k": "v"}}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    let v = json(out);
    assert_eq!(v["status"], 201);
    assert_eq!(v["body"], "created");
}

#[tokio::test]
async fn http_head_returns_status_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/probe"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-custom", "abc")
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let template = HttpHead {
        url: String::new(),
        headers: Default::default(),
    };
    let url = format!("{}/probe", server.uri());
    let out = template
        .run(serde_json::json!({"url": url}), &ToolCtx::default())
        .await
        .unwrap();
    let v = json(out);
    assert_eq!(v["status"], 200);
    assert_eq!(v["headers"]["x-custom"], "abc");
    assert_eq!(v["headers"]["content-type"], "application/json");
}

#[tokio::test]
async fn invalid_input_url_missing_returns_invalid_input_error() {
    let template = HttpGet {
        url: String::new(),
        headers: Default::default(),
    };
    // No "url" field: serde must reject the input.
    let err = template
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .expect_err("missing required field should error");
    match err {
        ToolError::InvalidInput { name, .. } => assert_eq!(name, "http_get"),
        other => panic!("expected InvalidInput, got {:?}", other),
    }
}

#[tokio::test]
async fn network_error_surfaces_as_execution_error() {
    // Bind a TCP listener then drop it, freeing the port. The HTTP client
    // pointed at the freed port will fail to connect with a deterministic
    // ConnectionRefused (or equivalent) reqwest error.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let template = HttpGet {
        url: String::new(),
        headers: Default::default(),
    };
    let url = format!("http://127.0.0.1:{port}/x");
    let err = template
        .run(serde_json::json!({"url": url}), &ToolCtx::default())
        .await
        .expect_err("unreachable host should error");
    match err {
        ToolError::Execution { name, reason } => {
            assert_eq!(name, "http_get");
            assert!(!reason.is_empty(), "reason must carry reqwest message");
        }
        other => panic!("expected Execution, got {:?}", other),
    }
}
