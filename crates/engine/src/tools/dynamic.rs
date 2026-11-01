use async_trait::async_trait;
use codewhale_protocol::runtime::DynamicToolSpec;
use serde_json::Value;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

pub struct RuntimeDynamicTool {
    spec: DynamicToolSpec,
}

impl RuntimeDynamicTool {
    pub fn new(spec: DynamicToolSpec) -> Self {
        Self { spec }
    }
}

#[async_trait]
impl ToolSpec for RuntimeDynamicTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        match &self.spec.namespace {
            Some(namespace) => format!(
                "runtime dynamic namespace {}",
                codewhale_runtime::safe_label::SafeLabel::identifier(namespace)
            )
            .into(),
            None => "runtime dynamic (no namespace)".into(),
        }
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn input_schema(&self) -> Value {
        self.spec.input_schema.clone()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        false
    }

    fn defer_loading(&self) -> bool {
        self.spec.defer_loading
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let executor = context
            .runtime
            .dynamic_tool_executor
            .as_ref()
            .ok_or_else(|| {
                ToolError::not_available(format!(
                    "runtime dynamic tool '{}' has no executor",
                    self.spec.name
                ))
            })?;
        executor
            .execute_dynamic_tool(
                context.runtime.active_thread_id.clone(),
                self.spec.namespace.clone(),
                self.spec.name.clone(),
                input,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::*;
    use crate::tools::spec::{DynamicToolExecutor, RuntimeToolServices};

    struct EchoExecutor;

    #[async_trait]
    impl DynamicToolExecutor for EchoExecutor {
        async fn execute_dynamic_tool(
            &self,
            thread_id: Option<String>,
            namespace: Option<String>,
            name: String,
            input: Value,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success(
                json!({
                    "thread_id": thread_id,
                    "namespace": namespace,
                    "name": name,
                    "input": input,
                })
                .to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn runtime_dynamic_tool_delegates_to_runtime_executor() {
        let tool = RuntimeDynamicTool::new(DynamicToolSpec {
            namespace: Some("bench".to_string()),
            name: "lookup".to_string(),
            description: "Lookup a record".to_string(),
            input_schema: json!({"type": "object"}),
            defer_loading: true,
        });
        let ctx = ToolContext::new(".").with_runtime_services(RuntimeToolServices {
            active_thread_id: Some("thr_1".to_string()),
            dynamic_tool_executor: Some(Arc::new(EchoExecutor)),
            ..RuntimeToolServices::default()
        });

        let result = tool.execute(json!({"id": "123"}), &ctx).await.unwrap();

        assert!(result.success);
        assert!(result.content.contains("\"thread_id\":\"thr_1\""));
        assert!(result.content.contains("\"namespace\":\"bench\""));
        assert!(result.content.contains("\"name\":\"lookup\""));
    }
}
