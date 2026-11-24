//! Canonical action-based wrapper for run/test/verifier tools.
//!
//! The model sees one tool: `Run` with an `action` parameter
//! (tests | verifiers). The per-action legacy execution aliases were removed
//! in v0.9.3.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use super::test_runner::RunTestsTool;
use super::verifier::RunVerifiersTool;
use codewhale_tools::required_action;

pub struct RunTool {
    name: &'static str,
    forced_action: Option<&'static str>,
}

impl RunTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
        }
    }

    const ACTIONS: &'static [&'static str] = &["tests", "verifiers"];
    const TESTS_ALLOWED_KEYS: &'static [&'static str] =
        &["args", "all_features", "cwd", "background", "timeout_ms"];
    const VERIFIERS_ALLOWED_KEYS: &'static [&'static str] = &[
        "profile",
        "level",
        "max_python_files",
        "commands",
        "background",
        "cwd",
        "timeout_ms",
    ];

    fn validate_action_keys(
        &self,
        action: &str,
        input: &Value,
        allowed: &[&str],
    ) -> Result<(), ToolError> {
        let Some(obj) = input.as_object() else {
            return Err(ToolError::invalid_input("Run tool input must be an object"));
        };
        let mut unexpected: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|key| !allowed.contains(key))
            .collect();
        if unexpected.is_empty() {
            return Ok(());
        }
        unexpected.sort_unstable();
        Err(ToolError::invalid_input(format!(
            "Unexpected parameter(s) for {} action \"{action}\": {}; nothing was run. Allowed keys for action \"{action}\": {}.",
            self.name,
            unexpected.join(", "),
            allowed.join(", ")
        )))
    }

    /// Policy-side resolution: approval and parallel-safety predicates cannot
    /// fail, so a missing action resolves to the most conservative answer.
    /// Execution does not share this fallback — see `required_action`.
    fn resolve_action<'a>(&self, input: &'a Value) -> &'a str {
        self.forced_action.unwrap_or_else(|| {
            input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("tests")
        })
    }

    fn required_action(&self, input: &Value) -> Result<String, ToolError> {
        if let Some(forced) = self.forced_action {
            return Ok(forced.to_string());
        }
        required_action(input, self.name, Self::ACTIONS)
    }

    fn strip_action(&self, input: Value) -> Result<Value, ToolError> {
        let mut input = input;
        if let Some(obj) = input.as_object_mut() {
            obj.remove("action");
            Ok(input)
        } else {
            Err(ToolError::invalid_input("Run tool input must be an object"))
        }
    }
}

