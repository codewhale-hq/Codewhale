//! Structured data validation tool: `validate_data`.
//!
//! Validates JSON or TOML from inline content or a workspace file path and
//! returns parser errors with lightweight metadata.

use std::fs;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_str,
};

/// Tool for validating JSON/TOML configuration data.
pub struct ValidateDataTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataFormat {
    Auto,
    Json,
    Toml,
}

impl DataFormat {
    fn from_input(raw: Option<&str>) -> Result<Self, ToolError> {
        let format = raw.unwrap_or("auto");
        match format {
            "auto" => Ok(Self::Auto),
            "json" => Ok(Self::Json),
            "toml" => Ok(Self::Toml),
            _ => Err(ToolError::invalid_input(format!(
                "Unsupported format '{format}'. Expected one of: auto, json, toml"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Json => "json",
            Self::Toml => "toml",
        }
    }
}

#[async_trait]
impl ToolSpec for ValidateDataTool {
    fn name(&self) -> &'static str {
        "validate_data"
    }

    fn description(&self) -> &'static str {
        "Validate JSON or TOML content from inline input or a workspace file."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Optional path to a file within the workspace."
                },
                "content": {
                    "type": "string",
                    "description": "Optional inline content to validate."
                },
                "format": {
                    "type": "string",
                    "enum": ["auto", "json", "toml"],
                    "default": "auto",
                    "description": "Validation format. 'auto' infers from extension then falls back to trying both."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let path = optional_str(&input, "path")?;
        let content = optional_str(&input, "content")?;
        let requested_format = DataFormat::from_input(optional_str(&input, "format")?)?;

        let (source_name, raw_content, extension) = load_input_source(path, content, context)?;
        if context
            .features
            .enabled(codewhale_runtime::features::Feature::DataHost)
        {
            return validate_host(
                &raw_content,
                &source_name,
                requested_format,
                extension.as_deref(),
                context,
            )
            .await;
        }
        match requested_format {
            DataFormat::Json => validate_json(&raw_content, &source_name),
            DataFormat::Toml => validate_toml(&raw_content, &source_name),
            DataFormat::Auto => validate_auto(&raw_content, &source_name, extension.as_deref()),
        }
    }
}

fn load_input_source(
    path: Option<&str>,
    content: Option<&str>,
    context: &ToolContext,
) -> Result<(String, String, Option<String>), ToolError> {
    match (path, content) {
        (Some(_), Some(_)) => Err(ToolError::invalid_input(
            "Provide either 'path' or 'content', but not both.",
        )),
        (None, None) => Err(ToolError::missing_field("path or content")),
        (Some(path), None) => {
            let resolved =
                crate::tools::file::resolve_guarded_read_path(context, path, "validate_data")?;
            let raw_content = if context
                .features
                .enabled(codewhale_runtime::features::Feature::DataHost)
            {
                use std::io::Read;
                let mut content = String::new();
                fs::File::open(&resolved)
                    .and_then(|file| file.take(1024 * 1024 + 1).read_to_string(&mut content))
                    .map_err(|e| {
                        ToolError::execution_failed(format!(
                            "Failed to read {}: {e}",
                            resolved.display()
                        ))
                    })?;
                if content.len() > 1024 * 1024 {
                    return Err(ToolError::invalid_input("Host data input exceeds 1 MiB"));
                }
                content
            } else {
                fs::read_to_string(&resolved).map_err(|e| {
                    ToolError::execution_failed(format!(
                        "Failed to read {}: {e}",
                        resolved.display()
                    ))
                })?
            };
            let extension = resolved
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|s| s.to_ascii_lowercase());
            Ok((path.to_string(), raw_content, extension))
        }
        (None, Some(content)) => Ok(("inline".to_string(), content.to_string(), None)),
    }
}

