//! Model capture proposes candidates. It cannot review or delete user memory.
//! The trusted Context Lens owns approval, correction and forgetting controls.
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use async_trait::async_trait;
use codewhale_runtime::native_memory::{MemoryScope, NativeMemoryStore};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    action: Option<String>,
    note: Option<String>,
    replaces: Option<String>,
    evidence: Option<String>,
    scope: Option<String>,
}
pub struct RememberTool;
#[async_trait]
impl ToolSpec for RememberTool {
    fn name(&self) -> &'static str {
        "remember"
    }
    fn description(&self) -> &'static str {
        "Propose a durable memory or correction for review in Context Lens. A successful capture is a candidate, not active knowledge. Do not store secrets, transient task state or private reasoning. Existing memory is never silently replaced or deleted by this tool."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"properties":{
        "action":{"type":"string","enum":["append","revise"],"default":"append"},
        "note":{"type":"string","minLength":1,"maxLength":8192},
        "scope":{"type":"string","enum":["global","workspace"]},
        "replaces":{"type":"string","description":"Exact current note, required for a correction."},
        "evidence":{"type":"string","description":"Why the proposed correction is supported."}
    },"required":["note"]})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }
    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let input: Input = serde_json::from_value(input)
            .map_err(|_| ToolError::invalid_input("invalid memory proposal"))?;
        let path = context
            .memory_path
            .clone()
            .ok_or_else(|| ToolError::execution_failed("memory is disabled"))?;
        let store = NativeMemoryStore::from_global_path(&path)
            .ok_or_else(|| ToolError::execution_failed("native memory store is not configured"))?;
        let workspace = context.workspace.clone();
        let action = input.action.unwrap_or_else(|| "append".into());
        if !matches!(action.as_str(), "append" | "revise") {
            return Err(ToolError::invalid_input(
                "Only append and revise candidates are supported. Forgetting requires the user's Context Lens control.",
            ));
        }
        let note = input
            .note
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::invalid_input("a note is required"))?;
        let scope = match input.scope.as_deref().unwrap_or("workspace") {
            "global" => MemoryScope::Global,
            "workspace" => MemoryScope::Workspace,
            _ => {
                return Err(ToolError::invalid_input(
                    "scope must be global or workspace",
                ));
            }
        };
        let hit=tokio::task::spawn_blocking(move||{
            let workspace_id=if scope==MemoryScope::Workspace {
                Some(NativeMemoryStore::workspace_id(&workspace).map_err(|_|ToolError::execution_failed("workspace identity unavailable"))?
                    .ok_or_else(||ToolError::execution_failed("workspace memory needs a git origin; explicitly select global for a user preference"))?)
            } else {None};
            let result=if action=="revise" {
                let from=input.replaces.as_deref().filter(|s|!s.trim().is_empty()).ok_or_else(||ToolError::invalid_input("replaces is required"))?;
                let evidence=input.evidence.as_deref().filter(|s|!s.trim().is_empty()).ok_or_else(||ToolError::invalid_input("evidence is required"))?;
                store.revise(scope,workspace_id.as_deref(),from,&note,evidence)
            } else {store.remember(scope,workspace_id.as_deref(),&note)};
            result.map_err(|_|ToolError::execution_failed("memory proposal was rejected or could not be stored; no active memory was changed"))
        }).await.map_err(|_|ToolError::execution_failed("memory worker stopped"))??;
        Ok(ToolResult::success(format!("Memory candidate #{} is available for review in Context Lens. It is not active context; existing knowledge is unchanged.",hit.id))
            .with_metadata(json!({"memory_backend":"native","memory_schema":2,"memory_id":hit.id,"candidate":true,"untrusted":true})))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_memory::{Access, Status};
    #[tokio::test]
    async fn disabled_capture_does_not_create_store() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        context.memory_path = None;
        assert!(
            RememberTool
                .execute(
                    json!({"note":"Keep constraints explicit","scope":"global"}),
                    &context
                )
                .await
                .is_err()
        );
        assert!(!temp.path().join("memory").exists());
    }
    #[tokio::test]
    async fn capture_is_reviewable_not_recalled() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        let root = temp.path().join("memory");
        context.memory_path = Some(root.join("global/MEMORY.md"));
        let result = RememberTool
            .execute(
                json!({"note":"Keep constraints explicit","scope":"global"}),
                &context,
            )
            .await
            .unwrap();
        assert!(result.success);
        let native = NativeMemoryStore::new(&root);
        assert!(native.search("constraints", 10).unwrap().is_empty());
        let store = native.open_structured().unwrap();
        let access = Access::operator(vec![NativeMemoryStore::owner_scope()]).unwrap();
        assert_eq!(
            store.list(&access, None, 10).unwrap()[0].status,
            Status::Candidate
        );
        assert!(!root.join("global/MEMORY.md").exists());
    }
    #[tokio::test]
    async fn forged_review_field_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let context = ToolContext::new(temp.path());
        assert!(
            RememberTool
                .execute(json!({"note":"claim","approved":true}), &context)
                .await
                .is_err()
        );
    }
}
