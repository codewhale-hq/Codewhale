//! Model-callable plugin review request. Never installs, trusts, or enables.
//!
//! 0.10.1 plugin offering policy: the tool is registered only in the
//! interactive TUI with contextual tips on (it returns a TUI slash command),
//! and a session gets one review request. A second call errors.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};
use codewhale_plugins::recommend::{load_marketplace_candidates, lookup_reviewable_plugin};

pub const REQUEST_PLUGIN_INSTALL_TOOL_NAME: &str = "request_plugin_install";

pub struct RequestPluginInstallTool;

/// Sessions (by `ToolContext::state_namespace`, the session id) that already
/// surfaced a review request. The registry is rebuilt every turn, so the
/// once-per-session budget cannot live on the tool value.
static REQUESTED_SESSIONS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn session_already_requested(namespace: &str) -> bool {
    REQUESTED_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(namespace)
}

fn record_session_request(namespace: &str) {
    REQUESTED_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(namespace.to_string());
}

#[async_trait]
impl ToolSpec for RequestPluginInstallTool {
    fn name(&self) -> &'static str {
        REQUEST_PLUGIN_INSTALL_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Ask the human to review a plugin the current task clearly needs \
         (installed-but-idle, or in a catalog they added). Never to advertise. \
         Once per session; a second call fails. Installs, trusts, and enables \
         nothing. Pass `name` and a short `reason`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Plugin name as shown by /plugin list or /plugin suggest."
                },
                "reason": {
                    "type": "string",
                    "description": "Short reason this plugin fits the current task."
                }
            },
            "required": ["name", "reason"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let name = required_str(&input, "name")?.trim();
        let reason = required_str(&input, "reason")?.trim();
        if name.is_empty() {
            return Err(ToolError::invalid_input(
                "request_plugin_install: name must not be empty",
            ));
        }
        if reason.is_empty() {
            return Err(ToolError::invalid_input(
                "request_plugin_install: reason must not be empty",
            ));
        }
        if session_already_requested(&ctx.state_namespace) {
            return Err(ToolError::not_available(
                "request_plugin_install: already used this session; a plugin review may be requested once per session",
            ));
        }
        let Some(registry) = ctx.plugin_registry.as_ref() else {
            return Err(ToolError::not_available(
                "request_plugin_install: plugin registry is not available",
            ));
        };
        let marketplace = load_marketplace_candidates(registry.state_path());
        let Some(matched) = lookup_reviewable_plugin(name, registry, &marketplace) else {
            return Err(ToolError::invalid_input(format!(
                "request_plugin_install: unknown plugin `{name}`"
            )));
        };
        record_session_request(&ctx.state_namespace);
        let command = matched.command();
        let payload = json!({
            "completed": false,
            "installed": false,
            "plugin": matched.name,
            "plugin_id": matched.id,
            "command": command,
            "reason": reason,
        });
        let mut result = ToolResult::success(format!(
            "Review requested for {}. Run `{command}` — nothing was installed, trusted, or enabled. Reason: {reason}",
            matched.name
        ));
        result.metadata = Some(payload);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn write_keyword_bundle(root: &std::path::Path, name: &str) {
        let bundle = root.join(".codewhale/plugins").join(name);
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("plugin.toml"),
            format!(
                "schema_version = 1\n[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\ndescription = \"{name}\"\nkeywords = [\"{name}\"]\n"
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn request_plugin_install_does_not_mutate_disk() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        write_keyword_bundle(root.path(), "supabase");
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let bundle = root.path().join(".codewhale/plugins/supabase/plugin.toml");
        let before = fs::read(&bundle).unwrap();
        let ctx = ToolContext::new(root.path())
            .with_plugin_registry(Arc::clone(&registry))
            .with_state_namespace("request-plugin-install-disk-test");

        let err = RequestPluginInstallTool
            .execute(
                json!({"name": "not-a-real-plugin", "reason": "guess"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("unknown"), "{err}");
        assert_eq!(fs::read(&bundle).unwrap(), before);

        let result = RequestPluginInstallTool
            .execute(
                json!({"name": "supabase", "reason": "needs hosted auth"}),
                &ctx,
            )
            .await
            .expect("known idle plugin");
        assert!(result.success);
        assert!(result.content.contains("/plugin trust supabase"));
        let meta = result.metadata.expect("metadata");
        assert_eq!(meta["installed"], json!(false));
        assert_eq!(meta["command"], json!("/plugin trust supabase"));
        assert_eq!(fs::read(&bundle).unwrap(), before);
    }

    /// Policy rule 4: one review request per session; a second call errors,
    /// and another session keeps its own budget.
    #[tokio::test]
    async fn request_plugin_install_is_once_per_session() {
        let _lock = lock_test_env();
        let root = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        write_keyword_bundle(root.path(), "supabase");
        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(root.path());
        let ctx = ToolContext::new(root.path())
            .with_plugin_registry(Arc::clone(&registry))
            .with_state_namespace("request-plugin-install-once-a");
        let input = json!({"name": "supabase", "reason": "needs hosted auth"});

        assert!(
            RequestPluginInstallTool
                .execute(input.clone(), &ctx)
                .await
                .is_ok()
        );
        let err = RequestPluginInstallTool
            .execute(input.clone(), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("once per session"), "{err}");

        let other = ToolContext::new(root.path())
            .with_plugin_registry(Arc::clone(&registry))
            .with_state_namespace("request-plugin-install-once-b");
        assert!(
            RequestPluginInstallTool
                .execute(input, &other)
                .await
                .is_ok()
        );
    }
}