/// Preserve the existing Rust parsers and their exact diagnostics. The host
/// selects the format and owns summary/result presentation from minimal facts.
async fn validate_host(
    raw: &str,
    source: &str,
    format: DataFormat,
    extension: Option<&str>,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    if raw.len() > 1024 * 1024 {
        return Err(ToolError::invalid_input("Host data input exceeds 1 MiB"));
    }
    let json = match serde_json::from_str::<Value>(raw) {
        Ok(parsed) => json!({"ok":true,"value":json_descriptor(&parsed)}),
        Err(error) => json!({"ok":false,"error":error.to_string()}),
    };
    let toml = match toml::from_str::<toml::Value>(raw) {
        Ok(parsed) => json!({"ok":true,"value":toml_descriptor(&parsed)}),
        Err(error) => json!({"ok":false,"error":error.to_string()}),
    };
    let result = crate::extension_host::manager().execute_stock(
        crate::extension_host::StockOperation::ValidateData,
        json!({"format":format.as_str(),"source":source,"extension":extension,"json":json,"toml":toml}),
        context,
        std::time::Duration::from_secs(120),
    ).await?;
    let metadata = result
        .metadata
        .as_ref()
        .ok_or_else(|| ToolError::execution_failed("Host data result omitted metadata"))?;
    if metadata.get("valid").and_then(Value::as_bool) != Some(result.success)
        || metadata.get("source").and_then(Value::as_str) != Some(source)
    {
        return Err(ToolError::execution_failed(
            "Host data result changed its captured source",
        ));
    }
    if result.success {
        if metadata.get("summary").is_none() {
            return Err(ToolError::execution_failed(
                "Host data result changed its captured source",
            ));
        }
        ToolResult::json(metadata).map_err(|error| ToolError::execution_failed(error.to_string()))
    } else {
        Ok(result)
    }
}
fn json_descriptor(value: &Value) -> Value {
    match value {
        Value::Object(map) => json!({"kind":"object","keys":map.keys().collect::<Vec<_>>()}),
        Value::Array(array) => json!({"kind":"array","entries":array.len()}),
        Value::String(_) => json!({"kind":"string"}),
        Value::Number(_) => json!({"kind":"number"}),
        Value::Bool(_) => json!({"kind":"boolean"}),
        Value::Null => json!({"kind":"null"}),
    }
}
fn toml_descriptor(value: &toml::Value) -> Value {
    match value {
        toml::Value::Table(table) => {
            json!({"kind":"table","keys":table.keys().collect::<Vec<_>>()})
        }
        toml::Value::Array(array) => json!({"kind":"array","entries":array.len()}),
        toml::Value::String(_) => json!({"kind":"string"}),
        toml::Value::Integer(_) => json!({"kind":"integer"}),
        toml::Value::Float(_) => json!({"kind":"float"}),
        toml::Value::Boolean(_) => json!({"kind":"boolean"}),
        toml::Value::Datetime(_) => json!({"kind":"datetime"}),
    }
}

fn validate_auto(
    raw_content: &str,
    source_name: &str,
    extension: Option<&str>,
) -> Result<ToolResult, ToolError> {
    let hint = match extension {
        Some("json") => Some(DataFormat::Json),
        Some("toml") => Some(DataFormat::Toml),
        _ => None,
    };

    if let Some(format_hint) = hint {
        return match format_hint {
            DataFormat::Json => validate_json(raw_content, source_name),
            DataFormat::Toml => validate_toml(raw_content, source_name),
            DataFormat::Auto => unreachable!(),
        };
    }

    let json_result = serde_json::from_str::<serde_json::Value>(raw_content);
    if let Ok(parsed) = &json_result {
        return build_success_result(DataFormat::Json, source_name, summarize_json(parsed));
    }

    let toml_result = toml::from_str::<toml::Value>(raw_content);
    if let Ok(parsed) = &toml_result {
        return build_success_result(DataFormat::Toml, source_name, summarize_toml(parsed));
    }

    let json_error = json_result.err().map(|e| e.to_string()).unwrap_or_default();
    let toml_error = toml_result.err().map(|e| e.to_string()).unwrap_or_default();

    Ok(
        ToolResult::error(
            "Validation failed in auto mode: content is neither valid JSON nor TOML.",
        )
        .with_metadata(json!({
            "valid": false,
            "format": DataFormat::Auto.as_str(),
            "source": source_name,
            "json_error": json_error,
            "toml_error": toml_error,
        })),
    )
}

