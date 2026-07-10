//! HTTP tool plugin for pg_synapse: `http_get`, `http_post`, `http_head`.
//!
//! All three tools are derived via `#[derive(pg_synapse_macros::Tool)]` and
//! share a single lazily-initialized `reqwest::Client` with a 30 s default
//! timeout. The `HttpToolsPlugin` newtype implements `pg_synapse_core::Plugin`
//! and registers all three tools into a host `Registry`.
//!
//! ## Output shape
//!
//! * `http_get` / `http_post`: `{ "status": <u16>, "body": <string> }`
//! * `http_head`: `{ "status": <u16>, "headers": { ... } }`
//!
//! Network or transport failures surface as `ToolError::Execution { name,
//! reason }` with `reason` taken from the underlying `reqwest::Error`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_macros::Tool as DeriveTool;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Shared HTTP client used by all three tools.
///
/// Lazily built on first access. A 30 second default timeout is applied so
/// runaway tool calls cannot pin an executor's tokio task indefinitely.
fn http() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builds with default config")
    })
}

// ---------------------------------------------------------------------------
// SSRF guard
// ---------------------------------------------------------------------------
//
// Tool calls are driven by (untrusted) LLM output, and this code runs inside a
// Postgres backend. Without a guard, an agent could reach the cloud metadata
// endpoint (169.254.169.254), loopback services, or RFC1918 hosts. We
// default-deny those ranges and let an operator allowlist specific hosts.

use std::net::IpAddr;

/// True for IPs an agent must not be allowed to reach by default: loopback,
/// private/RFC1918, link-local (incl. the cloud metadata endpoint),
/// unspecified, and their IPv6 equivalents (incl. IPv4-mapped forms).
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Operator allowlist of host strings that bypass [`is_blocked_ip`]. Seeded
/// once from the `PG_SYNAPSE_HTTP_ALLOW` env var (comma-separated hosts) and
/// extendable in-process via [`allow_host`] (used by tests and hosts that
/// cannot set env vars safely). Reading env is safe; `set_var` is not on
/// edition 2024, so the in-memory set is the writable surface.
fn allowlist() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static ALLOW: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    ALLOW.get_or_init(|| {
        let mut set = std::collections::HashSet::new();
        if let Ok(v) = std::env::var("PG_SYNAPSE_HTTP_ALLOW") {
            for h in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                set.insert(h.to_owned());
            }
        }
        std::sync::Mutex::new(set)
    })
}

/// Add a host (DNS name or IP literal, matched against the URL's host verbatim)
/// to the SSRF allowlist for this process.
pub fn allow_host(host: impl Into<String>) {
    allowlist().lock().unwrap().insert(host.into());
}

fn host_allowlisted(host: &str) -> bool {
    allowlist().lock().unwrap().contains(host)
}

/// Reject a URL that targets a blocked address before any request is sent.
///
/// Enforces http/https only, honors the operator allowlist, blocks IP literals
/// in blocked ranges, and resolves DNS names to reject hostnames that point at
/// internal addresses. `tool` is the tool name for the returned error.
///
/// ponytail: DNS is resolved once here and again by reqwest at send time, so a
/// rebinding attacker has a narrow TOCTOU window. Closing it fully needs a
/// custom connector that checks the socket address; not worth it until asked.
async fn guard_url(url: &str, tool: &str) -> Result<(), ToolError> {
    let blocked = |reason: String| ToolError::Execution {
        name: tool.into(),
        reason,
    };
    let parsed = reqwest::Url::parse(url).map_err(|e| blocked(format!("invalid url: {e}")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(blocked(format!("scheme '{other}' not allowed"))),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| blocked("url has no host".into()))?;
    if host_allowlisted(host) {
        return Ok(());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(blocked(format!(
                "request to internal address {ip} is blocked"
            )));
        }
        return Ok(());
    }
    // DNS name: resolve and reject if any resolved address is internal.
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| blocked(format!("dns resolution failed: {e}")))?;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(blocked(format!(
                "host '{host}' resolves to internal address {}; blocked",
                addr.ip()
            )));
        }
    }
    Ok(())
}

/// Fetch a URL via HTTP GET. Returns the response body as text.
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_get",
    description = "Fetch a URL via HTTP GET. Returns the response status and body as text."
)]
pub struct HttpGet {
    /// Absolute URL to fetch.
    pub url: String,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpGet {
    /// User-facing handler. The derive macro wraps this with input parsing.
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        guard_url(&self.url, Self::TOOL_NAME).await?;
        let mut req = http().get(&self.url);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "body": body }),
        ))
    }
}

