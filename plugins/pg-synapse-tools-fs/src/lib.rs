//! Filesystem tool plugin for pg_synapse: `read_file`, `write_file`, `edit_file`,
//! `list_files`, `grep`.
//!
//! All five tools share an [`FsSandbox`] that prevents path traversal and
//! symlink escapes. The sandbox root is configured at plugin construction time.
//!
//! ## Security model
//!
//! Every path argument is resolved against the sandbox root. The resolver:
//! 1. Rejects absolute paths (start with `/` or `\`).
//! 2. Rejects paths containing `..` components before touching the filesystem.
//! 3. Joins the relative argument onto the canonicalized root.
//! 4. Canonicalizes the deepest existing ancestor of the joined path.
//! 5. Verifies the result starts with the canonicalized root. If not, returns
//!    an `InvalidInput` error (catches symlink escapes).
//!
//! ## Arg-alias leniency
//!
//! Models frequently name tool arguments differently from the canonical field
//! names. Every input struct accepts the most common variants via
//! `#[serde(alias = "...")]`. The JSON schema still advertises only the
//! canonical names (schemars derives from field names, not aliases), so the
//! model is guided toward canonical names while aliased variants are silently
//! accepted at deserialization time.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use schemars::JsonSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

/// A sandboxed filesystem root. All tool paths are resolved relative to this.
#[derive(Debug, Clone)]
pub struct FsSandbox {
    root: PathBuf,
}

impl FsSandbox {
    /// Create a sandbox whose root is `root`. The root directory must exist.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let root = std::fs::canonicalize(root.into())?;
        Ok(Self { root })
    }

    /// Resolve `rel` to an absolute path inside the sandbox root.
    ///
    /// Returns `ToolError::InvalidInput` if `rel` is absolute, contains
    /// `..` components, or resolves to a path outside the root (including via
    /// symlinks).
    pub fn resolve(&self, rel: &str, tool_name: &str) -> Result<PathBuf, ToolError> {
        let rel = rel.trim();

        // Reject absolute paths.
        if rel.starts_with('/') || rel.starts_with('\\') {
            return Err(ToolError::InvalidInput {
                name: tool_name.into(),
                reason:
                    "absolute paths are not allowed; provide a path relative to the sandbox root"
                        .into(),
            });
        }

        // Reject explicit `..` components before touching the filesystem.
        for component in Path::new(rel).components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(ToolError::InvalidInput {
                    name: tool_name.into(),
                    reason: "path escapes sandbox root (.. component detected)".into(),
                });
            }
        }

        let joined = self.root.join(rel);

        // Canonicalize the deepest existing ancestor. This catches symlinks
        // that point outside the root even when the leaf does not yet exist.
        let canon = canonicalize_existing_prefix(&joined).map_err(|e| ToolError::InvalidInput {
            name: tool_name.into(),
            reason: format!("path resolution failed: {e}"),
        })?;

        if !canon.starts_with(&self.root) {
            return Err(ToolError::InvalidInput {
                name: tool_name.into(),
                reason: "path escapes sandbox root".into(),
            });
        }

        Ok(joined)
    }

    /// Canonicalized root path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Canonicalize the deepest existing ancestor of `path`.
