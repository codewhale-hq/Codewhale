use super::*;
use crate::core::authority::grants::{GrantEffect, GrantScope, GrantState};
use crate::core::authority::persistent_rules::persistent_rule_candidates;
use crate::tools::subagent::SubAgentManager;
use codewhale_execpolicy::{PermissionAction, Ruleset, RulesetLayer, ToolAskRule};

static PERMISSION_RULES_WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone)]
pub(crate) enum StandingRuleState {
    Claimed,
    Saved(Vec<ToolAskRule>),
}

#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct StandingRuleIneligible {
    pub reason: &'static str,
}

#[derive(Debug, thiserror::Error)]
#[error("permission rules are unavailable: {0}")]
pub struct PermissionRulesUnavailable(pub String);

#[derive(Debug, thiserror::Error)]
#[error("permission rules changed after they were listed; list them again")]
pub struct StalePermissionToken;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PermissionRuleView {
    pub index: usize,
    pub tool: String,
    pub command: Option<String>,
    pub command_exact: bool,
    pub path: Option<String>,
    pub workspace: Option<String>,
    pub action: PermissionAction,
    pub layer: RulesetLayer,
    pub removal_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PermissionRulesListing {
    Ok {
        path: String,
        exists: bool,
        rules: Vec<PermissionRuleView>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct StandingGrantView {
    pub grant_id: String,
    pub scope_kind: &'static str,
    pub thread_id: Option<String>,
    pub workspace_id: Option<String>,
    pub effect: GrantEffect,
    pub tool_name: String,
    pub label: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub uses: u32,
    pub state: GrantState,
    pub suspended_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StandingGrantsListing {
    Ok { grants: Vec<StandingGrantView> },
    Unavailable { reason: String },
}

impl RuntimeThreadManager {
    fn permissions_config_path(&self) -> Result<PathBuf, PermissionRulesUnavailable> {
        self.read_config()
            .loaded_config_path
            .clone()
            .ok_or_else(|| {
                PermissionRulesUnavailable(
                    "the Runtime loaded no user config, so no permissions.toml is in effect"
                        .to_string(),
                )
            })
    }

    fn require_owner_responder(&self, responder: Option<ApprovalResponder>) -> Result<()> {
        if self.owner_surface_attached && responder.is_some_and(ApprovalResponder::is_owner_surface)
        {
            Ok(())
        } else {
            Err(ApprovalOwnerRequired.into())
        }
    }

    pub async fn list_permission_rules(&self) -> PermissionRulesListing {
        let path = match self.permissions_config_path() {
            Ok(path) => path,
            Err(error) => return PermissionRulesListing::Unavailable { reason: error.0 },
        };
        let snapshot = tokio::task::spawn_blocking(move || {
            codewhale_config::load_permissions_snapshot(Some(path))
        })
        .await;
        let snapshot = match snapshot {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                return PermissionRulesListing::Unavailable {
                    reason: format!("{error:#}"),
                };
            }
            Err(error) => {
                return PermissionRulesListing::Unavailable {
                    reason: format!("permission read task failed: {error}"),
                };
            }
        };
        let rules = snapshot
            .rules()
            .iter()
            .enumerate()
            .map(|(index, rule)| PermissionRuleView {
                index,
                tool: rule.tool.clone(),
                command: rule.command.clone(),
                command_exact: rule.command_exact,
                path: rule.path.clone(),
                workspace: rule.workspace.clone(),
                action: rule.action,
                layer: RulesetLayer::User,
                removal_token: snapshot
                    .removal_token(index)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        PermissionRulesListing::Ok {
            path: snapshot.path().display().to_string(),
            exists: snapshot.file_exists(),
            rules,
        }
    }

    #[must_use]
    pub fn list_standing_grants(&self) -> StandingGrantsListing {
        match self.grants.standing_grants() {
            Ok(grants) => StandingGrantsListing::Ok {
                grants: grants
                    .into_iter()
                    .map(|grant| {
                        let (scope_kind, thread_id, workspace_id) = match grant.scope {
                            GrantScope::Thread { thread_id } => ("thread", Some(thread_id), None),
                            GrantScope::Workspace { workspace_id } => {
                                ("workspace", None, Some(workspace_id))
                            }
                        };
                        StandingGrantView {
                            grant_id: grant.grant_id,
                            scope_kind,
                            thread_id,
                            workspace_id,
                            effect: grant.effect,
                            tool_name: grant.display.tool_name,
                            label: grant.display.label,
                            summary: grant.display.redacted_summary,
                            created_at: grant.created.at,
                            last_used_at: grant.limits.last_used_at,
                            uses: grant.limits.uses,
                            state: grant.state,
                            suspended_reason: grant.suspended_reason,
                        }
                    })
                    .collect(),
            },
            Err(reason) => StandingGrantsListing::Unavailable { reason },
        }
    }

    pub async fn remove_permission_rule_by_token(
        &self,
        token: &str,
        responder: Option<ApprovalResponder>,
    ) -> Result<ToolAskRule> {
        self.require_owner_responder(responder)?;
        let path = self.permissions_config_path()?;
        let token = token.to_string();
        let _write = PERMISSION_RULES_WRITE.lock().await;
        let (removed, ruleset) = tokio::task::spawn_blocking(move || -> Result<_> {
            let snapshot = codewhale_config::load_permissions_snapshot(Some(path.clone()))
                .map_err(|error| PermissionRulesUnavailable(format!("{error:#}")))?;
            let index = (0..snapshot.rules().len())
                .find(|index| snapshot.removal_token(*index) == Some(token.as_str()))
                .ok_or(StalePermissionToken)?;
            let removed = codewhale_config::remove_permission_rule(Some(path), index, &token)
                .map_err(|error| -> anyhow::Error {
                    if error
                        .downcast_ref::<codewhale_config::StalePermissionSnapshot>()
                        .is_some()
                    {
                        StalePermissionToken.into()
                    } else {
                        PermissionRulesUnavailable(format!("{error:#}")).into()
                    }
                })?;
            let mut remaining = snapshot.permissions().clone();
            remaining.rules.remove(index);
            Ok((removed, remaining.ruleset()))
        })
        .await
        .context("permission removal task failed")??;
        self.apply_user_permission_ruleset(ruleset).await;
        Ok(removed)
    }

    pub async fn deliver_approval_with_standing_rule(
        &self,
        approval_id: &str,
        responder: Option<ApprovalResponder>,
    ) -> Result<Option<(bool, Vec<ToolAskRule>)>> {
        self.require_owner_responder(responder)?;
        let decision = ExternalApprovalDecision::Allow { remember: false };
        let lookup = {
            let pending = self.pending_approvals.lock();
            let Some(entry) = pending.get(approval_id) else {
                return Ok(None);
            };
            if entry.agent_settled.is_some() {
                return Err(StandingRuleIneligible {
                    reason: "an external agent's approval cannot create a standing rule",
                }
                .into());
            }
            match &entry.standing_rules {
                Some(StandingRuleState::Saved(rules)) => {
                    Err((rules.clone(), entry.publication.clone()))
                }
                _ => Ok((entry.thread_id.clone(), entry.request.clone())),
            }
        };
        let (thread_id, request) = match lookup {
            Ok(found) => found,
            Err((rules, publication)) => {
                let delivered = match self
                    .await_delivered_approval(approval_id, decision, publication, None)
                    .await
                {
                    Ok(delivered) => delivered,
                    Err(error) => {
                        tracing::warn!(
                            approval_id,
                            "standing rule re-delivery was not confirmed: {error:#}"
                        );
                        false
                    }
                };
                return Ok(Some((delivered, rules)));
            }
        };
        let rules = self.standing_allow_rules_for(&thread_id, &request)?;
        let path = self.permissions_config_path()?;
        {
            let mut pending = self.pending_approvals.lock();
            let Some(entry) = pending.get_mut(approval_id) else {
                return Ok(None);
            };
            if entry.standing_rules.is_some() || entry.proposed.is_some() || entry.sender.is_none()
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "approval already has a proposed answer",
                )
                .into());
            }
            entry.standing_rules = Some(StandingRuleState::Claimed);
        }
        let _claim = StandingRuleClaim {
            manager: self,
            approval_id,
        };
        {
            let _write = PERMISSION_RULES_WRITE.lock().await;
            let appended = rules.clone();
            let ruleset = tokio::task::spawn_blocking(move || -> Result<Ruleset> {
                let mut store = codewhale_config::ConfigStore::load(Some(path))
                    .map_err(|error| PermissionRulesUnavailable(format!("{error:#}")))?;
                store
                    .append_allow_rules(&appended)
                    .map_err(|error| PermissionRulesUnavailable(format!("{error:#}")))?;
                Ok(store.permissions().ruleset())
            })
            .await
            .context("permission write task failed")??;
            self.apply_user_permission_ruleset(ruleset).await;
        }
        if let Err(error) = self
            .emit_event(
                &thread_id,
                Some(&request.turn_id),
                None,
                "permission.rules_saved",
                json!({
                    "approval_id": approval_id,
                    "tool_call_id": request.tool_call_id,
                    "tool_name": request.tool_name,
                    "standing_rules": rules,
                }),
            )
            .await
        {
            tracing::warn!(thread_id = %thread_id, approval_id, "failed to record saved permission rules: {error:#}");
        }
        let (publication, agent_settled) = {
            let mut pending = self.pending_approvals.lock();
            let Some(entry) = pending.get_mut(approval_id) else {
                return Ok(Some((false, rules)));
            };
            if !matches!(entry.standing_rules, Some(StandingRuleState::Claimed)) {
                return Ok(Some((false, rules)));
            }
            entry.standing_rules = Some(StandingRuleState::Saved(rules.clone()));
            let Some(sender) = entry.sender.take() else {
                return Ok(Some((false, rules)));
            };
            entry.responder = responder;
            if sender.send(decision).is_err() {
                return Ok(Some((false, rules)));
            }
            entry.proposed = Some(decision);
            (entry.publication.clone(), entry.agent_settled.clone())
        };
        let delivered = match self
            .await_delivered_approval(approval_id, decision, publication, agent_settled)
            .await
        {
            Ok(delivered) => delivered,
            Err(error) => {
                tracing::warn!(
                    approval_id,
                    "standing rule saved but delivery was not confirmed: {error:#}"
                );
                false
            }
        };
        Ok(Some((delivered, rules)))
    }

    fn standing_allow_rules_for(
        &self,
        thread_id: &str,
        request: &PendingApprovalRequest,
    ) -> Result<Vec<ToolAskRule>> {
        if request
            .tool_call_id
            .as_deref()
            .is_some_and(SubAgentManager::is_child_approval_id)
        {
            return Err(StandingRuleIneligible {
                reason: "a sub-agent's approval cannot create a standing rule",
            }
            .into());
        }
        let Some(input) = request.input.as_ref() else {
            return Err(StandingRuleIneligible {
                reason: "this approval carries no captured tool input to build a rule from",
            }
            .into());
        };
        let thread = self
            .store
            .load_thread(thread_id)
            .with_context(|| format!("Thread not found: {thread_id}"))?;
        if thread.archived {
            return Err(StandingRuleIneligible {
                reason: "an archived thread cannot create a standing rule",
            }
            .into());
        }
        if thread.executor.is_some() {
            return Err(StandingRuleIneligible {
                reason: "an external agent's approval cannot create a standing rule",
            }
            .into());
        }
        let allow = standing_rule_candidates(
            request.tool_call_id.as_deref(),
            &request.tool_name,
            input,
            &request.description,
            &thread.workspace,
        );
        if allow.is_empty() {
            return Err(StandingRuleIneligible {
                reason: "only a safe shell command, or a write_file, edit_file or apply_patch inside the thread's workspace, can become a standing allow rule",
            }
            .into());
        }
        Ok(allow)
    }

    pub(super) async fn resync_user_permission_ruleset(&self) {
        let Ok(path) = self.permissions_config_path() else {
            return;
        };
        let _write = PERMISSION_RULES_WRITE.lock().await;
        let snapshot = tokio::task::spawn_blocking(move || {
            codewhale_config::load_permissions_snapshot(Some(path))
        })
        .await;
        match snapshot {
            Ok(Ok(snapshot)) => {
                self.apply_user_permission_ruleset(snapshot.permissions().ruleset())
                    .await;
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    "permission rules were not re-read after a config reload: {error:#}"
                );
            }
            Err(error) => {
                tracing::warn!("permission rules re-read task failed: {error}");
            }
        }
    }

    async fn apply_user_permission_ruleset(&self, ruleset: Ruleset) {
        self.read_config()
            .exec_policy_engine
            .clone()
            .set_ruleset(ruleset.clone());
        let engines: Vec<EngineHandle> = {
            let active = self.active.lock().await;
            active
                .engines
                .values()
                .map(|state| state.engine.clone())
                .collect()
        };
        for engine in engines {
            engine.replace_user_permission_ruleset(ruleset.clone());
        }
    }
}

#[must_use]
pub(super) fn standing_rule_candidates(
    tool_call_id: Option<&str>,
    tool_name: &str,
    input: &Value,
    description: &str,
    workspace: &Path,
) -> Vec<ToolAskRule> {
    if tool_call_id.is_some_and(SubAgentManager::is_child_approval_id) {
        return Vec::new();
    }
    persistent_rule_candidates(tool_name, input, description, workspace).allow
}

struct StandingRuleClaim<'a> {
    manager: &'a RuntimeThreadManager,
    approval_id: &'a str,
}