#[async_trait]
impl ToolSpec for RunTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        self.name == "Run"
    }

    fn description(&self) -> &'static str {
        "Run Cargo tests or repository verifier gates. Use tests for focused Rust test runs (auto-backgrounds after 30s by default unless background=false; set background=true to detach immediately); use verifiers for cross-language build, test, lint, and syntax gates."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["tests", "verifiers"],
                    "description": "Action to perform"
                },
                "args": {
                    "type": "string",
                    "description": "Extra arguments for cargo test (action=tests)"
                },
                "all_features": {
                    "type": "boolean",
                    "description": "Include --all-features for cargo test (action=tests)"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory, relative to the workspace, to run the tests or gates in. Must exist inside the workspace."
                },
                "profile": {
                    "type": "string",
                    "enum": ["auto", "rust", "node", "python", "go"],
                    "description": "Verifier profile (action=verifiers)"
                },
                "level": {
                    "type": "string",
                    "enum": ["quick", "full"],
                    "description": "Verifier level (action=verifiers)"
                },
                "max_python_files": {
                    "type": "integer",
                    "description": "Maximum Python files for the verifier syntax gate (action=verifiers)"
                },
                "commands": {
                    "type": "array",
                    "description": "Optional explicit verifier gates (action=verifiers)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "program": { "type": "string" },
                            "args": { "type": "array", "items": { "type": "string" } },
                            "cwd": { "type": "string" },
                            "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 1800000 }
                        },
                        "required": ["name", "program"]
                    }
                },
                "background": {
                    "type": "boolean",
                    "description": "Start tests or verifier gates immediately as background jobs and return task_id(s). For action=tests, omitting background waits up to timeout_ms (default 30000ms) and auto-backgrounds if still running; set background=false to block until completion."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1800000,
                    "description": "Wall-clock limit in milliseconds. For action=tests, foreground wait before auto-backgrounding (default 30000), or maximum blocking timeout when background=false. For action=verifiers (foreground only), built-in gate timeout."
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode, ToolCapability::Sandboxable]
    }

    fn approval_requirement_for(&self, _input: &Value) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn is_read_only_for(&self, _input: &Value) -> bool {
        false
    }

    fn supports_parallel_for(&self, _input: &Value) -> bool {
        false
    }

    fn starts_detached_for(&self, input: &Value) -> bool {
        matches!(self.resolve_action(input), "tests" | "verifiers")
            && input.get("background").and_then(Value::as_bool) == Some(true)
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        crate::tools::execution_envelope::enforce_tool_denial(context, self.name(), &input)?;
        let action = self.required_action(&input)?;
        let input = self.strip_action(input)?;

        match action.as_str() {
            "tests" => {
                self.validate_action_keys(&action, &input, Self::TESTS_ALLOWED_KEYS)?;
                RunTestsTool.execute(input, context).await
            }
            "verifiers" => {
                self.validate_action_keys(&action, &input, Self::VERIFIERS_ALLOWED_KEYS)?;
                RunVerifiersTool.execute(input, context).await
            }
            other => Err(ToolError::invalid_input(format!(
                "Unknown Run action \"{other}\"; nothing was run. Pass one of: {}.",
                Self::ACTIONS.join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    async fn err(input: Value) -> String {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        RunTool::new("Run")
            .execute(input, &ctx)
            .await
            .expect_err("call must be refused")
            .to_string()
    }

    /// Defaulting here ran `cargo test` for a model that meant `verifiers`.
    #[tokio::test]
    async fn missing_action_does_not_silently_run_tests() {
        let message = err(json!({"background": true})).await;
        assert!(message.contains("requires an `action`"), "{message}");
        assert!(message.contains("nothing was run"), "{message}");
        assert!(message.contains("tests, verifiers"), "{message}");
    }

    #[tokio::test]
    async fn unknown_action_names_the_actions_that_dispatch() {
        let message = err(json!({"action": "lint"})).await;
        assert!(message.contains("lint"), "{message}");
        assert!(message.contains("tests, verifiers"), "{message}");
    }

    #[test]
    fn advertised_actions_match_the_actions_that_dispatch() {
        let schema = RunTool::new("Run").input_schema();
        let advertised: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .map(|value| value.as_str().expect("string"))
            .collect();
        assert_eq!(advertised, RunTool::ACTIONS);
        let props = schema["properties"].as_object().expect("properties");
        for key in props.keys() {
            if key == "action" {
                continue;
            }
            assert!(
                RunTool::TESTS_ALLOWED_KEYS.contains(&key.as_str())
                    || RunTool::VERIFIERS_ALLOWED_KEYS.contains(&key.as_str()),
                "schema key `{key}` missing from per-action allowlists"
            );
        }
        let tool = RunTool::new("Run");
        assert!(tool.starts_detached_for(&json!({"action": "tests", "background": true})));
        assert!(tool.starts_detached_for(&json!({"action": "verifiers", "background": true})));
        assert!(!tool.starts_detached_for(&json!({"action": "tests"})));
        assert!(!tool.starts_detached_for(&json!({"action": "tests", "background": false})));
    }

    #[tokio::test]
    async fn cross_action_and_unknown_parameters_fail_loudly_without_running() {
        let tests_err = err(json!({
            "action": "tests",
            "profile": "rust",
            "level": "full",
            "background": true,
            "timeout_ms": 5000
        }))
        .await;
        assert!(tests_err.contains("Unexpected parameter"), "{tests_err}");
        assert!(tests_err.contains("level"), "{tests_err}");
        assert!(tests_err.contains("profile"), "{tests_err}");
        assert!(tests_err.contains("nothing was run"), "{tests_err}");
        assert!(tests_err.contains("background"), "{tests_err}");
        assert!(tests_err.contains("timeout_ms"), "{tests_err}");

        let verifiers_err = err(json!({
            "action": "verifiers",
            "args": "--lib foo",
            "all_features": true
        }))
        .await;
        assert!(
            verifiers_err.contains("Unexpected parameter"),
            "{verifiers_err}"
        );
        assert!(verifiers_err.contains("all_features"), "{verifiers_err}");
        assert!(verifiers_err.contains("args"), "{verifiers_err}");
        assert!(verifiers_err.contains("nothing was run"), "{verifiers_err}");
        assert!(verifiers_err.contains("profile"), "{verifiers_err}");

        let tool = RunTool::new("Run");
        assert!(
            tool.validate_action_keys(
                "tests",
                &json!({"args": "--lib", "background": true, "timeout_ms": 1000, "cwd": "."}),
                RunTool::TESTS_ALLOWED_KEYS
            )
            .is_ok()
        );
    }
}
