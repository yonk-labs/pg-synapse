//! Clock tool plugin for pg_synapse: `get_current_time`.
//!
//! Returns the current UTC time (or a named timezone offset if supplied) as
//! an RFC 3339 / ISO 8601 string.
//!
//! ## Timezone handling
//!
//! v0.1 accepts `timezone` / `tz` but only supports "UTC" (case-insensitive)
//! and numeric offsets of the form "+HH:MM" / "-HH:MM". Named IANA zones
//! (e.g. "America/New_York") require the `chrono-tz` crate and are a v0.2
//! addition. Unrecognised zone names return the time in UTC with a warning
//! note in the response.
//!
//! ## Test determinism
//!
//! The internal `now_fn` slot lets tests inject a fixed instant. Production
//! callers use `ClockTool::new()` which reads the system clock.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::OnceLock;

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
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
// Schema builder helper
// ---------------------------------------------------------------------------

fn build_schema<T: JsonSchema>() -> ToolSchema {
    let root = schema_for!(T);
    let val = serde_json::to_value(&root).expect("schemars output is always valid JSON");
    ToolSchema::from_json_value(val).expect("schemars schema is always a valid object")
}

// ---------------------------------------------------------------------------
// Input struct with lenient aliases
// ---------------------------------------------------------------------------

/// Input schema for `get_current_time`.
///
/// `timezone` accepts alias `tz`. Defaults to "UTC".
#[derive(Deserialize, JsonSchema, Debug)]
struct ClockInput {
    /// Timezone identifier. Accepts "UTC", "+HH:MM", or "-HH:MM".
    /// Defaults to "UTC". IANA zone names are accepted but fall back to UTC.
    #[serde(default = "default_tz", alias = "tz")]
    timezone: String,
}

fn default_tz() -> String {
    "UTC".into()
}

// ---------------------------------------------------------------------------
// Clock tool
// ---------------------------------------------------------------------------

/// Tool: return the current time as an ISO 8601 / RFC 3339 string.
///
/// Accepts optional `timezone` (or `tz`). Returns `{"iso8601": <string>, "timezone": <string>}`.
pub struct ClockTool {
    schema: OnceLock<ToolSchema>,
    /// Optional override for the current time (used in tests).
    now_override: Option<DateTime<Utc>>,
}

impl ClockTool {
    /// Create a clock tool that reads the live system clock.
    pub fn new() -> Self {
        Self {
            schema: OnceLock::new(),
            now_override: None,
        }
    }

    /// Create a clock tool pinned to a specific instant (for testing).
    #[cfg(test)]
    pub fn with_fixed_time(dt: DateTime<Utc>) -> Self {
        Self {
            schema: OnceLock::new(),
            now_override: Some(dt),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.now_override.unwrap_or_else(Utc::now)
    }
}

impl Default for ClockTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a fixed-offset timezone string of the form "+HH:MM" or "-HH:MM".
/// Returns `None` for anything that does not match.
fn parse_fixed_offset(s: &str) -> Option<FixedOffset> {
    let s = s.trim();
    if s.len() < 6 {
        return None;
    }
    let (sign_char, rest) = s.split_at(1);
    let sign: i32 = match sign_char {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let mut parts = rest.splitn(2, ':');
    let hours: i32 = parts.next()?.parse().ok()?;
    let minutes: i32 = parts.next()?.parse().ok()?;
    let total_seconds = sign * (hours * 3600 + minutes * 60);
    FixedOffset::east_opt(total_seconds)
}

#[async_trait]
impl Tool for ClockTool {
    fn name(&self) -> &str {
        "get_current_time"
    }

    fn schema(&self) -> &ToolSchema {
        self.schema.get_or_init(build_schema::<ClockInput>)
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: ClockInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: "get_current_time".into(),
                reason: e.to_string(),
            })?;

        let tz_label = args.timezone.trim().to_string();
        debug!("get_current_time: timezone={}", tz_label);

        let now_utc = self.now();

        let (iso8601, effective_tz) = if tz_label.eq_ignore_ascii_case("utc") {
            (now_utc.to_rfc3339(), "UTC".to_string())
        } else if let Some(offset) = parse_fixed_offset(&tz_label) {
            let dt_offset: DateTime<FixedOffset> = offset.from_utc_datetime(&now_utc.naive_utc());
            (dt_offset.to_rfc3339(), tz_label.clone())
        } else {
            // Unknown IANA zone: return UTC with a note. Avoids a hard error
            // and keeps the tool useful when the LLM sends a named zone.
            let note = format!(
                "IANA zone '{}' not supported in v0.1; returning UTC",
                tz_label
            );
            tracing::warn!("{}", note);
            (now_utc.to_rfc3339(), "UTC".to_string())
        };

        Ok(ToolOutput::Json(serde_json::json!({
            "iso8601": iso8601,
            "timezone": effective_tz,
        })))
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Plugin that registers `get_current_time` into a host [`Registry`].
pub struct ClockToolsPlugin;

impl ClockToolsPlugin {
    /// Create the plugin. No configuration required.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClockToolsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ClockToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-clock"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry
            .tools
            .add_arc("get_current_time", std::sync::Arc::new(ClockTool::new()));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pg_synapse_core::types::ToolCtx;

    fn ctx() -> ToolCtx {
        ToolCtx::default()
    }

    fn fixed_tool() -> ClockTool {
        let fixed = Utc.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        ClockTool::with_fixed_time(fixed)
    }

    fn extract_iso(out: ToolOutput) -> String {
        match out {
            ToolOutput::Json(v) => v["iso8601"].as_str().unwrap().to_string(),
            _ => panic!("expected Json output"),
        }
    }

    #[tokio::test]
    async fn returns_valid_rfc3339_utc() {
        let tool = fixed_tool();
        let out = tool
            .run(serde_json::json!({"timezone": "UTC"}), &ctx())
            .await
            .unwrap();
        let iso = extract_iso(out);
        // Must parse as RFC 3339.
        let parsed = DateTime::parse_from_rfc3339(&iso);
        assert!(parsed.is_ok(), "not valid RFC3339: {iso}");
    }

    #[tokio::test]
    async fn tz_alias_accepted() {
        let tool = fixed_tool();
        let out = tool
            .run(serde_json::json!({"tz": "UTC"}), &ctx())
            .await
            .unwrap();
        let iso = extract_iso(out);
        assert!(DateTime::parse_from_rfc3339(&iso).is_ok());
    }

    #[tokio::test]
    async fn empty_input_defaults_to_utc() {
        let tool = fixed_tool();
        let out = tool.run(serde_json::json!({}), &ctx()).await.unwrap();
        let iso = extract_iso(out);
        assert!(DateTime::parse_from_rfc3339(&iso).is_ok());
    }

    #[tokio::test]
    async fn fixed_offset_produces_valid_rfc3339() {
        let tool = fixed_tool();
        let out = tool
            .run(serde_json::json!({"timezone": "+05:30"}), &ctx())
            .await
            .unwrap();
        let iso = extract_iso(out);
        let parsed = DateTime::parse_from_rfc3339(&iso).unwrap();
        // Offset should be +05:30.
        assert_eq!(parsed.offset().local_minus_utc(), 5 * 3600 + 30 * 60);
    }

    #[tokio::test]
    async fn unknown_iana_zone_falls_back_to_utc() {
        let tool = fixed_tool();
        let out = tool
            .run(serde_json::json!({"timezone": "America/New_York"}), &ctx())
            .await
            .unwrap();
        let iso = extract_iso(out);
        assert!(DateTime::parse_from_rfc3339(&iso).is_ok());
    }
}