fn validate_json(raw_content: &str, source_name: &str) -> Result<ToolResult, ToolError> {
    match serde_json::from_str::<serde_json::Value>(raw_content) {
        Ok(parsed) => build_success_result(DataFormat::Json, source_name, summarize_json(&parsed)),
        Err(err) => Ok(
            ToolResult::error(format!("Invalid JSON: {err}")).with_metadata(json!({
                "valid": false,
                "format": DataFormat::Json.as_str(),
                "source": source_name,
                "error": err.to_string(),
            })),
        ),
    }
}

fn validate_toml(raw_content: &str, source_name: &str) -> Result<ToolResult, ToolError> {
    match toml::from_str::<toml::Value>(raw_content) {
        Ok(parsed) => build_success_result(DataFormat::Toml, source_name, summarize_toml(&parsed)),
        Err(err) => Ok(
            ToolResult::error(format!("Invalid TOML: {err}")).with_metadata(json!({
                "valid": false,
                "format": DataFormat::Toml.as_str(),
                "source": source_name,
                "error": err.to_string(),
            })),
        ),
    }
}

fn build_success_result(
    format: DataFormat,
    source_name: &str,
    summary: Value,
) -> Result<ToolResult, ToolError> {
    ToolResult::json(&json!({
        "valid": true,
        "format": format.as_str(),
        "source": source_name,
        "summary": summary,
    }))
    .map_err(|e| ToolError::execution_failed(e.to_string()))
}

fn summarize_json(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Object(map) => json!({
            "top_level": "object",
            "entries": map.len(),
            "keys_preview": map.keys().take(10).collect::<Vec<_>>(),
        }),
        serde_json::Value::Array(arr) => json!({
            "top_level": "array",
            "entries": arr.len(),
        }),
        serde_json::Value::String(_) => json!({ "top_level": "string" }),
        serde_json::Value::Number(_) => json!({ "top_level": "number" }),
        serde_json::Value::Bool(_) => json!({ "top_level": "boolean" }),
        serde_json::Value::Null => json!({ "top_level": "null" }),
    }
}

fn summarize_toml(value: &toml::Value) -> Value {
    match value {
        toml::Value::Table(table) => json!({
            "top_level": "table",
            "entries": table.len(),
            "keys_preview": table.keys().take(10).collect::<Vec<_>>(),
        }),
        toml::Value::Array(arr) => json!({
            "top_level": "array",
            "entries": arr.len(),
        }),
        toml::Value::String(_) => json!({ "top_level": "string" }),
        toml::Value::Integer(_) => json!({ "top_level": "integer" }),
        toml::Value::Float(_) => json!({ "top_level": "float" }),
        toml::Value::Boolean(_) => json!({ "top_level": "boolean" }),
        toml::Value::Datetime(_) => json!({ "top_level": "datetime" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn validate_json_content_succeeds() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());

        let result = ValidateDataTool
            .execute(
                json!({"content": "{\"name\":\"deepseek\"}", "format": "json"}),
                &ctx,
            )
            .await
            .expect("execute");
        assert!(result.success);
        let content: Value = serde_json::from_str(&result.content).expect("validation json");
        assert_eq!(content.get("valid").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn validate_toml_file_succeeds() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());
        let config = tmp.path().join("config.toml");
        fs::write(&config, "name = \"deepseek\"\n").expect("write");

        let result = ValidateDataTool
            .execute(json!({"path": "config.toml", "format": "toml"}), &ctx)
            .await
            .expect("execute");
        assert!(result.success);
        let content: Value = serde_json::from_str(&result.content).expect("validation json");
        assert_eq!(content.get("format").and_then(Value::as_str), Some("toml"));
    }

    #[tokio::test]
    async fn validate_auto_reports_error_for_invalid_content() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());

        let result = ValidateDataTool
            .execute(json!({"content": "not-valid-data"}), &ctx)
            .await
            .expect("execute");
        assert!(!result.success);
        assert!(result.content.contains("Validation failed in auto mode"));
    }

    #[tokio::test]
    async fn validate_rejects_path_and_content_together() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path());

        let err = ValidateDataTool
            .execute(json!({"path": "a.toml", "content": "x=1"}), &ctx)
            .await
            .expect_err("should fail");
        assert!(matches!(err, ToolError::InvalidInput { .. }));
    }
}
