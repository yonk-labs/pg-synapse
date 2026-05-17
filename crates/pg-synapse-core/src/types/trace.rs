use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceLevel {
    Off = 0,
    Error = 1,
    Info = 2,
    Debug = 3,
    Full = 4,
}

impl Default for TraceLevel {
    fn default() -> Self {
        Self::Info
    }
}

impl TraceLevel {
    pub fn should_persist_messages(&self, run_succeeded: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Error => !run_succeeded,
            _ => true,
        }
    }

    pub fn should_persist_events(&self) -> bool {
        matches!(self, Self::Debug | Self::Full)
    }

    pub fn should_persist_raw_payloads(&self) -> bool {
        matches!(self, Self::Full)
    }
}

impl std::str::FromStr for TraceLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "full" => Ok(Self::Full),
            _ => Err(format!("unknown trace level: {s}")),
        }
    }
}

impl std::fmt::Display for TraceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Full => "full",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    LlmRequest,
    LlmResponse,
    ToolStart,
    ToolEnd,
    ToolError,
    RetryAttempt,
    CostCapCheck,
    IterationCapCheck,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_level_ord_matches_verbosity() {
        assert!(TraceLevel::Off < TraceLevel::Error);
        assert!(TraceLevel::Error < TraceLevel::Info);
        assert!(TraceLevel::Info < TraceLevel::Debug);
        assert!(TraceLevel::Debug < TraceLevel::Full);
    }

    #[test]
    fn persistence_decisions() {
        assert!(!TraceLevel::Off.should_persist_messages(true));
        assert!(!TraceLevel::Off.should_persist_messages(false));
        assert!(!TraceLevel::Error.should_persist_messages(true));
        assert!(TraceLevel::Error.should_persist_messages(false));
        assert!(TraceLevel::Info.should_persist_messages(true));
        assert!(TraceLevel::Info.should_persist_messages(false));
        assert!(!TraceLevel::Info.should_persist_events());
        assert!(TraceLevel::Debug.should_persist_events());
        assert!(!TraceLevel::Debug.should_persist_raw_payloads());
        assert!(TraceLevel::Full.should_persist_raw_payloads());
    }

    #[test]
    fn parse_roundtrip() {
        for level in [
            TraceLevel::Off,
            TraceLevel::Error,
            TraceLevel::Info,
            TraceLevel::Debug,
            TraceLevel::Full,
        ] {
            let s = level.to_string();
            let parsed: TraceLevel = s.parse().unwrap();
            assert_eq!(parsed, level);
        }
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!("DEBUG".parse::<TraceLevel>().unwrap(), TraceLevel::Debug);
        assert_eq!("Full".parse::<TraceLevel>().unwrap(), TraceLevel::Full);
    }

    #[test]
    fn parse_unknown_errors() {
        assert!("verbose".parse::<TraceLevel>().is_err());
    }
}
