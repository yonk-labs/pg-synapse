//! `search_news`: keyword news search via Google News RSS.
//!
//! There is no free, keyless, general-purpose web/news search API; Google
//! News' search RSS feed is the practical exception; a plain HTTP GET,
//! title / link / source / publish date per item, no key or approval
//! needed. It is unofficial and undocumented (Google can change or throttle
//! it without notice), so this is the right tool for "watch for news about
//! X" demo and prototype agents, not something to depend on for anything
//! business-critical.
//!
//! ## What you get, and what you don't
//!
//! Each result has a real `title`, `source`, and `published_at`, and a
//! `url`. That `url` is a `news.google.com` redirect link (it works when
//! followed, but is not the publisher's own URL). There is no article body
//! or real summary in the feed: Google News' `<description>` is just the
//! title re-wrapped in decorative HTML, not a summary, so this tool does
//! not expose it. An agent that wants a one-line summary should write one
//! from the headline; fetching and summarizing the full article is a
//! separate, heavier capability this tool does not attempt.

#![forbid(unsafe_code)]

use std::time::Duration;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// Arguments accepted by [`SearchNewsTool`].
#[derive(JsonSchema, Deserialize)]
struct SearchNewsArgs {
    /// Keyword(s) to search for, e.g. "postgresql" or "AI regulation".
    /// Matches literally: Google News does exact/near-exact keyword
    /// matching, not semantic search, so a synonym or paraphrase may not
    /// surface a relevant article.
    #[serde(alias = "q", alias = "keyword", alias = "topic", alias = "search")]
    query: String,
    /// Maximum number of results to return (1-50).
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    10
}

/// Parse a Google News RSS response body into normalized result rows.
/// A pure function so it can be unit-tested against a captured sample feed
/// with no network access.
fn parse_feed(body: &[u8], limit: usize) -> Result<Vec<Value>, ToolError> {
    let channel = rss::Channel::read_from(body).map_err(|e| ToolError::Execution {
        name: "search_news".into(),
        reason: format!("could not parse the feed: {e}"),
    })?;
    Ok(channel
        .items()
        .iter()
        .take(limit)
        .map(|item| {
            let source = item
                .source()
                .map(|s| s.title().unwrap_or_else(|| s.url()).to_string());
            json!({
                "title": item.title(),
                "url": item.link(),
                "source": source,
                "published_at": item.pub_date(),
            })
        })
        .collect())
}

/// `search_news` tool: hits Google News' search RSS feed and returns
/// normalized results.
pub struct SearchNewsTool {
    http: reqwest::Client,
}

impl SearchNewsTool {
    /// Construct with a fresh HTTP client (10s timeout).
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for SearchNewsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SearchNewsTool {
    fn name(&self) -> &str {
        "search_news"
    }
    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| ToolSchema::from_root(schemars::schema_for!(SearchNewsArgs)))
    }
    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: SearchNewsArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "search_news".into(),
                reason: e.to_string(),
            })?;
        let limit = args.limit.clamp(1, 50) as usize;

        let mut url = reqwest::Url::parse("https://news.google.com/rss/search").map_err(|e| {
            ToolError::Execution {
                name: "search_news".into(),
                reason: e.to_string(),
            }
        })?;
        url.query_pairs_mut()
            .append_pair("q", &args.query)
            .append_pair("hl", "en-US")
            .append_pair("gl", "US")
            .append_pair("ceid", "US:en");

        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::Execution {
                name: "search_news".into(),
                reason: format!("request failed: {e}"),
            })?;
        let body = resp.bytes().await.map_err(|e| ToolError::Execution {
            name: "search_news".into(),
            reason: format!("could not read response: {e}"),
        })?;

        let results = parse_feed(&body, limit)?;
        Ok(ToolOutput::Json(Value::Array(results)))
    }
}

/// Registers `search_news`.
pub struct NewsSearchToolsPlugin;

impl NewsSearchToolsPlugin {
    /// Construct the plugin.
    pub fn new() -> Self {
        Self
    }
}

