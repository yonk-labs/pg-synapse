//! Integration tests for pg-synapse-tools-fs.
//!
//! Tests are grouped into:
//! - Happy-path tool behavior (read, write, edit, list, grep).
//! - Error cases (missing file, ambiguous edit, etc.).
//! - Sandbox escape prevention (traversal, absolute path, symlink).

use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_tools_fs::FsToolsPlugin;
use serde_json::json;
use std::os::unix::fs as unix_fs;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test harness helpers
// ---------------------------------------------------------------------------

/// Build a populated registry backed by a fresh temp directory.
/// Returns `(registry, tempdir)`. Keep `tempdir` alive for the test's duration.
fn setup() -> (Registry, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let plugin = FsToolsPlugin::new(dir.path()).expect("plugin init");
    let mut reg = Registry::new();
    plugin.register(&mut reg);
    (reg, dir)
}

/// Call a tool by name with a JSON input and return the result.
async fn call(
    reg: &Registry,
    tool: &str,
    input: serde_json::Value,
) -> Result<ToolOutput, pg_synapse_core::error::ToolError> {
    let t = reg.tools.get(tool).expect("tool registered");
    t.run(input, &ToolCtx::default()).await
}

// ---------------------------------------------------------------------------
// Happy path tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_write_and_read_roundtrip() {
    let (reg, _dir) = setup();

    let w = call(
        &reg,
        "write_file",
        json!({ "path": "hello.txt", "content": "hello world" }),
    )
    .await
    .expect("write_file ok");
    assert!(matches!(w, ToolOutput::Json(_)));
    if let ToolOutput::Json(v) = &w {
        assert_eq!(v["bytes_written"], 11);
    }

    let r = call(&reg, "read_file", json!({ "path": "hello.txt" }))
        .await
        .expect("read_file ok");
    assert!(matches!(&r, ToolOutput::Text(s) if s == "hello world"));
}

#[tokio::test]
async fn test_write_creates_parent_dirs() {
    let (reg, _dir) = setup();

    call(
        &reg,
        "write_file",
        json!({ "path": "a/b/c/file.txt", "content": "nested" }),
    )
    .await
    .expect("write into nested dirs");

    let r = call(&reg, "read_file", json!({ "path": "a/b/c/file.txt" }))
        .await
        .expect("read nested file");
    assert!(matches!(&r, ToolOutput::Text(s) if s == "nested"));
}

#[tokio::test]
async fn test_read_missing_file_errors() {
    let (reg, _dir) = setup();
    let err = call(&reg, "read_file", json!({ "path": "nope.txt" }))
        .await
        .expect_err("should error on missing file");
    let msg = err.to_string();
    assert!(
        msg.contains("not found") || msg.contains("nope.txt"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn test_edit_unique_match() {
    let (reg, _dir) = setup();
    call(
        &reg,
        "write_file",
        json!({ "path": "edit.txt", "content": "foo bar baz" }),
    )
    .await
    .unwrap();

    let out = call(
        &reg,
        "edit_file",
        json!({ "path": "edit.txt", "old": "bar", "new": "QUX" }),
    )
    .await
    .expect("edit_file ok");
    assert!(matches!(out, ToolOutput::Json(_)));

    let r = call(&reg, "read_file", json!({ "path": "edit.txt" }))
        .await
        .unwrap();
    assert!(matches!(&r, ToolOutput::Text(s) if s == "foo QUX baz"));
}

#[tokio::test]
async fn test_edit_zero_matches_errors() {
    let (reg, _dir) = setup();
    call(
        &reg,
        "write_file",
        json!({ "path": "e.txt", "content": "hello" }),
    )
    .await
    .unwrap();

    let err = call(
        &reg,
        "edit_file",
        json!({ "path": "e.txt", "old": "nothere", "new": "x" }),
    )
    .await
    .expect_err("0 matches should error");
    let msg = err.to_string();
    assert!(
        msg.contains("0 matches") || msg.contains("not found"),
        "unexpected: {msg}"
    );
}

#[tokio::test]
async fn test_edit_multi_match_errors() {
    let (reg, _dir) = setup();
    call(
        &reg,
        "write_file",
        json!({ "path": "dup.txt", "content": "aa aa aa" }),
    )
    .await
    .unwrap();

    let err = call(
        &reg,
        "edit_file",
        json!({ "path": "dup.txt", "old": "aa", "new": "bb" }),
    )
    .await
    .expect_err("3 matches should error");
    let msg = err.to_string();
    assert!(
        msg.contains("3 times") || msg.contains("exactly once"),
        "unexpected: {msg}"
    );
}

#[tokio::test]
async fn test_list_files_entries() {
    let (reg, dir) = setup();

    // Create a file and a subdir.
    std::fs::write(dir.path().join("alpha.txt"), b"a").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let out = call(&reg, "list_files", json!({ "dir": "." }))
        .await
        .expect("list_files ok");
    if let ToolOutput::Json(v) = out {
        let entries = v["entries"].as_array().expect("entries array");
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"alpha.txt"),
            "expected alpha.txt in {names:?}"
        );
        assert!(names.contains(&"sub"), "expected sub in {names:?}");
        // Verify sorted order.
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "entries should be sorted");
        // Verify is_dir field.
        let sub = entries.iter().find(|e| e["name"] == "sub").unwrap();
        assert_eq!(sub["is_dir"], true);
    } else {
        panic!("expected Json output from list_files");
    }
}

