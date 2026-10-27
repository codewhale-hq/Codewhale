//! Canonical action-based wrapper for web tools.
//!
//! The model sees one tool: `Web` with an `action` parameter
//! (search | fetch | wait). The per-action legacy execution aliases were
//! removed in v0.9.3.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::dev_server_readiness::WaitForDevServerTool;
use super::fetch_url::FetchUrlTool;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use super::web_search::WebSearchTool;
use codewhale_tools::required_action;

pub struct WebTool {
    name: &'static str,
    forced_action: Option<&'static str>,
}

impl WebTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
        }
    }

    const ACTIONS: &'static [&'static str] = &["search", "fetch", "wait"];

    /// Policy-side resolution: approval and parallel-safety predicates cannot
    /// fail, so a missing action resolves to the most conservative answer.
    /// Execution does not share this fallback — see `required_action`.
    fn resolve_action<'a>(&self, input: &'a Value) -> &'a str {
        self.forced_action.unwrap_or_else(|| {
            input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("search")
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
            Err(ToolError::invalid_input("Web tool input must be an object"))
        }
    }
}

#[async_trait]
impl ToolSpec for WebTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        self.name == "Web"
    }

    fn description(&self) -> &'static str {
        "Search the web, fetch a known URL, or wait for a local dev server. fetch retrieves a canonical URL directly; search finds sources when the URL is unknown. Web actions are read-only and network-policy aware."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "fetch", "wait"],
                    "description": "Action to perform"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (action=search)"
                },
                "q": {
                    "type": "string",
                    "description": "Search query alias (action=search)"
                },
                "search_query": {
                    "type": "array",
                    "description": "Advanced search query array (action=search)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "q": { "type": "string" },
                            "query": { "type": "string" },
                            "max_results": { "type": "integer" },
                            "recency": {
                                "oneOf": [
                                    { "type": "string", "enum": ["day", "week", "month", "year"] },
                                    { "type": "integer", "minimum": 1, "maximum": 3650 }
                                ]
                            },
                            "domains": { "type": "array", "items": { "type": "string" } },
                            "locale": {
                                "type": "string",
                                "description": "BCP 47-style result locale tag such as zh-CN or ja-JP"
                            }
                        }
                    }
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum search results (action=search)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (action=search, fetch, or wait)"
                },
                "recency": {
                    "oneOf": [
                        { "type": "string", "enum": ["day", "week", "month", "year"] },
                        { "type": "integer", "minimum": 1, "maximum": 3650 }
                    ],
                    "description": "Requested freshness window (action=search)"
                },
                "domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict search results to domains (action=search)"
                },
                "locale": {
                    "type": "string",
                    "description": "Requested result locale as a BCP 47-style tag such as zh-CN or ja-JP (action=search); malformed values are ignored and backends that cannot honor the region report it as degraded"
                },
                "url": {
                    "type": "string",
                    "description": "URL to fetch (action=fetch) or healthcheck URL (action=wait)"
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "markdown", "raw"],
                    "description": "Post-processing for fetched response (action=fetch)"
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Truncate fetched response after this many bytes (action=fetch)"
                },
                "fields": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional JSONPath projections for JSON responses (action=fetch). Supports $, .field, [index], [*], and ['field']; for example $.items[*].name. Recursive descent (..) and filters are not supported; returns matches under fields."
                },
                "host": {
                    "type": "string",
                    "description": "Loopback host to poll (action=wait)"
                },
                "port": {
                    "type": "integer",
                    "description": "TCP port to wait for (action=wait)"
                },
                "poll_interval_ms": {
                    "type": "integer",
                    "description": "Delay between readiness probes in milliseconds (action=wait)"
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match self.resolve_action(input) {
            "fetch" => FetchUrlTool.approval_requirement_for(input),
            "search" => WebSearchTool.approval_requirement_for(input),
            "wait" => WaitForDevServerTool.approval_requirement_for(input),
            _ => ApprovalRequirement::Required,
        }
    }

    fn is_read_only_for(&self, _input: &Value) -> bool {
        true
    }

    fn supports_parallel_for(&self, input: &Value) -> bool {
        self.resolve_action(input) == "search"
    }

    fn starts_detached_for(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = self.required_action(&input)?;
        let input = self.strip_action(input)?;

        match action.as_str() {
            "search" => WebSearchTool.execute(input, context).await,
            "fetch" => FetchUrlTool.execute(input, context).await,
            "wait" => WaitForDevServerTool.execute(input, context).await,
            other => Err(ToolError::invalid_input(format!(
                "Unknown Web action \"{other}\"; nothing was run. Pass one of: {}.",
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
        WebTool::new("Web")
            .execute(input, &ctx)
            .await
            .expect_err("call must be refused")
            .to_string()
    }

    /// `Web{url: ...}` meant `fetch`; defaulting turned it into a search.
    #[tokio::test]
    async fn missing_action_does_not_silently_search() {
        let message = err(json!({"url": "https://example.com"})).await;
        assert!(message.contains("requires an `action`"), "{message}");
        assert!(message.contains("nothing was run"), "{message}");
        assert!(message.contains("search, fetch, wait"), "{message}");
    }

    #[tokio::test]
    async fn unknown_action_names_the_actions_that_dispatch() {
        let message = err(json!({"action": "get", "url": "https://example.com"})).await;
        assert!(message.contains("get"), "{message}");
        assert!(message.contains("search, fetch, wait"), "{message}");
    }

    #[test]
    fn advertised_actions_match_the_actions_that_dispatch() {
        let schema = WebTool::new("Web").input_schema();
        let advertised: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .map(|value| value.as_str().expect("string"))
            .collect();
        assert_eq!(advertised, WebTool::ACTIONS);
    }
}