///
/// Walks up the path one component at a time until it finds an existing
/// ancestor, canonicalizes it, then appends the remaining (non-existing)
/// suffix. This lets us resolve symlinks in the existing part of the path
/// even when the leaf file does not yet exist (e.g. write_file creating a new
/// file in a new directory).
fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, std::io::Error> {
    let mut components: Vec<_> = path.iter().collect();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();

    loop {
        let candidate: PathBuf = components.iter().collect();
        if candidate.exists() {
            let mut result = std::fs::canonicalize(&candidate)?;
            for s in suffix.iter().rev() {
                result = result.join(s);
            }
            return Ok(result);
        }
        match components.pop() {
            Some(part) => suffix.push(part.to_os_string()),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "path has no existing ancestor",
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared schema builder helper
// ---------------------------------------------------------------------------

fn build_schema<T: JsonSchema>() -> ToolSchema {
    let root = schema_for!(T);
    let val = serde_json::to_value(&root).expect("schemars output is always valid JSON");
    ToolSchema::from_json_value(val).expect("schemars schema is always a valid object")
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Deserialize a JSON value into `T`, returning a typed `ToolError::InvalidInput`
/// on failure.
fn parse_input<T: for<'de> Deserialize<'de>>(
    tool_name: &'static str,
    input: Value,
) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
        name: tool_name.into(),
        reason: e.to_string(),
    })
}

/// If `input` is a plain JSON string, wrap it as `{"field": <string>}`.
/// Useful for lenient single-arg tools.
fn coerce_string_to_field(input: Value, field: &str) -> Value {
    match input {
        Value::String(s) => serde_json::json!({ field: s }),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// 1. read_file
// ---------------------------------------------------------------------------

const MAX_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

/// Input schema for `read_file`.
///
/// Canonical field: `path`. Accepted aliases: `file`, `filename`, `filepath`,
/// `file_path`, `filePath`.
#[derive(Deserialize, JsonSchema, Debug)]
struct ReadFileInput {
    /// Path to the file to read, relative to the sandbox root.
    #[serde(
        alias = "file",
        alias = "filename",
        alias = "filepath",
        alias = "file_path",
        alias = "filePath"
    )]
    path: String,
}

/// Tool: read the contents of a file as UTF-8 text.
pub struct ReadFileTool {
    sandbox: Arc<FsSandbox>,
    schema: OnceLock<ToolSchema>,
}

impl ReadFileTool {
    fn new(sandbox: Arc<FsSandbox>) -> Self {
        Self {
            sandbox,
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<ReadFileInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: ReadFileInput = parse_input("read_file", input)?;
        let path = self.sandbox.resolve(args.path.trim(), "read_file")?;

        debug!("read_file: {:?}", path);

        if !path.exists() {
            return Err(ToolError::Execution {
                name: "read_file".into(),
                reason: format!("file not found: {}", args.path.trim()),
            });
        }
        if !path.is_file() {
            return Err(ToolError::Execution {
                name: "read_file".into(),
                reason: format!("not a file: {}", args.path.trim()),
            });
        }

        let meta = std::fs::metadata(&path).map_err(|e| ToolError::Execution {
            name: "read_file".into(),
            reason: format!("stat failed: {e}"),
        })?;

        if meta.len() > MAX_FILE_BYTES {
            return Err(ToolError::Execution {
                name: "read_file".into(),
                reason: format!("file is {} bytes, over the 1 MiB limit", meta.len()),
            });
        }

        let contents = std::fs::read_to_string(&path).map_err(|e| ToolError::Execution {
            name: "read_file".into(),
            reason: format!("read failed: {e}"),
        })?;

        Ok(ToolOutput::text(contents))
    }
}

// ---------------------------------------------------------------------------
// 2. write_file
// ---------------------------------------------------------------------------

/// Input schema for `write_file`.
///
/// Canonical fields: `path`, `content`. Path aliases: `file`, `filename`,
/// `filepath`, `file_path`, `filePath`. Content aliases: `text`, `data`,
/// `body`, `file_content`, `contents`.
#[derive(Deserialize, JsonSchema, Debug)]
struct WriteFileInput {
    /// Path to write, relative to the sandbox root. Parent directories are
    /// created automatically.
    #[serde(
        alias = "file",
        alias = "filename",
        alias = "filepath",
        alias = "file_path",
        alias = "filePath"
    )]
    path: String,
    /// File contents as a UTF-8 string.
    #[serde(
        alias = "text",
        alias = "data",
        alias = "body",
        alias = "file_content",
        alias = "contents"
    )]
    content: String,
}

/// Tool: write content to a file, creating parent directories as needed.
pub struct WriteFileTool {
    sandbox: Arc<FsSandbox>,
    schema: OnceLock<ToolSchema>,
}

impl WriteFileTool {
    fn new(sandbox: Arc<FsSandbox>) -> Self {
        Self {
            sandbox,
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<WriteFileInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: WriteFileInput = parse_input("write_file", input)?;
        let path = self.sandbox.resolve(args.path.trim(), "write_file")?;

        debug!("write_file: {:?}", path);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::Execution {
                name: "write_file".into(),
                reason: format!("create_dir_all failed: {e}"),
            })?;
        }

        let bytes = args.content.as_bytes();
        std::fs::write(&path, bytes).map_err(|e| ToolError::Execution {
            name: "write_file".into(),
            reason: format!("write failed: {e}"),
        })?;

        Ok(ToolOutput::Json(
            serde_json::json!({ "bytes_written": bytes.len() }),
        ))
    }
}

// ---------------------------------------------------------------------------
// 3. edit_file
// ---------------------------------------------------------------------------