#[tokio::test]
async fn test_grep_recursive() {
    let (reg, dir) = setup();

    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle is here\nnot here\n").unwrap();
    std::fs::write(dir.path().join("subdir/b.txt"), "also needle\nnothing\n").unwrap();

    let out = call(&reg, "grep", json!({ "pattern": "needle", "path": "." }))
        .await
        .expect("grep ok");
    if let ToolOutput::Json(v) = out {
        let matches = v["matches"].as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            2,
            "expected 2 needle matches, got {matches:?}"
        );
    } else {
        panic!("expected Json from grep");
    }
}

// ---------------------------------------------------------------------------
// Sandbox escape tests (CRITICAL)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sandbox_rejects_dotdot_traversal() {
    let (reg, _dir) = setup();

    let err = call(&reg, "read_file", json!({ "path": "../../etc/passwd" }))
        .await
        .expect_err("dotdot traversal must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("escapes sandbox") || msg.contains(".."),
        "expected sandbox escape error, got: {msg}"
    );
}

#[tokio::test]
async fn test_sandbox_rejects_absolute_path() {
    let (reg, _dir) = setup();

    let err = call(&reg, "read_file", json!({ "path": "/etc/passwd" }))
        .await
        .expect_err("absolute path must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("absolute") || msg.contains("not allowed"),
        "expected absolute-path error, got: {msg}"
    );
}

#[tokio::test]
async fn test_sandbox_rejects_symlink_escape() {
    let (reg, dir) = setup();

    // Create a symlink inside the sandbox that points outside it.
    let outside = tempfile::tempdir().expect("outside dir");
    std::fs::write(outside.path().join("secret.txt"), b"secret contents").unwrap();
    let link_path = dir.path().join("escape_link");
    unix_fs::symlink(outside.path().join("secret.txt"), &link_path).expect("create symlink");

    let err = call(&reg, "read_file", json!({ "path": "escape_link" }))
        .await
        .expect_err("symlink escape must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("escapes sandbox") || msg.contains("sandbox root"),
        "expected symlink-escape error, got: {msg}"
    );
}

#[tokio::test]
async fn test_plugin_registers_five_tools() {
    let dir = tempfile::tempdir().expect("temp dir");
    let plugin = FsToolsPlugin::new(dir.path()).expect("plugin init");
    let mut reg = Registry::new();
    plugin.register(&mut reg);
    let mut names = reg.tools.names();
    names.sort();
    assert_eq!(
        names,
        vec!["edit_file", "grep", "list_files", "read_file", "write_file"]
    );
}

#[tokio::test]
async fn test_grep_single_file() {
    let (reg, dir) = setup();
    std::fs::write(
        dir.path().join("src.txt"),
        "line one\ntarget line\nline three\n",
    )
    .unwrap();

    let out = call(
        &reg,
        "grep",
        json!({ "pattern": "target", "path": "src.txt" }),
    )
    .await
    .expect("grep single file");
    if let ToolOutput::Json(v) = out {
        let matches = v["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["line_no"], 2);
        assert!(matches[0]["line"].as_str().unwrap().contains("target"));
    } else {
        panic!("expected Json from grep");
    }
}

// ---------------------------------------------------------------------------
// Arg-alias leniency tests (B12)
// Each test verifies that a common LLM alias is accepted at deserialization.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_alias_read_file_via_file_field() {
    // Models often say {"file": "x.txt"} instead of {"path": "x.txt"}.
    let (reg, dir) = setup();
    std::fs::write(dir.path().join("alias_test.txt"), b"alias content").unwrap();

    let out = call(&reg, "read_file", json!({ "file": "alias_test.txt" }))
        .await
        .expect("read_file should accept 'file' alias for path");
    assert!(
        matches!(&out, ToolOutput::Text(s) if s == "alias content"),
        "unexpected output: {out:?}"
    );
}

