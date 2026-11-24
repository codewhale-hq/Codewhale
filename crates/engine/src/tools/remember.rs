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
    limit: Option<u64>,
}
pub struct RememberTool;
#[async_trait]
impl ToolSpec for RememberTool {
    fn name(&self) -> &'static str {
        "remember"
    }
    fn description(&self) -> &'static str {
        "Propose a durable memory or correction for review in Context Lens. A successful capture is a candidate, not active knowledge. Do not store secrets, transient task state or private reasoning. Existing memory is never silently replaced or deleted by this tool. action=list returns up to limit current notes with their exact text for `replaces`."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"properties":{
            "action":{"type":"string","enum":["append","revise","list"],"default":"append"},
            "note":{"type":"string","minLength":1,"maxLength":8192},
            "scope":{"type":"string","enum":["global","workspace"]},
            "replaces":{"type":"string","description":"Exact current note, required for a correction."},
            "evidence":{"type":"string","description":"Why the proposed correction is supported."},
            "limit":{"type":"integer","minimum":1,"maximum":20,"default":8,"description":"list only."}
        }})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }
    fn is_read_only_for(&self, input: &Value) -> bool {
        input.get("action").and_then(Value::as_str) == Some("list")
    }
    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let input: Input = serde_json::from_value(input).map_err(|error| {
            ToolError::invalid_input(format!("invalid memory proposal: {error}"))
        })?;
        let path = context
            .memory_path
            .clone()
            .ok_or_else(|| ToolError::execution_failed("memory is disabled"))?;
        let store = NativeMemoryStore::from_global_path(&path)
            .ok_or_else(|| ToolError::execution_failed("native memory store is not configured"))?;
        let workspace = context.workspace.clone();
        let action = input.action.unwrap_or_else(|| "append".into());
        if !matches!(action.as_str(), "append" | "revise" | "list") {
            return Err(ToolError::invalid_input(
                "Only append, revise and list are supported. Forgetting requires the user's Context Lens control.",
            ));
        }
        if action == "list" {
            let scope = match input.scope.as_deref() {
                None => None,
                Some("global") => Some(MemoryScope::Global),
                Some("workspace") => Some(MemoryScope::Workspace),
                Some(_) => {
                    return Err(ToolError::invalid_input(
                        "scope must be global or workspace",
                    ));
                }
            };
            let limit = input.limit.unwrap_or(8).clamp(1, 20) as usize;
            let hits = tokio::task::spawn_blocking(move || {
                let workspace_id = NativeMemoryStore::workspace_id(&workspace)?;
                store.list_in_workspace(scope, workspace_id.as_deref(), Some(&workspace), limit + 1)
            })
            .await
            .map_err(|_| ToolError::execution_failed("memory worker stopped"))?
            .map_err(|error| ToolError::execution_failed(format!("memory list failed: {error}")))?;
            return Ok(super::native_memory::output(hits, limit));
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
                Some(NativeMemoryStore::workspace_id(&workspace).map_err(|error|ToolError::execution_failed(format!("workspace identity unavailable: {error}")))?
                    .ok_or_else(||ToolError::execution_failed("workspace memory needs a git origin; explicitly select global for a user preference"))?)
            } else {None};
            let result=if action=="revise" {
                let from=input.replaces.as_deref().filter(|s|!s.trim().is_empty()).ok_or_else(||ToolError::invalid_input("replaces is required"))?;
                let evidence=input.evidence.as_deref().filter(|s|!s.trim().is_empty()).ok_or_else(||ToolError::invalid_input("evidence is required"))?;
                store.revise(scope,workspace_id.as_deref(),from,&note,evidence)
            } else {store.remember(scope,workspace_id.as_deref(),&note)};
            result.map_err(|error|ToolError::execution_failed(format!("memory proposal was rejected or could not be stored: {error}; no active memory was changed")))
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
    async fn rejected_correction_reports_the_store_reason_and_list_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        context.memory_path = Some(temp.path().join("memory/global/MEMORY.md"));
        let error = RememberTool
            .execute(
                json!({"action":"revise","note":"new","replaces":"missing","evidence":"seen","scope":"global"}),
                &context,
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("exactly one active note"),
            "{error}"
        );
        let list = json!({"action":"list","scope":"global","limit":3});
        assert!(RememberTool.is_read_only_for(&list));
        let listed = RememberTool.execute(list, &context).await.unwrap();
        assert!(listed.content.contains("No bounded memory matches."));
    }
    #[tokio::test]
    async fn list_limits_eligible_notes_and_reports_when_more_remain() {
        use codewhale_memory::{MemoryBackend, Snapshot};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("memory");
        let mut context = ToolContext::new(temp.path());
        context.memory_path = Some(root.join("global/MEMORY.md"));
        let native = NativeMemoryStore::new(&root);
        let empty = RememberTool
            .execute(json!({"action":"list"}), &context)
            .await
            .unwrap();
        assert_eq!(empty.metadata.unwrap()["count"], 0);
        assert!(!root.exists());
        for note in ["Use cedar.", "Use birch.", "Use spruce."] {
            native.remember(MemoryScope::Global, None, note).unwrap();
        }
        let access = Access::operator(vec![NativeMemoryStore::owner_scope()]).unwrap();
        let mut store = native.open_structured().unwrap();
        let ordered = store.list(&access, None, 3).unwrap();
        let list = json!({"action":"list","scope":"global","limit":1});
        for (index, memory) in ordered.iter().enumerate().skip(1) {
            store
                .approve(
                    &access,
                    &memory.id,
                    memory.revision,
                    None,
                    &Snapshot::default(),
                )
                .unwrap();
            let listed = RememberTool.execute(list.clone(), &context).await.unwrap();
            let metadata = listed.metadata.unwrap();
            assert_eq!(metadata["count"], 1);
            assert_eq!(metadata["truncated"], index > 1);
            assert!(listed.content.contains(&ordered[1].draft.body));
            assert!(!listed.content.contains(&ordered[0].draft.body));
            assert!(!listed.content.contains(&ordered[2].draft.body));
        }
    }
    #[tokio::test]
    async fn listing_checks_workspace_dependency_freshness() {
        use codewhale_memory::{MemoryBackend, policy, workspace};

        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        let native = NativeMemoryStore::new(temp.path().join("memory"));
        context.memory_path = Some(native.global_path());
        let id = NativeMemoryStore::workspace_id(temp.path())
            .unwrap()
            .unwrap();
        let access =
            Access::operator(vec![NativeMemoryStore::workspace_scope(&id).unwrap()]).unwrap();
        native
            .remember(MemoryScope::Workspace, Some(&id), "Fixture note.")
            .unwrap();
        let mut store = native.open_structured().unwrap();
        let mut draft = store.list(&access, None, 1).unwrap().remove(0).draft;
        draft.body = "This note tracks file state.".into();
        draft
            .dependencies
            .insert("note.txt".into(), policy::sha256(b"first"));
        std::fs::write(temp.path().join("note.txt"), "first").unwrap();
        let memory = store
            .capture(&access, "file-dependent-note", draft)
            .unwrap()
            .memory;
        let snapshot = workspace::snapshot(temp.path(), ["note.txt".to_owned()]).unwrap();
        store
            .approve(&access, &memory.id, memory.revision, None, &snapshot)
            .unwrap();
        let list = json!({"action":"list","scope":"workspace"});
        assert!(
            RememberTool
                .execute(list.clone(), &context)
                .await
                .unwrap()
                .content
                .contains("freshness=current")
        );
        std::fs::write(temp.path().join("note.txt"), "second").unwrap();
        assert!(
            RememberTool
                .execute(list, &context)
                .await
                .unwrap()
                .content
                .contains("freshness=stale_or_unknown")
        );
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
