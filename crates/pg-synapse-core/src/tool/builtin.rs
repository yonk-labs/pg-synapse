//! Kernel-provided tools that exist because of a harness component rather
//! than a plugin. Today: [`LoadSkillTool`], which serves the body of an
//! on-demand skill (see [`crate::types::Harness`]).

use async_trait::async_trait;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::Tool;
use crate::types::{Skill, ToolCtx, ToolOutput, ToolSchema};

/// Name under which the skill loader is registered.
pub const LOAD_SKILL_TOOL: &str = "load_skill";

/// Returns a skill's body when the model asks for it by name.
///
/// Constructed per execution by the runtime with the agent's skills
/// captured, and registered only when the agent has at least one skill, so
/// agents without skills see no new tool.
#[derive(Debug, Clone)]
pub struct LoadSkillTool {
    skills: Vec<Skill>,
    schema: ToolSchema,
}

impl LoadSkillTool {
    /// Build the tool over this agent's skills.
    pub fn new(skills: Vec<Skill>) -> Self {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        let schema = ToolSchema::from_json_value(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill to load.",
                    "enum": names,
                }
            },
            "required": ["name"],
            "additionalProperties": false
        }))
        .expect("static load_skill schema is valid JSON Schema");
        Self { skills, schema }
    }

    fn available(&self) -> String {
        self.skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        LOAD_SKILL_TOOL
    }

    fn schema(&self) -> &ToolSchema {
        &self.schema
    }

    async fn run(&self, input: serde_json::Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidInput {
                name: LOAD_SKILL_TOOL.into(),
                reason: format!(
                    "missing field `name`; available skills: {}",
                    self.available()
                ),
            })?;
        match self.skills.iter().find(|s| s.name == name) {
            Some(s) => Ok(ToolOutput::text(format!(
                "# Skill: {}\n\n{}",
                s.name,
                s.body.trim()
            ))),
            None => Err(ToolError::InvalidInput {
                name: LOAD_SKILL_TOOL.into(),
                reason: format!(
                    "no skill named '{name}'; available skills: {}",
                    self.available()
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ctx() -> ToolCtx {
        ToolCtx {
            execution_id: Uuid::nil(),
            caller_role: None,
            agent_name: Some("a".into()),
        }
    }

    fn tool() -> LoadSkillTool {
        LoadSkillTool::new(vec![
            Skill {
                name: "escalation".into(),
                description: "d".into(),
                body: "  step one\nstep two  ".into(),
            },
            Skill {
                name: "refunds".into(),
                description: "d".into(),
                body: "refund body".into(),
            },
        ])
    }

    #[tokio::test]
    async fn loads_named_skill_body() {
        let out = tool()
            .run(json!({"name": "escalation"}), &ctx())
            .await
            .unwrap();
        match out {
            ToolOutput::Text(t) => {
                assert!(t.starts_with("# Skill: escalation"));
                assert!(t.contains("step one\nstep two"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_skill_lists_available() {
        let err = tool()
            .run(json!({"name": "nope"}), &ctx())
            .await
            .unwrap_err();
        let s = err.to_string();
        assert!(s.contains("no skill named 'nope'"), "{s}");
        assert!(s.contains("escalation, refunds"), "{s}");
    }

    #[tokio::test]
    async fn missing_name_is_invalid_input() {
        let err = tool().run(json!({}), &ctx()).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }

    #[test]
    fn schema_enumerates_skill_names() {
        let s = serde_json::to_value(&tool().schema().0).unwrap();
        let en = s["properties"]["name"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), 2);
    }
}
