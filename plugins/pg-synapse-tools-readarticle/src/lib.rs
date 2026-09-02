//! `read_article`: fetch a URL and extract its main article text.
//!
//! Given a direct article URL, this tool fetches the page and runs a Rust
//! port of Mozilla's Readability algorithm (the `readability` crate, same
//! idea as Firefox Reader Mode) to strip nav/ads/boilerplate and return the
//! article's own title and body text. Pair it with `lede_compress`
//! (pg-synapse-tools-lede, when enabled) to go from "here is an article" to
//! "here is what it says", without spending LLM context tokens on raw HTML.
//!
//! It does NOT close the gap left by `search_news` (pg-synapse-tools-
//! newssearch) returning no article body: see below, that combination does
//! not work.
//!
//! Readability-style extraction is a heuristic (candidate-scoring over the
//! DOM), not a guarantee: it can occasionally pick the wrong block on an
//! unusual page layout, and it does not run JavaScript, so content that only
//! appears after client-side rendering will not be extracted.
//!
//! ## Google News links specifically do not work
//!
//! `search_news` returns `news.google.com/rss/articles/...` links, not the
//! publisher's own URL. Those links resolve to a client-rendered Google News
//! page (an Angular app that fetches the real article via JS after load);
//! a plain HTTP GET only ever sees that shell, never the article. This was
//! confirmed live: fetching one returns HTTP 200 with a Google News page
//! titled "Google News" and no article text. Rather than silently return
//! that shell as if it were the article, [`ReadArticleTool`] detects the
//! `news.google.com` host up front and fails with a clear message telling
//! the caller why, since decoding Google's redirect token would mean
//! reverse-engineering an undocumented, unstable internal API.

#![forbid(unsafe_code)]

use std::io::Cursor;
use std::time::Duration;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

const TOOL_NAME: &str = "read_article";

/// Arguments accepted by [`ReadArticleTool`].
#[derive(JsonSchema, Deserialize)]
struct ReadArticleArgs {
    /// The article URL to fetch and extract, e.g. one returned by
    /// `search_news`.
    #[serde(alias = "link")]
    url: String,
    /// Maximum characters of extracted body text to return (500-100000).
    /// Long articles are truncated rather than spending the caller's whole
    /// context budget on one page; a truncated result is a common,
    /// expected outcome, not an error.
    #[serde(default = "default_max_chars")]
    max_chars: usize,
}

fn default_max_chars() -> usize {
    20_000
}

/// Extract title/body text from a fetched page. A pure function so it can
/// be unit-tested against a captured HTML sample with no network access.
fn extract_article(body: &[u8], url: &url::Url, max_chars: usize) -> Result<Value, ToolError> {
    let mut cursor = Cursor::new(body);
    // Product.title comes from the page's <head><title>, not the DOM node
    // Readability picked as the article body, so it is only as good as the
    // page's own <title>; most publishers keep the two in sync.
    let product =
        readability::extractor::extract(&mut cursor, url).map_err(|e| ToolError::Execution {
            name: TOOL_NAME.into(),
            reason: format!("could not extract article content: {e}"),
        })?;

    let text = product.text.trim();
    let truncated = text.chars().count() > max_chars;
    let text: String = text.chars().take(max_chars).collect();

    Ok(json!({
        "title": product.title,
        "text": text,
        "truncated": truncated,
    }))
}

/// `read_article` tool: fetches a URL and returns its extracted article
/// title and body text.
pub struct ReadArticleTool {
    http: reqwest::Client,
}

impl ReadArticleTool {
    /// Construct with a fresh HTTP client (15s timeout: article pages are
    /// often heavier than an RSS feed).
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                // Many publishers 403 requests with no (or an obviously
                // non-browser) User-Agent as basic bot protection; a
                // realistic desktop UA is required for this tool to work
                // against real news sites, not optional hardening.
                .user_agent(
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                     Chrome/124.0.0.0 Safari/537.36",
                )
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for ReadArticleTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadArticleTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| ToolSchema::from_root(schemars::schema_for!(ReadArticleArgs)))
    }
    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: ReadArticleArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: TOOL_NAME.into(),
                reason: e.to_string(),
            })?;
        let max_chars = args.max_chars.clamp(500, 100_000);

        let url = url::Url::parse(&args.url).map_err(|e| ToolError::InvalidInput {
            name: TOOL_NAME.into(),
            reason: format!("not a valid URL: {e}"),
        })?;
        if url.host_str() == Some("news.google.com") {
            return Err(ToolError::InvalidInput {
                name: TOOL_NAME.into(),
                reason: "news.google.com links (e.g. from search_news) redirect to the article \
                         via JavaScript, which this tool cannot execute; use the article's own \
                         publisher URL instead, not the news.google.com one"
                    .into(),
            });
        }

        let resp = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|e| ToolError::Execution {
                name: TOOL_NAME.into(),
                reason: format!("request failed: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(ToolError::Execution {
                name: TOOL_NAME.into(),
                reason: format!("server returned {}", resp.status()),
            });
        }
        let body = resp.bytes().await.map_err(|e| ToolError::Execution {
            name: TOOL_NAME.into(),
            reason: format!("could not read response: {e}"),
        })?;

        let result = extract_article(&body, &url, max_chars)?;
        Ok(ToolOutput::Json(result))
    }
}

