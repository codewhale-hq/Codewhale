//! Programmable admission listeners reuse the native strict hook fold.
//! The host returns proposals, never tool or approval authority.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::{CoreRequest, HookCallPayload, HookEvaluateParams, HookVerdictWire};
use super::{HostAttachment, ManagerShared};
use crate::hooks::HookResult;

const HOOK_DEADLINE: Duration = Duration::from_secs(5);

impl HostAttachment {
    /// Only the reviewed owners this engine's own snapshot desires see its
    /// call. A shared host never broadcasts another workspace's arguments.
    pub async fn tool_before_hooks(&self, mut payload: HookCallPayload) -> Vec<HookResult> {
        let shared = &self.manager.shared;
        let desired = shared
            .attachments
            .lock()
            .expect("attachments lock")
            .get(&self.id)
            .map(|state| state.selection.clone())
            .unwrap_or_default();
        let hooks: Vec<_> = shared
            .registry
            .lock()
            .expect("registry lock")
            .live_hooks()
            .into_iter()
            .filter(|hook| {
                desired.includes(
                    &hook.owner.plugin_id,
                    &hook.content_hash,
                    hook.scope.as_ref(),
                )
            })
            .collect();
        let mut results = Vec::with_capacity(hooks.len());
        let batch_started = Instant::now();
        for hook in hooks {
            let started = Instant::now();
            let answer = async {
                let host = shared.live_host_for_hook(&hook).await?;
                let still_desired = desired.revision.is_some_and(|revision| {
                    shared.selection_current(
                        revision,
                        &hook.owner.plugin_id,
                        &hook.content_hash,
                        hook.scope.as_ref(),
                    )
                });
                if !still_desired {
                    return Err("extension hook was withdrawn".to_string());
                }
                let remaining_ms = HOOK_DEADLINE
                    .saturating_sub(batch_started.elapsed())
                    .as_millis() as u64;
                if remaining_ms == 0 {
                    return Err("extension hook batch deadline elapsed".to_string());
                }
                let value = host
                    .call(
                        CoreRequest::HookEvaluate(HookEvaluateParams {
                            handle: hook.handle,
                            event: hook.event.clone(),
                            payload: payload.clone(),
                            deadline_ms: remaining_ms,
                        }),
                        Some(hook.owner.plugin_id.clone()),
                    )
                    .await
                    .map_err(|_| "extension hook did not answer".to_string())?;
                // A late response cannot restore an owner revoked while the
                // callback waited. Revalidate its receipt and generation too.
                shared.live_host_for_hook(&hook).await?;
                let still_desired = desired.revision.is_some_and(|revision| {
                    shared.selection_current(
                        revision,
                        &hook.owner.plugin_id,
                        &hook.content_hash,
                        hook.scope.as_ref(),
                    )
                });
                if !still_desired {
                    return Err("extension hook was withdrawn".to_string());
                }
                let verdict: HookVerdictWire = serde_json::from_value(value)
                    .map_err(|_| "extension hook returned a malformed verdict".to_string())?;
                let stdout = verdict_stdout(verdict.clone())?;
                if let HookVerdictWire::Revise { input } = verdict {
                    payload.input = Value::Object(input);
                }
                Ok(stdout)
            }
            .await;
            let answered = answer.is_ok();
            let denies = answer.as_ref().is_ok_and(|stdout| {
                crate::hooks::parse_tool_call_before_stdout(stdout).decision
                    == Some(crate::hooks::ToolCallDecision::Deny)
            });
            results.push(HookResult {
                name: Some(format!("extension:{}:pre-execute", hook.plugin_name)),
                success: answered,
                background: false,
                strict: true,
                exit_code: answered.then_some(0),
                stdout: answer.unwrap_or_default(),
                stderr: String::new(),
                duration: started.elapsed(),
                error: (!answered).then(|| "extension hook returned no valid verdict".to_string()),
            });
            if !answered || denies {
                break;
            }
        }
        results
    }
}

impl ManagerShared {
    async fn live_host_for_hook(
        &self,
        hook: &super::registry::HookRegistration,
    ) -> Result<std::sync::Arc<super::supervisor::HostProcess>, String> {
        self.live_host(hook.tier, |registry| {
            registry
                .is_live_hook(hook.handle, &hook.owner)
                .then(|| hook.owner.clone())
                .ok_or_else(|| "extension hook is no longer registered".to_string())
        })
        .await
    }
}