/// POST a JSON body to a URL. Returns the response status and body as text.
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_post",
    description = "POST a JSON body to a URL. Returns the response status and body as text."
)]
pub struct HttpPost {
    /// Absolute URL to post to.
    pub url: String,
    /// Request body, serialized as JSON. Defaults to `null` when omitted.
    #[serde(default)]
    pub body: Value,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpPost {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        guard_url(&self.url, Self::TOOL_NAME).await?;
        let mut req = http().post(&self.url).json(&self.body);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "body": body }),
        ))
    }
}

/// HEAD request: returns the response status and headers (no body).
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_head",
    description = "HEAD request: returns the response status and headers (no body)."
)]
pub struct HttpHead {
    /// Absolute URL to inspect.
    pub url: String,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpHead {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        guard_url(&self.url, Self::TOOL_NAME).await?;
        let mut req = http().head(&self.url);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let headers: BTreeMap<String, String> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
            .collect();
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "headers": headers }),
        ))
    }
}

/// Plugin that registers `http_get`, `http_post`, and `http_head` against a
/// host's [`Registry`].
///
/// Each tool is registered as a single shared `Arc<dyn Tool>`. The fields on
/// the template instances are inert: `Tool::run` re-parses input from JSON on
/// every call, so the template values are never read.
#[derive(Default, Debug)]
pub struct HttpToolsPlugin;

impl Plugin for HttpToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-http"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        let getter = HttpGet {
            url: String::new(),
            headers: BTreeMap::new(),
        };
        let poster = HttpPost {
            url: String::new(),
            body: Value::Null,
            headers: BTreeMap::new(),
        };
        let header = HttpHead {
            url: String::new(),
            headers: BTreeMap::new(),
        };
        registry
            .tools
            .add_arc(HttpGet::TOOL_NAME.to_string(), Arc::new(getter));
        registry
            .tools
            .add_arc(HttpPost::TOOL_NAME.to_string(), Arc::new(poster));
        registry
            .tools
            .add_arc(HttpHead::TOOL_NAME.to_string(), Arc::new(header));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn blocks_loopback_private_and_link_local_ips() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata endpoint
            "0.0.0.0",
            "::1",
            "fc00::1",          // unique-local IPv6
            "fe80::1",          // link-local IPv6
            "::ffff:127.0.0.1", // IPv4-mapped loopback
        ] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(is_blocked_ip(parsed), "{ip} should be blocked");
        }
    }

    #[test]
    fn allows_public_ips() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(!is_blocked_ip(parsed), "{ip} should be allowed");
        }
    }

    #[tokio::test]
    async fn http_get_blocks_metadata_endpoint() {
        // Never allowlisted by any test; the guard must reject before sending.
        let tool = HttpGet {
            url: String::new(),
            headers: BTreeMap::new(),
        };
        let err = pg_synapse_core::Tool::run(
            &tool,
            serde_json::json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
        match err {
            ToolError::Execution { name, reason } => {
                assert_eq!(name, "http_get");
                assert!(reason.contains("blocked"), "reason: {reason}");
            }
            other => panic!("expected Execution error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allowlisted_host_bypasses_the_block() {
        // Loopback is blocked by default; allowlisting it lets the request
        // through the guard. Bind then drop a listener to get a closed port so
        // the connection is refused immediately (proving the guard did not
        // reject: the error is a connect failure, not "blocked").
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        allow_host("127.0.0.1");
        let tool = HttpGet {
            url: String::new(),
            headers: BTreeMap::new(),
        };
        let err = pg_synapse_core::Tool::run(
            &tool,
            serde_json::json!({ "url": format!("http://127.0.0.1:{port}/x") }),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
        match err {
            ToolError::Execution { reason, .. } => {
                assert!(
                    !reason.contains("blocked"),
                    "should not be blocked: {reason}"
                );
            }
            other => panic!("expected Execution error, got {other:?}"),
        }
    }

    #[test]
    fn plugin_register_inserts_three_tools() {
        let mut reg = Registry::new();
        HttpToolsPlugin.register(&mut reg);
        let mut names = reg.tools.names();
        names.sort();
        assert_eq!(names, vec!["http_get", "http_head", "http_post"]);
    }

    #[test]
    fn plugin_metadata_present() {
        let p = HttpToolsPlugin;
        assert_eq!(p.name(), "pg-synapse-tools-http");
        assert!(!p.version().is_empty());
    }
}