/// Registers `read_article`.
pub struct ReadArticleToolsPlugin;

impl ReadArticleToolsPlugin {
    /// Construct the plugin.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadArticleToolsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ReadArticleToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-readarticle"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn register(self, registry: &mut Registry) {
        registry
            .tools
            .add_arc(TOOL_NAME, std::sync::Arc::new(ReadArticleTool::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small hand-authored page with the clutter Readability-style
    /// extraction is meant to strip (nav, a sidebar ad block) around one
    /// real article body, used to test extraction without network access.
    const SAMPLE_PAGE: &str = r#"<!DOCTYPE html>
<html>
<head><title>Postgres Adoption Keeps Climbing - Example News Site</title></head>
<body>
<nav><a href="/">Home</a> <a href="/world">World</a> <a href="/tech">Tech</a></nav>
<aside class="sidebar-ad">Subscribe now! Limited offer!</aside>
<article>
<h1>Postgres Adoption Keeps Climbing</h1>
<p>Postgres has become the default choice for a growing share of new
application backends, according to a survey of database administrators
released this week.</p>
<p>Respondents cited extensibility, a permissive license, and a large
plugin ecosystem as the top reasons for choosing Postgres over commercial
alternatives. The survey polled over two thousand engineers across
industries including finance, retail, and healthcare.</p>
<p>The report also found that teams running Postgres in production for
more than three years were markedly less likely to consider migrating away,
citing operational familiarity as a major retention factor.</p>
</article>
<footer>Copyright Example News Site. All rights reserved.</footer>
</body>
</html>"#;

    fn sample_url() -> url::Url {
        url::Url::parse("https://example.com/news/postgres-adoption").unwrap()
    }

    #[test]
    fn extract_article_pulls_title_and_body_text() {
        let result = extract_article(SAMPLE_PAGE.as_bytes(), &sample_url(), 20_000).unwrap();
        assert_eq!(
            result["title"],
            "Postgres Adoption Keeps Climbing - Example News Site"
        );
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("growing share of new"));
        assert!(!text.contains("Subscribe now"));
        assert!(!text.contains("Copyright Example News Site"));
        assert_eq!(result["truncated"], false);
    }

    #[test]
    fn extract_article_truncates_and_flags_it() {
        let result = extract_article(SAMPLE_PAGE.as_bytes(), &sample_url(), 50).unwrap();
        let text = result["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), 50);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn run_rejects_news_google_com_links_before_fetching() {
        let tool = ReadArticleTool::new();
        let err = tool
            .run(
                json!({"url": "https://news.google.com/rss/articles/CBMi123"}),
                &ToolCtx::default(),
            )
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput { name, reason } => {
                assert_eq!(name, TOOL_NAME);
                assert!(reason.contains("JavaScript"));
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn extract_article_on_empty_input_returns_empty_not_error() {
        // html5ever parses even empty/garbage bytes into a (trivial) DOM, so
        // extraction never fails outright; on a page with no real content
        // it degrades to an empty Product rather than an error. Callers
        // should treat an empty `text` as "nothing extracted", not rely on
        // this tool to detect that case for them.
        let result = extract_article(b"", &sample_url(), 20_000).unwrap();
        assert_eq!(result["text"], "");
    }

    // Live check against a real page. Mirrors the workspace's convention of
    // gating network-dependent tests so `cargo test` never requires
    // internet: skips cleanly unless explicitly requested.
    #[tokio::test]
    async fn live_read_returns_real_article() {
        if std::env::var("PG_SYNAPSE_LIVE_READARTICLE_TEST").is_err() {
            eprintln!("skipping: set PG_SYNAPSE_LIVE_READARTICLE_TEST=1 to hit a real page");
            return;
        }
        let tool = ReadArticleTool::new();
        let out = tool
            .run(
                json!({"url": "https://www.rust-lang.org/"}),
                &ToolCtx::default(),
            )
            .await
            .unwrap();
        let ToolOutput::Json(value) = out else {
            panic!("expected a JSON object");
        };
        assert!(value["text"].as_str().unwrap_or_default().len() > 10);
    }
}