/// Input schema for `edit_file`.
///
/// Canonical fields: `path`, `old`, `new`. Path aliases: `file`, `filename`,
/// `filepath`, `file_path`, `filePath`. Old-text aliases: `old_string`,
/// `old_str`, `search`, `find`, `from`. New-text aliases: `new_string`,
/// `new_str`, `replace`, `replacement`, `to`.
#[derive(Deserialize, JsonSchema, Debug)]
struct EditFileInput {
    /// Path to the file to edit, relative to the sandbox root.
    #[serde(
        alias = "file",
        alias = "filename",
        alias = "filepath",
        alias = "file_path",
        alias = "filePath"
    )]
    path: String,
    /// Exact text to find. Must appear exactly once in the file.
    #[serde(
        alias = "old_string",
        alias = "old_str",
        alias = "search",
        alias = "find",
        alias = "from"
    )]
    old: String,
    /// Replacement text.
    #[serde(
        alias = "new_string",
        alias = "new_str",
        alias = "replace",
        alias = "replacement",
        alias = "to"
    )]
    new: String,
}

/// Tool: replace a unique substring in a file.
pub struct EditFileTool {
    sandbox: Arc<FsSandbox>,
    schema: OnceLock<ToolSchema>,
}

impl EditFileTool {
    fn new(sandbox: Arc<FsSandbox>) -> Self {
        Self {
            sandbox,
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<EditFileInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: EditFileInput = parse_input("edit_file", input)?;
        let path = self.sandbox.resolve(args.path.trim(), "edit_file")?;

        debug!("edit_file: {:?}", path);

        if !path.exists() || !path.is_file() {
            return Err(ToolError::Execution {
                name: "edit_file".into(),
                reason: format!("file not found: {}", args.path.trim()),
            });
        }

        let contents = std::fs::read_to_string(&path).map_err(|e| ToolError::Execution {
            name: "edit_file".into(),
            reason: format!("read failed: {e}"),
        })?;

        let count = contents.matches(args.old.as_str()).count();
        if count == 0 {
            return Err(ToolError::Execution {
                name: "edit_file".into(),
                reason: "old text not found in file (0 matches); check the exact text to replace"
                    .into(),
            });
        }
        if count > 1 {
            return Err(ToolError::Execution {
                name: "edit_file".into(),
                reason: format!(
                    "old text matches {} times; must match exactly once to avoid ambiguous edits",
                    count
                ),
            });
        }

        let updated = contents.replacen(args.old.as_str(), args.new.as_str(), 1);
        std::fs::write(&path, updated.as_bytes()).map_err(|e| ToolError::Execution {
            name: "edit_file".into(),
            reason: format!("write failed: {e}"),
        })?;

        Ok(ToolOutput::Json(serde_json::json!({ "replaced": true })))
    }
}

// ---------------------------------------------------------------------------
// 4. list_files
// ---------------------------------------------------------------------------

/// Input schema for `list_files`.
///
/// Canonical field: `dir`. Accepted aliases: `directory`, `folder`,
/// `dir_path`, `dirpath`, `path` (models often say `path` for list_files).
#[derive(Deserialize, JsonSchema, Debug)]
struct ListFilesInput {
    /// Directory to list, relative to the sandbox root. Defaults to "." (the
    /// root itself).
    #[serde(
        default = "default_dot",
        alias = "directory",
        alias = "folder",
        alias = "dir_path",
        alias = "dirpath",
        alias = "path"
    )]
    dir: String,
}

fn default_dot() -> String {
    ".".into()
}

/// Tool: list entries in a directory (non-recursive, sorted by name).
pub struct ListFilesTool {
    sandbox: Arc<FsSandbox>,
    schema: OnceLock<ToolSchema>,
}