fn verdict_stdout(verdict: HookVerdictWire) -> Result<String, String> {
    let revises = matches!(verdict, HookVerdictWire::Revise { .. });
    let value = match verdict {
        HookVerdictWire::Abstain => json!({}),
        HookVerdictWire::Deny { reason } => json!({"decision":"deny", "reason":reason}),
        HookVerdictWire::Ask { reason } => json!({"decision":"ask", "reason":reason}),
        HookVerdictWire::Annotate { text } => json!({"additionalContext":text}),
        HookVerdictWire::Revise { input } => json!({"updatedInput":Value::Object(input)}),
    };
    let stdout = serde_json::to_string(&value)
        .map_err(|_| "extension hook verdict is not JSON".to_string())?;
    // Reuse the existing validation and size limit. A rejected revision is
    // a strict no-verdict, never a silently ignored plugin proposal.
    if revises
        && crate::hooks::parse_tool_call_before_stdout(&stdout)
            .updated_input
            .is_none()
    {
        return Err("extension hook input revision exceeded the limit".to_string());
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::turn_loop::run_tool_call_before_hooks;
    use crate::extension_host::test_support::{FixturePlugins, node_for_tests};
    use codewhale_config::AppMode;
    use std::sync::Arc;

    #[tokio::test]
    async fn real_host_hook_timeout_and_owner_revocation_fail_closed_and_do_not_cross_workspaces() {
        let Some(node) = node_for_tests("real_host_hook_timeout_and_revocation") else {
            return;
        };
        let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(true);
        let fixture = FixturePlugins::new(&["hook-policy"]).await;
        let manager = Arc::new(super::super::ExtensionHostManager::new(
            super::super::ExtensionHostOptions {
                runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
                node_override: Some(node),
                root: Some(fixture.root.clone()),
                supervision: super::super::SupervisionOptions {
                    heartbeat_interval: Duration::from_secs(60),
                    ..Default::default()
                },
                ..Default::default()
            },
        ));
        let source = Arc::new(manager.attach(fixture.registry()));
        source.sync().await.unwrap();
        let timeout = run_tool_call_before_hooks(
            None,
            Some(&source),
            "read",
            "timeout",
            &json!({"path":"timeout.txt"}),
            AppMode::Agent,
            fixture.workspace(),
            "fixture",
        )
        .await;
        assert!(
            timeout
                .unwrap_err()
                .to_string()
                .contains("returned no verdict")
        );

        let other_workspace = tempfile::tempdir().unwrap();
        let other = manager.attach(Arc::new(codewhale_plugins::PluginRegistry::empty(
            other_workspace.path(),
        )));
        other.sync().await.unwrap();
        let unrelated = run_tool_call_before_hooks(
            None,
            Some(&other),
            "read",
            "other",
            &json!({"path":"blocked.txt"}),
            AppMode::Agent,
            other_workspace.path(),
            "fixture",
        )
        .await
        .unwrap();
        assert_eq!(
            unrelated,
            Default::default(),
            "a workspace does not receive another workspace's listener"
        );

        let sent = manager.host_requests_started().unwrap();
        let pending_source = Arc::clone(&source);
        let workspace = fixture.workspace().to_path_buf();
        let pending = tokio::spawn(async move {
            run_tool_call_before_hooks(
                None,
                Some(&pending_source),
                "read",
                "held",
                &json!({"path":"held.txt"}),
                AppMode::Agent,
                &workspace,
                "fixture",
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.host_requests_started() == Some(sent) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        source.set_plugins(fixture.disable("hook-policy"));
        source.sync().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .expect("revocation cancels a held hook")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("returned no verdict"));
        let after = run_tool_call_before_hooks(
            None,
            Some(&source),
            "read",
            "after",
            &json!({"path":"before.txt"}),
            AppMode::Agent,
            fixture.workspace(),
            "fixture",
        )
        .await
        .unwrap();
        assert_eq!(after, Default::default());
        manager.shutdown().await;
    }
}