impl Drop for StandingRuleClaim<'_> {
    fn drop(&mut self) {
        if let Some(entry) = self
            .manager
            .pending_approvals
            .lock()
            .get_mut(self.approval_id)
            && matches!(entry.standing_rules, Some(StandingRuleState::Claimed))
        {
            entry.standing_rules = None;
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl RuntimeThreadManager {
    pub fn register_pending_tool_approval_for_thread_for_test(
        &self,
        thread_id: &str,
        tool_name: &str,
        input: Value,
    ) -> (String, oneshot::Receiver<ExternalApprovalDecision>) {
        self.register_pending_tool_call_approval_for_test(
            thread_id,
            &format!("call-{tool_name}"),
            tool_name,
            input,
        )
    }

    pub fn register_pending_tool_call_approval_for_test(
        &self,
        thread_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        input: Value,
    ) -> (String, oneshot::Receiver<ExternalApprovalDecision>) {
        self.register_pending_approval_with_policy(
            thread_id,
            PendingApprovalRequest {
                id: String::new(),
                turn_id: "test-turn".to_string(),
                tool_name: tool_name.to_string(),
                input: Some(input),
                description: format!("test approval for {tool_name}"),
                intent_summary: None,
                tool_call_id: Some(tool_call_id.to_string()),
                summary: None,
            },
            None,
            false,
        )
    }

    pub fn standing_rule_claimed_for_test(&self, approval_id: &str, claimed: bool) {
        if let Some(entry) = self.pending_approvals.lock().get_mut(approval_id) {
            entry.standing_rules = claimed.then_some(StandingRuleState::Claimed);
        }
    }
}