impl Default for NewsSearchToolsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for NewsSearchToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-newssearch"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn register(self, registry: &mut Registry) {
        registry
            .tools
            .add_arc("search_news", std::sync::Arc::new(SearchNewsTool::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real response captured from the live feed (query "postgresql"),
    /// trimmed to two items, used to test parsing without network access.
    const SAMPLE_FEED: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel><generator>NFE/5.0</generator><title>"postgresql" - Google News</title><link>https://news.google.com/search?q=postgresql&amp;hl=en-US&amp;gl=US&amp;ceid=US:en</link><language>en-US</language><lastBuildDate>Thu, 27 Aug 2026 12:14:32 GMT</lastBuildDate><description>Google News</description><item><title>Postgres pioneer credits Oracle with helping his database take over the world - The Register</title><link>https://news.google.com/rss/articles/CBMizwFAAAA?oc=5</link><guid isPermaLink="false">CBMizwFAAAA</guid><pubDate>Wed, 19 Aug 2026 12:37:00 GMT</pubDate><description>&lt;a href="https://news.google.com/rss/articles/CBMizwFAAAA?oc=5" target="_blank"&gt;Postgres pioneer credits Oracle with helping his database take over the world&lt;/a&gt;&amp;nbsp;&amp;nbsp;&lt;font color="#6f6f6f"&gt;The Register&lt;/font&gt;</description><source url="https://www.theregister.com">The Register</source></item><item><title>Migrate multilingual full-text search from SQL Server to PostgreSQL - AWS</title><link>https://news.google.com/rss/articles/CBMiqgFBBBB?oc=5</link><guid isPermaLink="false">CBMiqgFBBBB</guid><pubDate>Wed, 19 Aug 2026 10:00:00 GMT</pubDate><description>&lt;a href="https://news.google.com/rss/articles/CBMiqgFBBBB?oc=5" target="_blank"&gt;Migrate multilingual full-text search from SQL Server to PostgreSQL&lt;/a&gt;&amp;nbsp;&amp;nbsp;&lt;font color="#6f6f6f"&gt;Amazon Web Services (AWS)&lt;/font&gt;</description><source url="https://aws.amazon.com">Amazon Web Services (AWS)</source></item></channel></rss>"##;

    #[test]
    fn parse_feed_extracts_title_url_source_and_date() {
        let results = parse_feed(SAMPLE_FEED.as_bytes(), 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0]["title"],
            "Postgres pioneer credits Oracle with helping his database take over the world - The Register"
        );
        assert_eq!(results[0]["source"], "The Register");
        assert_eq!(results[0]["published_at"], "Wed, 19 Aug 2026 12:37:00 GMT");
        assert!(
            results[0]["url"]
                .as_str()
                .unwrap()
                .starts_with("https://news.google.com/rss/articles/")
        );
        assert_eq!(results[1]["source"], "Amazon Web Services (AWS)");
    }

    #[test]
    fn parse_feed_respects_limit() {
        let results = parse_feed(SAMPLE_FEED.as_bytes(), 1).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn parse_feed_rejects_garbage() {
        let err = parse_feed(b"not xml at all", 10).unwrap_err();
        match err {
            ToolError::Execution { name, .. } => assert_eq!(name, "search_news"),
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    // Live check against the real feed. Mirrors the workspace's convention
    // of gating network-dependent tests so `cargo test` never requires
    // internet: skips cleanly unless explicitly requested.
    #[tokio::test]
    async fn live_search_returns_real_results() {
        if std::env::var("PG_SYNAPSE_LIVE_NEWS_TEST").is_err() {
            eprintln!("skipping: set PG_SYNAPSE_LIVE_NEWS_TEST=1 to hit the real feed");
            return;
        }
        let tool = SearchNewsTool::new();
        let out = tool
            .run(
                json!({"query": "postgresql", "limit": 3}),
                &ToolCtx::default(),
            )
            .await
            .unwrap();
        let ToolOutput::Json(Value::Array(rows)) = out else {
            panic!("expected a JSON array");
        };
        assert!(!rows.is_empty());
        assert!(rows[0]["title"].as_str().unwrap_or_default().len() > 3);
    }
}