impl ListFilesTool {
    fn new(sandbox: Arc<FsSandbox>) -> Self {
        Self {
            sandbox,
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<ListFilesInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        // Leniency: bare string -> treat as the dir field.
        let input = coerce_string_to_field(input, "dir");
        // Leniency: null input -> use default.
        let input = if input.is_null() {
            serde_json::json!({ "dir": "." })
        } else {
            input
        };
        let args: ListFilesInput = parse_input("list_files", input)?;
        let dir_arg = args.dir.trim().to_string();
        let path = self.sandbox.resolve(&dir_arg, "list_files")?;

        debug!("list_files: {:?}", path);

        if !path.is_dir() {
            return Err(ToolError::Execution {
                name: "list_files".into(),
                reason: format!("not a directory: {dir_arg}"),
            });
        }

        let read_dir = std::fs::read_dir(&path).map_err(|e| ToolError::Execution {
            name: "list_files".into(),
            reason: format!("readdir failed: {e}"),
        })?;

        let mut entries: Vec<Value> = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|e| ToolError::Execution {
                name: "list_files".into(),
                reason: format!("readdir entry error: {e}"),
            })?;
            let meta = entry.metadata().map_err(|e| ToolError::Execution {
                name: "list_files".into(),
                reason: format!("metadata error: {e}"),
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            entries.push(serde_json::json!({
                "name": name,
                "is_dir": meta.is_dir(),
                "size": meta.len(),
            }));
        }

        entries.sort_by(|a, b| {
            let na = a["name"].as_str().unwrap_or("");
            let nb = b["name"].as_str().unwrap_or("");
            na.cmp(nb)
        });

        Ok(ToolOutput::Json(serde_json::json!({ "entries": entries })))
    }
}

// ---------------------------------------------------------------------------
// 5. grep
// ---------------------------------------------------------------------------

const GREP_MAX_MATCHES: usize = 200;

/// Input schema for `grep`.
///
/// Canonical fields: `pattern`, `path`. Pattern aliases: `query`, `search`,
/// `regex`, `q`, `text`. Path aliases: `file`, `filename`, `filepath`,
/// `file_path`, `filePath`.
#[derive(Deserialize, JsonSchema, Debug)]
struct GrepInput {
    /// Plain substring to search for (not a regex).
    #[serde(
        alias = "query",
        alias = "search",
        alias = "regex",
        alias = "q",
        alias = "text"
    )]
    pattern: String,
    /// File or directory to search. Defaults to ".". When a directory is
    /// given, the search recurses into subdirectories.
    #[serde(
        default = "default_dot",
        alias = "file",
        alias = "filename",
        alias = "filepath",
        alias = "file_path",
        alias = "filePath"
    )]
    path: String,
}

/// Tool: plain substring search across a file or directory tree.
pub struct GrepTool {
    sandbox: Arc<FsSandbox>,
    schema: OnceLock<ToolSchema>,
}

impl GrepTool {
    fn new(sandbox: Arc<FsSandbox>) -> Self {
        Self {
            sandbox,
            schema: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<GrepInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: GrepInput = parse_input("grep", input)?;
        let path_arg = args.path.trim().to_string();
        let pattern = args.pattern.trim().to_string();
        let resolved = self.sandbox.resolve(&path_arg, "grep")?;

        debug!("grep: pattern={:?} path={:?}", pattern, resolved);

        let mut matches: Vec<Value> = Vec::new();
        grep_path(&resolved, &resolved, &pattern, &mut matches);
        matches.truncate(GREP_MAX_MATCHES);

        Ok(ToolOutput::Json(serde_json::json!({ "matches": matches })))
    }
}

/// Recursively search `path` for lines containing `pattern`.
fn grep_path(base: &Path, path: &Path, pattern: &str, out: &mut Vec<Value>) {
    if out.len() >= GREP_MAX_MATCHES {
        return;
    }
    if path.is_file() {
        grep_file(base, path, pattern, out);
    } else if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if out.len() >= GREP_MAX_MATCHES {
                    break;
                }
                grep_path(base, &entry.path(), pattern, out);
            }
        }
    }
}

/// Search a single file for lines containing `pattern`.
fn grep_file(base: &Path, path: &Path, pattern: &str, out: &mut Vec<Value>) {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return, // skip unreadable / binary files
    };
    let display = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let display = if display.is_empty() {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        display
    };
    for (i, line) in contents.lines().enumerate() {
        if out.len() >= GREP_MAX_MATCHES {
            break;
        }
        if line.contains(pattern) {
            out.push(serde_json::json!({
                "file": display,
                "line_no": i + 1,
                "line": line,
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Plugin that registers all five filesystem tools into a host [`Registry`].
///
/// All tools share a single [`FsSandbox`] via `Arc`. The sandbox root is set
/// at construction time and cannot be changed after that.
pub struct FsToolsPlugin {
    sandbox: Arc<FsSandbox>,
}

impl FsToolsPlugin {
    /// Create the plugin with `root` as the sandbox root.
    ///
    /// Returns an error if the root directory cannot be canonicalized (i.e. it
    /// does not exist or is otherwise inaccessible). Create the directory
    /// before calling this.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        Ok(Self {
            sandbox: Arc::new(FsSandbox::new(root)?),
        })
    }
}

impl Plugin for FsToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-fs"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        let sb = self.sandbox;
        registry
            .tools
            .add_arc("read_file", Arc::new(ReadFileTool::new(sb.clone())));
        registry
            .tools
            .add_arc("write_file", Arc::new(WriteFileTool::new(sb.clone())));
        registry
            .tools
            .add_arc("edit_file", Arc::new(EditFileTool::new(sb.clone())));
        registry
            .tools
            .add_arc("list_files", Arc::new(ListFilesTool::new(sb.clone())));
        registry.tools.add_arc("grep", Arc::new(GrepTool::new(sb)));
    }
}