#[tokio::test]
async fn test_alias_write_file_via_filename_and_text() {
    // Models sometimes say {"filename": ..., "text": ...}.
    let (reg, _dir) = setup();

    let w = call(
        &reg,
        "write_file",
        json!({ "filename": "aliaswrite.txt", "text": "written via aliases" }),
    )
    .await
    .expect("write_file should accept 'filename'/'text' aliases");
    assert!(matches!(w, ToolOutput::Json(_)));

    let r = call(&reg, "read_file", json!({ "path": "aliaswrite.txt" }))
        .await
        .expect("read back");
    assert!(
        matches!(&r, ToolOutput::Text(s) if s == "written via aliases"),
        "unexpected: {r:?}"
    );
}

#[tokio::test]
async fn test_alias_list_files_via_directory() {
    // Models often say {"directory": "subdir"} for list_files.
    let (reg, dir) = setup();
    std::fs::create_dir(dir.path().join("mydir")).unwrap();
    std::fs::write(dir.path().join("mydir/x.txt"), b"x").unwrap();

    let out = call(&reg, "list_files", json!({ "directory": "mydir" }))
        .await
        .expect("list_files should accept 'directory' alias for dir");
    if let ToolOutput::Json(v) = out {
        let entries = v["entries"].as_array().expect("entries");
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"x.txt"), "expected x.txt, got {names:?}");
    } else {
        panic!("expected Json from list_files");
    }
}

#[tokio::test]
async fn test_alias_list_files_via_path_field() {
    // Some models say {"path": "."} for list_files.
    let (reg, dir) = setup();
    std::fs::write(dir.path().join("rootfile.txt"), b"r").unwrap();

    let out = call(&reg, "list_files", json!({ "path": "." }))
        .await
        .expect("list_files should accept 'path' alias for dir");
    if let ToolOutput::Json(v) = out {
        let entries = v["entries"].as_array().expect("entries");
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"rootfile.txt"),
            "expected rootfile.txt, got {names:?}"
        );
    } else {
        panic!("expected Json from list_files with path alias");
    }
}

#[tokio::test]
async fn test_alias_edit_file_via_old_string_new_string() {
    // Claude-style tool use sends {"old_string": ..., "new_string": ...}.
    let (reg, _dir) = setup();
    call(
        &reg,
        "write_file",
        json!({ "path": "editalias.txt", "content": "alpha beta gamma" }),
    )
    .await
    .unwrap();

    let out = call(
        &reg,
        "edit_file",
        json!({
            "path": "editalias.txt",
            "old_string": "beta",
            "new_string": "DELTA"
        }),
    )
    .await
    .expect("edit_file should accept 'old_string'/'new_string' aliases");
    assert!(matches!(out, ToolOutput::Json(_)));

    let r = call(&reg, "read_file", json!({ "path": "editalias.txt" }))
        .await
        .unwrap();
    assert!(
        matches!(&r, ToolOutput::Text(s) if s == "alpha DELTA gamma"),
        "unexpected: {r:?}"
    );
}

#[tokio::test]
async fn test_alias_grep_via_query_field() {
    // Some models call the pattern field "query" or "search".
    let (reg, dir) = setup();
    std::fs::write(dir.path().join("grep_alias.txt"), b"foo bar\nbaz qux\n").unwrap();

    let out = call(
        &reg,
        "grep",
        json!({ "query": "bar", "path": "grep_alias.txt" }),
    )
    .await
    .expect("grep should accept 'query' alias for pattern");
    if let ToolOutput::Json(v) = out {
        let matches = v["matches"].as_array().expect("matches");
        assert_eq!(
            matches.len(),
            1,
            "expected 1 match for 'bar', got {matches:?}"
        );
        assert!(matches[0]["line"].as_str().unwrap().contains("bar"));
    } else {
        panic!("expected Json from grep with query alias");
    }
}

#[tokio::test]
async fn test_alias_write_file_via_filepath_and_contents() {
    // filepath and contents are also common.
    let (reg, _dir) = setup();

    call(
        &reg,
        "write_file",
        json!({ "filepath": "fp_alias.txt", "contents": "written via filepath+contents" }),
    )
    .await
    .expect("write_file should accept 'filepath'/'contents' aliases");

    let r = call(&reg, "read_file", json!({ "path": "fp_alias.txt" }))
        .await
        .expect("read back");
    assert!(
        matches!(&r, ToolOutput::Text(s) if s == "written via filepath+contents"),
        "unexpected: {r:?}"
    );
}
