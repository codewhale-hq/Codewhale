use async_trait::async_trait;
use codewhale_ratatui::{DATA_DISPLAY_SCHEMA, DataDisplay, DataWords};
use serde_json::{Value, json};

use super::spec::{ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec};

pub struct ShowDataTool;

pub fn parse_data_display(input: &Value) -> Result<DataDisplay, ToolError> {
    DataDisplay::from_json(&input.to_string())
        .map_err(|error| ToolError::invalid_input(error.to_string()))
}

#[async_trait]
impl ToolSpec for ShowDataTool {
    fn name(&self) -> &'static str {
        "show_data"
    }

    fn description(&self) -> &'static str {
        "Show the person numbers as a small terminal chart: `sparkline` (one row, newest value \
         on the right), `bars` (up to 24 labelled bars) or `trend` (a multi-row line chart). \
         Read-only: nothing is written or sent. Values must be finite numbers, oldest first. \
         The result repeats the same facts as plain text, so say what matters in words too."
    }

    fn input_schema(&self) -> Value {
        serde_json::from_str(DATA_DISPLAY_SCHEMA).expect("kit DATA_DISPLAY_SCHEMA is valid JSON")
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        let display = parse_data_display(&input)?;
        let summary = format!("Shown: {}", display.describe(&DataWords::default()));
        Ok(ToolResult::success(summary).with_metadata(json!({ "data_display": display })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn schema_parses_and_bad_input_names_the_broken_limit() {
        assert_eq!(ShowDataTool.input_schema()["type"], "object");

        let shown = parse_data_display(&json!({
            "kind": "bars",
            "label": "Tokens",
            "unit": "tok",
            "items": [{ "label": "opus", "value": 100.0 }]
        }))
        .expect("valid bars");
        let words = shown.describe(&DataWords::default());
        assert!(words.starts_with("Tokens: opus 100"), "{words}");

        let context = ToolContext::new(".");
        let result = ShowDataTool
            .execute(serde_json::to_value(&shown).unwrap(), &context)
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.content, format!("Shown: {words}"));
        assert_eq!(
            result.metadata.as_ref().unwrap()["data_display"],
            serde_json::to_value(shown).unwrap()
        );

        let too_many: Vec<Value> = (0..25)
            .map(|i| json!({ "label": format!("b{i}"), "value": i }))
            .collect();
        let error = ShowDataTool
            .execute(
                json!({
                    "kind": "bars",
                    "label": "Too many",
                    "items": too_many
                }),
                &context,
            )
            .await
            .expect_err("25 bars is over the limit");
        assert!(error.to_string().contains("25 bars; at most 24"), "{error}");

        let error =
            parse_data_display(&json!({ "kind": "pie", "label": "x" })).expect_err("unknown kind");
        assert!(error.to_string().contains("not a data display"), "{error}");
    }
}
