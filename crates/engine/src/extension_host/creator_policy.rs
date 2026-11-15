//! Core admission, preview, scope, and carried-decision enforcement for the
//! built-in creator `plugin_manager` tool (`CR-05a`).

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use codewhale_paths::{codewhale_home_override, user_home};
use codewhale_plugins::activation::{PolicyScope, extension_host_policy_enabled};
use codewhale_plugins::agent_plugin::resolve_manifest_path;
#[cfg(any(test, feature = "test-support"))]
use codewhale_plugins::host_services::TestOffer;
use codewhale_plugins::host_services::{
    AgentApproval, AgentPosture, HostService, HostServiceLine, HostServiceReview, PersonVia,
    PluginApprover, PluginCohort, ReviewPeer, cohort_of, offered,
};
use codewhale_plugins::install::{
    DEFAULT_MAX_SIZE_BYTES, PluginInstallSource, clean_dir_is_strictly_inside, dsh,
    has_regular_install_marker, installed_by_the_agent, path_is_within,
};
use codewhale_plugins::manifest::{PluginInventory, PluginManifest};
use codewhale_plugins::registry::PluginRegistry;
use codewhale_plugins::types::{LoadedPlugin, PluginScope};
use codewhale_sanitize::redact::redact_model_bound_secrets;
use codewhale_sanitize::sanitize::{is_bidi_control, sanitize_text, strip_ansi_into};
use codewhale_skill_install::install::{INSTALLED_FROM_MARKER, InstallSource, InstalledFromMarker};
use codewhale_util::path_identity::metadata_is_link_or_reparse;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::dsh_broker::BrokerEffect;
use crate::core::engine::LiveRuntimeAuthority;
use crate::tools::spec::{ToolContext, ToolError, ToolSpec};

/// Model-visible tool name owned by the tier-0 creator host module.
pub const PLUGIN_MANAGER_TOOL_NAME: &str = "plugin_manager";

/// Tier-0 owner id for the built-in creator host module.
pub const CREATOR_PLUGIN_ID: &str = "host:creator";

/// Maximum accepted character/byte bounds for validated creator arguments and summaries.
pub const MAX_ACTION_BYTES: usize = 32;
pub const MAX_TARGET_BYTES: usize = 512;
pub const MAX_VERSION_BYTES: usize = 128;
pub const MAX_REGISTRY_BYTES: usize = 512;
pub const MAX_APPROVED_BUILDS: usize = 16;
pub const MAX_TEXT_BYTES: usize = 256;
pub const MAX_DIAGNOSTIC_BYTES: usize = 2048;
pub const MAX_WARNING_ENTRIES: usize = 12;
pub const MAX_PREVIEW_TREE_BYTES: u64 = DEFAULT_MAX_SIZE_BYTES;
pub const MAX_PREVIEW_TREE_ENTRIES: usize = 4_096;
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(10);

const CREATOR_SCOPE_PREFIX: &str = "ext:host:creator@";

/// Return true when `input` requests a read-only `plugin_manager` list action.
#[must_use]
pub fn is_list_action(input: &Value) -> bool {
    matches!(
        input.get("action").and_then(Value::as_str).map(str::trim),
        Some("list_plugins" | "list_bundles" | "list_version_exemptions")
    )
}

/// Return true when `name` identifies the built-in `plugin_manager` tool.
#[must_use]
pub fn is_creator_plugin_manager_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(PLUGIN_MANAGER_TOOL_NAME)
}

/// Return true when `spec` identifies the built-in `host:creator` `plugin_manager` tool.
#[must_use]
pub fn is_creator_plugin_manager_spec(spec: &dyn ToolSpec) -> bool {
    spec.name() == PLUGIN_MANAGER_TOOL_NAME
        && spec
            .approval_scope()
            .is_some_and(|scope| scope.starts_with(CREATOR_SCOPE_PREFIX))
}

/// Return true when a call to `spec` with `input` targets a changing `plugin_manager` action.
#[must_use]
pub fn is_creator_changing_spec_call(spec: &dyn ToolSpec, input: &Value) -> bool {
    is_creator_plugin_manager_spec(spec) && !is_list_action(input)
}

/// Initial Rust-authored approval card fallback description before async preview completes.
#[must_use]
pub(super) fn initial_prepared_description(input: &Value) -> String {
    if is_list_action(input) {
        return "List installed plugins and bundle status (read-only)".to_string();
    }
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("change");
    let mut truncated = false;
    let clean_action = sanitize_inline(action, MAX_ACTION_BYTES, &mut truncated);
    match input.get("target").and_then(Value::as_str).map(str::trim) {
        Some(target) if !target.is_empty() => {
            let clean_target = sanitize_inline(target, MAX_TEXT_BYTES, &mut truncated);
            format!("Change plugin state ({clean_action}) for `{clean_target}`")
        }
        _ => format!("Change plugin state ({clean_action})"),
    }
}

/// Mutating action requested on `plugin_manager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorAction {
    Install,
    Remove,
    Enable,
    Disable,
}

impl CreatorAction {
    /// Wire method name handled by the `plugins` broker arm.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

/// Authority that admitted a `plugin_manager` change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorDecider {
    Person,
    Agent(AgentPosture),
}

/// Structured plugin preview summary passed to Auto-Review (`plugin_review`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreatorReviewSummary {
    pub action: String,
    pub name: String,
    pub target: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source: String,
    pub content_hash: String,
    pub capability_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsh_package: Option<String>,
    pub capabilities: Value,
    pub host_services: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Target lookup result against the discovered plugin registry.
pub(crate) enum TargetLookup<'a> {
    Found(&'a LoadedPlugin),
    Missing,
    Ambiguous {
        message: String,
        first: &'a LoadedPlugin,
    },
}

/// Paths held only by Rust for stripping host-private prefixes from messages.
pub(crate) struct ScrubContext<'a> {
    pub workspace: &'a Path,
    pub canon_workspace: Option<&'a Path>,
    pub plugin_root: Option<&'a Path>,
    pub user_plugins_dir: Option<&'a Path>,
    pub state_path: Option<&'a Path>,
}

/// Pre-gate preview of one valid `plugin_manager` change call.
#[derive(Debug, Clone)]
pub enum PlannedChangePreview {
    LocalInstall {
        raw_target: String,
        canon_source: PathBuf,
        is_dsh: bool,
        plugin_name: String,
        want_enabled: bool,
        pre_hash: String,
        capability_hash: String,
        expected_gate_target: String,
        person_review_lines: Vec<String>,
        agent_review_lines: Vec<String>,
        warnings: Vec<String>,
    },
    UnseenFetchInstall {
        raw_target: String,
        is_remote: bool,
        canon_source: Option<PathBuf>,
        remote_label: String,
        remote_host: String,
    },
    ExistingRemove {
        plugin_id: String,
        plugin_name: String,
        expected_gate_target: String,
    },
    ExistingToggle {
        enabled: bool,
        plugin_id: String,
        plugin_name: String,
        content_hash: String,
        capability_hash: String,
        expected_gate_target: String,
        person_review_lines: Vec<String>,
        person_review_token: Option<String>,
        agent_review_lines: Vec<String>,
        agent_review_token: Option<String>,
    },
}

/// Pre-approval plan carried on `ToolExecutionPlan::creator_gate`.
#[derive(Debug, Clone)]
pub struct CreatorGatePlan {
    pub action: CreatorAction,
    pub in_agent_scope: bool,
    pub unseen_network_fetch: bool,
    pub outside_scope_reason: Option<String>,
    pub card_description: String,
    pub auto_review_summary: Option<CreatorReviewSummary>,
    pub preview: PlannedChangePreview,
}

impl CreatorGatePlan {
    /// Construct the single-use carried decision attached to `ToolContext` on admission.
    pub fn build_carrier(
        &self,
        decider: CreatorDecider,
        admitted_authority: LiveRuntimeAuthority,
    ) -> Result<CreatorDecisionCarrier, ToolError> {
        if matches!(decider, CreatorDecider::Agent(_))
            && (!self.in_agent_scope || self.unseen_network_fetch)
        {
            return Err(ToolError::permission_denied(
                self.outside_scope_reason.clone().unwrap_or_else(|| {
                    "this plugin change is outside agent scope and requires a person's approval"
                        .to_string()
                }),
            ));
        }

        let is_agent = matches!(decider, CreatorDecider::Agent(_));
        let change = match &self.preview {
            PlannedChangePreview::LocalInstall {
                raw_target,
                canon_source,
                is_dsh,
                plugin_name,
                pre_hash,
                capability_hash,
                want_enabled,
                expected_gate_target,
                person_review_lines,
                agent_review_lines,
                warnings,
                ..
            } => AdmittedChangeKind::LocalInstall {
                raw_target: raw_target.clone(),
                canon_source: canon_source.clone(),
                is_dsh: *is_dsh,
                plugin_name: plugin_name.clone(),
                pre_hash: pre_hash.clone(),
                capability_hash: capability_hash.clone(),
                want_enabled: *want_enabled,
                expected_gate_target: expected_gate_target.clone(),
                shown_service_lines: if is_agent {
                    agent_review_lines.clone()
                } else {
                    person_review_lines.clone()
                },
                preview_warnings: warnings.clone(),
                remaining_writes: Arc::new(AtomicUsize::new(1)),
            },
            PlannedChangePreview::UnseenFetchInstall {
                raw_target,
                is_remote,
                canon_source,
                remote_label,
                remote_host,
            } => {
                let reach_slots = if *is_remote { 2 } else { 1 };
                AdmittedChangeKind::UnseenFetchInstall {
                    raw_target: raw_target.clone(),
                    is_remote: *is_remote,
                    canon_source: canon_source.clone(),
                    remote_label: remote_label.clone(),
                    remote_host: remote_host.clone(),
                    remaining_reach: Arc::new(AtomicUsize::new(reach_slots)),
                    used_reach: Arc::new(AtomicUsize::new(0)),
                    remaining_writes: Arc::new(AtomicUsize::new(1)),
                }
            }
            PlannedChangePreview::ExistingRemove {
                plugin_id,
                plugin_name,
                expected_gate_target,
            } => AdmittedChangeKind::Remove {
                plugin_id: plugin_id.clone(),
                plugin_name: plugin_name.clone(),
                expected_gate_target: expected_gate_target.clone(),
                remaining_writes: Arc::new(AtomicUsize::new(1)),
            },
            PlannedChangePreview::ExistingToggle {
                enabled,
                plugin_id,
                plugin_name,
                content_hash,
                capability_hash,
                expected_gate_target,
                person_review_lines,
                person_review_token,
                agent_review_lines,
                agent_review_token,
            } => AdmittedChangeKind::Toggle {
                enabled: *enabled,
                plugin_id: plugin_id.clone(),
                plugin_name: plugin_name.clone(),
                content_hash: content_hash.clone(),
                capability_hash: capability_hash.clone(),
                expected_gate_target: expected_gate_target.clone(),
                shown_service_lines: if is_agent {
                    agent_review_lines.clone()
                } else {
                    person_review_lines.clone()
                },
                shown_service_token: if is_agent {
                    agent_review_token.clone()
                } else {
                    person_review_token.clone()
                },
                remaining_writes: Arc::new(AtomicUsize::new(1)),
            },
        };

        Ok(CreatorDecisionCarrier {
            decider,
            admitted_authority,
            in_agent_scope: self.in_agent_scope,
            change,
        })
    }
}

/// Exact operation scope permitted by an admitted [`CreatorDecisionCarrier`].
#[derive(Debug, Clone)]
pub enum AdmittedChangeKind {
    LocalInstall {
        raw_target: String,
        canon_source: PathBuf,
        is_dsh: bool,
        plugin_name: String,
        pre_hash: String,
        capability_hash: String,
        want_enabled: bool,
        expected_gate_target: String,
        shown_service_lines: Vec<String>,
        preview_warnings: Vec<String>,
        remaining_writes: Arc<AtomicUsize>,
    },
    UnseenFetchInstall {
        raw_target: String,
        is_remote: bool,
        canon_source: Option<PathBuf>,
        remote_label: String,
        remote_host: String,
        remaining_reach: Arc<AtomicUsize>,
        used_reach: Arc<AtomicUsize>,
        remaining_writes: Arc<AtomicUsize>,
    },
    Remove {
        plugin_id: String,
        plugin_name: String,
        expected_gate_target: String,
        remaining_writes: Arc<AtomicUsize>,
    },
    Toggle {
        enabled: bool,
        plugin_id: String,
        plugin_name: String,
        content_hash: String,
        capability_hash: String,
        expected_gate_target: String,
        shown_service_lines: Vec<String>,
        shown_service_token: Option<String>,
        remaining_writes: Arc<AtomicUsize>,
    },
}

/// Carried admission placed by the Engine gate on `ToolExecutionState::creator_decision`.
#[derive(Debug, Clone)]
pub struct CreatorDecisionCarrier {
    pub decider: CreatorDecider,
    pub admitted_authority: LiveRuntimeAuthority,
    pub in_agent_scope: bool,
    pub change: AdmittedChangeKind,
}

impl CreatorDecisionCarrier {
    /// Return true when this admission covers unseen network bytes (Option A).
    #[must_use]
    pub fn is_unseen_fetch(&self) -> bool {
        matches!(self.change, AdmittedChangeKind::UnseenFetchInstall { .. })
    }

    /// Audit wire value identifying who admitted the action.
    #[must_use]
    pub const fn decider_wire(&self) -> &'static str {
        match self.decider {
            CreatorDecider::Person => "person",
            CreatorDecider::Agent(_) => "agent",
        }
    }

    /// Convert the carried decision into a [`PluginApprover`] for persistence, refusing Option A unseen-byte admissions.
    pub fn trust_approver(
        &self,
        canon_workspace: &Path,
        thread_id: String,
        turn_id: Option<String>,
        tool_call_id: Option<String>,
    ) -> Result<PluginApprover, String> {
        if self.is_unseen_fetch() {
            return Err(
                "unseen network bytes cannot receive a trust receipt before review".to_string(),
            );
        }
        match self.decider {
            CreatorDecider::Person => Ok(PluginApprover::person(PersonVia::ApprovalCard)),
            CreatorDecider::Agent(posture) => Ok(PluginApprover::Agent(AgentApproval {
                posture,
                workspace: canon_workspace.to_string_lossy().into_owned(),
                thread_id: Some(thread_id),
                turn_id,
                tool_call_id,
            })),
        }
    }
}

fn take_slot(counter: &AtomicUsize, label: &str) -> Result<(), String> {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        let Some(next) = current.checked_sub(1) else {
            return Err(format!(
                "plugin change decision has already spent its {label} slot"
            ));
        };
        match counter.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

fn is_write_target_with_suffix(target: &str, suffix: &str) -> bool {
    let Some(prefix) = target
        .strip_suffix(suffix)
        .and_then(|s| s.strip_suffix(' '))
    else {
        return false;
    };
    let Some((name, hash)) = prefix.rsplit_once(' ') else {
        return false;
    };
    !name.trim().is_empty()
        && hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Consume one broker `Write` or `Reach` act against the carried Engine admission.
pub(crate) fn admit_broker_act(
    tool: &ToolContext,
    live_authority: Option<&LiveRuntimeAuthority>,
    method: &str,
    effect: BrokerEffect,
    raw_target: Option<&str>,
) -> Result<(&'static str, &'static str), String> {
    if tool.execution.owner_agent_id.is_some() || tool.execution.child_host.is_some() {
        return Err("a sub-agent cannot change plugins".to_string());
    }
    let Some(carrier) = tool.execution.creator_decision.as_ref() else {
        return Err("plugin changes require an admitted engine decision".to_string());
    };
    let Some(live) = live_authority else {
        return Err("runtime authority is unavailable for this plugin change".to_string());
    };
    if live.mode() == AppMode::Plan || live.approval_mode() == ApprovalMode::Never {
        return Err("cannot change plugins in this mode".to_string());
    }
    if live.narrows(&carrier.admitted_authority) {
        return Err("cannot continue after session permissions were narrowed".to_string());
    }
    if matches!(carrier.decider, CreatorDecider::Agent(_))
        && (!carrier.in_agent_scope || carrier.is_unseen_fetch())
    {
        return Err("agent decision cannot authorize an outside-scope plugin change".to_string());
    }

    match &carrier.change {
        AdmittedChangeKind::LocalInstall {
            expected_gate_target,
            remaining_writes,
            ..
        } => {
            if method != "install" || effect != BrokerEffect::Write {
                return Err(
                    "plugin change decision only permits installing this bundle".to_string()
                );
            }
            if raw_target != Some(expected_gate_target.as_str()) {
                return Err("plugin bundle changed after approval".to_string());
            }
            take_slot(remaining_writes, "write")?;
        }
        AdmittedChangeKind::UnseenFetchInstall {
            raw_target: approved_spec,
            is_remote,
            canon_source,
            remote_label,
            remote_host,
            remaining_reach,
            used_reach,
            remaining_writes,
        } => {
            if carrier.decider != CreatorDecider::Person {
                return Err("unseen network fetches require a person's approval card".to_string());
            }
            if method != "install" {
                return Err("plugin install decision cannot run another action".to_string());
            }
            match effect {
                BrokerEffect::Reach => {
                    let allowed = if *is_remote {
                        raw_target == Some(format!("{remote_label} from {remote_host}").as_str())
                            || raw_target
                                == Some(
                                    format!(
                                        "{remote_label} npm dependencies from registry.npmjs.org"
                                    )
                                    .as_str(),
                                )
                    } else {
                        raw_target
                            == Some(
                                format!("{approved_spec} npm dependencies from registry.npmjs.org")
                                    .as_str(),
                            )
                            || canon_source.as_ref().is_some_and(|dir| {
                                raw_target
                                    == Some(
                                        format!(
                                            "{} npm dependencies from registry.npmjs.org",
                                            dir.display()
                                        )
                                        .as_str(),
                                    )
                            })
                    };
                    if !allowed {
                        return Err(
                            "network fetch target does not match the approved plugin spec"
                                .to_string(),
                        );
                    }
                    take_slot(remaining_reach, "reach")?;
                    used_reach.fetch_add(1, Ordering::SeqCst);
                }
                BrokerEffect::Write => {
                    if used_reach.load(Ordering::SeqCst) == 0 {
                        return Err(
                            "unseen-fetch install must perform its admitted fetch before writing"
                                .to_string(),
                        );
                    }
                    let allowed = if *is_remote {
                        raw_target.is_some_and(|t| is_write_target_with_suffix(t, approved_spec))
                    } else {
                        raw_target.is_some_and(|t| {
                            t == approved_spec
                                || is_write_target_with_suffix(t, approved_spec)
                                || canon_source.as_ref().is_some_and(|dir| {
                                    let dir_str = dir.to_string_lossy();
                                    t == dir_str.as_ref()
                                        || is_write_target_with_suffix(t, dir_str.as_ref())
                                })
                        })
                    };
                    if !allowed {
                        return Err(
                            "write target does not match the approved unseen-fetch install"
                                .to_string(),
                        );
                    }
                    take_slot(remaining_writes, "write")?;
                }
                BrokerEffect::Read => {}
            }
        }
        AdmittedChangeKind::Remove {
            expected_gate_target,
            remaining_writes,
            ..
        } => {
            if method != "remove" || effect != BrokerEffect::Write {
                return Err("plugin remove decision cannot run another action".to_string());
            }
            if raw_target != Some(expected_gate_target.as_str()) {
                return Err("plugin remove target does not match the approved plugin".to_string());
            }
            take_slot(remaining_writes, "write")?;
        }
        AdmittedChangeKind::Toggle {
            enabled,
            expected_gate_target,
            remaining_writes,
            ..
        } => {
            let expected_method = if *enabled { "enable" } else { "disable" };
            if method != expected_method || effect != BrokerEffect::Write {
                return Err(
                    "plugin toggle decision does not match the requested method".to_string()
                );
            }
            if raw_target != Some(expected_gate_target.as_str()) {
                return Err("plugin state changed after approval".to_string());
            }
            take_slot(remaining_writes, "write")?;
        }
    }

    let posture = match carrier.decider {
        CreatorDecider::Person => person_via_wire(PersonVia::ApprovalCard),
        CreatorDecider::Agent(posture) => agent_posture_wire(posture),
    };
    Ok((carrier.decider_wire(), posture))
}

struct ParsedChangeInput {
    action: CreatorAction,
    target: String,
    want_enabled: bool,
    registry_ignored: bool,
}

fn validate_control_free_str(raw: &str, field: &str, max_bytes: usize) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if trimmed.len() > max_bytes {
        return Err(format!("{field} is longer than {max_bytes} bytes"));
    }
    if trimmed
        .chars()
        .any(|ch| ch == '\0' || ch.is_control() || is_bidi_control(ch))
    {
        return Err(format!("{field} contains disallowed control characters"));
    }
    Ok(trimmed.to_string())
}

fn validate_optional_control_free_str(
    value: Option<&Value>,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(raw) = value.as_str() else {
        return Err(format!("{field} must be a string"));
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    validate_control_free_str(raw, field, max_bytes).map(Some)
}

fn parse_change_input(input: &Value) -> Result<ParsedChangeInput, ToolError> {
    let raw_action = input
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_input("plugin_manager requires a string `action`"))?;
    let clean_action = validate_control_free_str(raw_action, "action", MAX_ACTION_BYTES)
        .map_err(ToolError::invalid_input)?;
    let explicit_enabled = match input.get("enabled") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => {
            return Err(ToolError::invalid_input("`enabled` must be a boolean"));
        }
    };
    let action = match clean_action.as_str() {
        "install" | "install_bundle" => CreatorAction::Install,
        "remove" | "remove_bundle" => CreatorAction::Remove,
        "enable" => CreatorAction::Enable,
        "disable" => CreatorAction::Disable,
        "set_bundle" | "set_plugin" => match explicit_enabled {
            Some(true) => CreatorAction::Enable,
            Some(false) => CreatorAction::Disable,
            None => {
                return Err(ToolError::invalid_input(format!(
                    "`{clean_action}` requires a boolean `enabled`"
                )));
            }
        },
        "set_version_exemption" => {
            return Err(ToolError::invalid_input(
                "Codewhale does not exempt plugins from the host compatibility contract",
            ));
        }
        other => {
            return Err(ToolError::invalid_input(format!(
                "unsupported plugin_manager action `{other}`"
            )));
        }
    };

    let raw_target = input.get("target").and_then(Value::as_str).ok_or_else(|| {
        ToolError::invalid_input(format!(
            "`{}` requires a non-empty `target`",
            action.as_str()
        ))
    })?;
    let target = validate_control_free_str(raw_target, "target", MAX_TARGET_BYTES)
        .map_err(ToolError::invalid_input)?;

    let want_enabled = explicit_enabled.unwrap_or(true);

    let registry_ignored =
        validate_optional_control_free_str(input.get("registry"), "registry", MAX_REGISTRY_BYTES)
            .map_err(ToolError::invalid_input)?
            .is_some();

    let _ = validate_optional_control_free_str(
        input.get("runtimeVersion"),
        "runtimeVersion",
        MAX_VERSION_BYTES,
    )
    .map_err(ToolError::invalid_input)?;

    match input.get("acceptRisk") {
        None | Some(Value::Null) | Some(Value::Bool(_)) => {}
        Some(_) => {
            return Err(ToolError::invalid_input("`acceptRisk` must be a boolean"));
        }
    }

    if let Some(builds_val) = input
        .get("approvedBuilds")
        .or_else(|| input.get("approved_builds"))
        .filter(|v| !v.is_null())
    {
        let Some(arr) = builds_val.as_array() else {
            return Err(ToolError::invalid_input(
                "`approvedBuilds` must be an array of strings",
            ));
        };
        if arr.len() > MAX_APPROVED_BUILDS {
            return Err(ToolError::invalid_input(format!(
                "`approvedBuilds` accepts at most {MAX_APPROVED_BUILDS} entries"
            )));
        }
        for item in arr {
            let Some(text) = item.as_str() else {
                return Err(ToolError::invalid_input(
                    "`approvedBuilds` entries must be strings",
                ));
            };
            validate_control_free_str(text, "approvedBuilds entry", MAX_TEXT_BYTES)
                .map_err(ToolError::invalid_input)?;
        }
    }

    Ok(ParsedChangeInput {
        action,
        target,
        want_enabled,
        registry_ignored,
    })
}

/// Preview and scope check a mutating `plugin_manager` call before Auto-Review or a person card.
pub async fn plan_change_async(
    tool: &ToolContext,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<CreatorGatePlan, ToolError> {
    let parsed = parse_change_input(input)?;
    let Some(base_registry) = tool.plugin_registry.as_ref().cloned() else {
        return Err(ToolError::not_available(
            "plugin manager is not available in this session",
        ));
    };

    if parsed.action == CreatorAction::Install {
        let source = parse_install_target_syntax(&parsed.target)
            .map_err(|(_, msg)| ToolError::invalid_input(msg))?;
        if let PluginInstallSource::Remote(_) = source {
            let (remote_label, remote_host) = remote_target_display(&source, &parsed.target);
            let mut truncated = false;
            let safe_spec = sanitize_inline(&parsed.target, MAX_TEXT_BYTES, &mut truncated);
            let safe_host = sanitize_inline(&remote_host, MAX_TEXT_BYTES, &mut truncated);
            let card_description = format!(
                "Action: install\n\
                 Source: {safe_spec}\n\
                 Remote host: {safe_host}\n\
                 Outside agent scope: fetches unseen bytes over the network (bytes are never fetched or inspected before you confirm).\n\
                 Fetched bytes will land disabled and untrusted until you review and enable the plugin on the Plugins page or via `/plugin trust <name>`."
            );
            return Ok(CreatorGatePlan {
                action: CreatorAction::Install,
                in_agent_scope: false,
                unseen_network_fetch: true,
                outside_scope_reason: Some(format!(
                    "fetches unseen plugin bytes from `{safe_host}`"
                )),
                card_description,
                auto_review_summary: None,
                preview: PlannedChangePreview::UnseenFetchInstall {
                    raw_target: parsed.target,
                    is_remote: true,
                    canon_source: None,
                    remote_label,
                    remote_host,
                },
            });
        }
    }

    let workspace = tool.workspace.clone();
    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    #[cfg(any(test, feature = "test-support"))]
    let test_offered: Vec<HostService> = HostService::ALL
        .into_iter()
        .filter(|service| offered(*service))
        .collect();

    let blocking = spawn_blocking_policy_job(BlockingPolicyJob::PlanChange {
        parsed,
        workspace,
        base_registry,
        policy,
        #[cfg(any(test, feature = "test-support"))]
        env_scope,
        #[cfg(any(test, feature = "test-support"))]
        test_offered,
    });

    let joined = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            return Err(ToolError::Cancelled {
                message: "cancelled while inspecting plugin change".to_string(),
            });
        }
        res = tokio::time::timeout(PREVIEW_TIMEOUT, blocking) => res,
    };

    match joined {
        Ok(Ok(BlockingPolicyResult::Plan(result))) => *result,
        Ok(Ok(_)) => Err(ToolError::execution_failed(
            "unexpected plugin preview task result".to_string(),
        )),
        Ok(Err(join_err)) => Err(ToolError::execution_failed(format!(
            "plugin preview task failed: {join_err}"
        ))),
        Err(_) => Err(ToolError::execution_failed(
            "timed out inspecting plugin bundle for review".to_string(),
        )),
    }
}

fn scope_label(scope: PluginScope) -> &'static str {
    match scope {
        PluginScope::Builtin => "builtin",
        PluginScope::User => "user_from_project",
        PluginScope::Workspace => "workspace",
    }
}

fn format_capabilities_summary(inv: &PluginInventory) -> Vec<String> {
    let mut parts = Vec::new();
    if inv.skills > 0 {
        parts.push(format!("{} skill(s)", inv.skills));
    }
    if inv.mcp_servers > 0 {
        parts.push(format!(
            "{} MCP server(s) ({} stdio, {} remote)",
            inv.mcp_servers, inv.stdio_mcp_servers, inv.remote_mcp_servers
        ));
    }
    if inv.commands > 0 {
        parts.push(format!("{} command(s)", inv.commands));
    }
    if inv.agents > 0 {
        parts.push(format!("{} agent(s)", inv.agents));
    }
    if inv.hooks > 0 {
        parts.push(format!("{} hook(s)", inv.hooks));
    }
    if inv.lsp > 0 {
        parts.push(format!("{} LSP server(s)", inv.lsp));
    }
    if inv.native > 0 {
        parts.push(format!("{} native module(s)", inv.native));
    }
    if inv.providers > 0 {
        parts.push(format!("{} provider(s)", inv.providers));
    }
    if !inv.filesystem_roots.is_empty() {
        parts.push(format!("{} filesystem root(s)", inv.filesystem_roots.len()));
    }
    if !inv.network_hosts.is_empty() {
        parts.push(format!("{} network host(s)", inv.network_hosts.len()));
    }
    if inv.lifecycle_mutation {
        parts.push("lifecycle mutation".to_string());
    }
    if parts.is_empty() {
        parts.push("none declared".to_string());
    }
    parts
}

fn build_auto_review_summary(
    action: &str,
    name: &str,
    target: &str,
    scope: &str,
    version: Option<&str>,
    source: &str,
    content_hash: &str,
    capability_hash: &str,
    enabled: Option<bool>,
    dsh_package: Option<&str>,
    inv: &PluginInventory,
    host_services: &[String],
    warnings: &[String],
) -> CreatorReviewSummary {
    let mut truncated = false;
    CreatorReviewSummary {
        action: action.to_string(),
        name: name.to_string(),
        target: sanitize_inline(target, MAX_TEXT_BYTES, &mut truncated),
        scope: scope.to_string(),
        version: version.map(str::to_string),
        source: sanitize_inline(source, MAX_TEXT_BYTES, &mut truncated),
        content_hash: content_hash.to_string(),
        capability_hash: capability_hash.to_string(),
        enabled,
        dsh_package: dsh_package.map(str::to_string),
        capabilities: json!({
            "skills": inv.skills,
            "mcp_servers": inv.mcp_servers,
            "stdio_mcp_servers": inv.stdio_mcp_servers,
            "remote_mcp_servers": inv.remote_mcp_servers,
            "commands": inv.commands,
            "agents": inv.agents,
            "hooks": inv.hooks,
            "lsp": inv.lsp,
            "native": inv.native,
            "providers": inv.providers,
            "filesystem_roots": inv.filesystem_roots,
            "network_hosts": inv.network_hosts,
            "lifecycle_mutation": inv.lifecycle_mutation,
        }),
        host_services: host_services.to_vec(),
        warnings: warnings.to_vec(),
    }
}

fn format_local_install_card(
    name: &str,
    version: Option<&str>,
    want_enabled: bool,
    source_label: &str,
    content_hash: &str,
    capability_hash: &str,
    in_agent_scope: bool,
    outside_reason: Option<&str>,
    inventory: &PluginInventory,
    review_lines: &[String],
    warnings: &[String],
) -> String {
    let mut lines = Vec::new();
    let verb = if want_enabled {
        "install and enable"
    } else {
        "install (disabled)"
    };
    lines.push(format!("Action: {verb}"));
    match version {
        Some(v) => lines.push(format!("Plugin: {name} v{v}")),
        None => lines.push(format!("Plugin: {name}")),
    }
    lines.push(format!("Source: {source_label}"));
    lines.push(format!("Content hash: {content_hash}"));
    lines.push(format!("Capability hash: {capability_hash}"));
    if in_agent_scope {
        lines.push("Scope: inside this project".to_string());
    } else {
        lines.push(format!(
            "Outside agent scope: {}",
            outside_reason.unwrap_or("requires a person's approval")
        ));
    }
    if want_enabled {
        lines.push("If enabled, this user-level bundle applies for every project.".to_string());
    }
    lines.push(format!(
        "Capabilities: {}",
        format_capabilities_summary(inventory).join(", ")
    ));
    if !review_lines.is_empty() {
        lines.extend(review_lines.iter().cloned());
    }
    if !warnings.is_empty() {
        lines.push(format!("Warnings: {}", warnings.join("; ")));
    }
    lines.join("\n")
}

fn format_toggle_card(
    enabled: bool,
    name: &str,
    plugin_id: &str,
    version: Option<&str>,
    scope: PluginScope,
    content_hash: &str,
    capability_hash: &str,
    in_agent_scope: bool,
    outside_reason: Option<&str>,
    inventory: &PluginInventory,
    review_lines: &[String],
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "Action: {}",
        if enabled { "enable" } else { "disable" }
    ));
    match version {
        Some(v) => lines.push(format!("Plugin: {name} v{v} ({plugin_id})")),
        None => lines.push(format!("Plugin: {name} ({plugin_id})")),
    }
    lines.push(format!("Bundle scope: {}", scope.as_str()));
    lines.push(format!("Content hash: {content_hash}"));
    lines.push(format!("Capability hash: {capability_hash}"));
    if !in_agent_scope {
        lines.push(format!(
            "Outside agent scope: {}",
            outside_reason.unwrap_or("requires a person's approval")
        ));
    } else {
        lines.push("Scope: installed by agent from this project".to_string());
    }
    if enabled {
        lines.push(format!(
            "Capabilities: {}",
            format_capabilities_summary(inventory).join(", ")
        ));
        if !review_lines.is_empty() {
            lines.extend(review_lines.iter().cloned());
        }
    }
    lines.join("\n")
}

fn source_display_for_review(
    canon_src: &Path,
    canon_workspace: Option<&Path>,
    is_dsh: bool,
) -> String {
    let label = canon_workspace
        .and_then(|ws| canon_src.strip_prefix(ws).ok().map(Path::to_path_buf))
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|| canon_src.to_string_lossy().into_owned());
    if is_dsh {
        format!("dsh:{label}")
    } else {
        label
    }
}

fn source_display_for_card(
    raw_target: &str,
    canon_src: &Path,
    canon_workspace: Option<&Path>,
    is_dsh: bool,
    dsh_package: Option<&str>,
) -> String {
    let mut truncated = false;
    let rel_or_raw = canon_workspace
        .and_then(|ws| {
            canon_src
                .strip_prefix(ws)
                .ok()
                .map(Path::to_path_buf)
                .filter(|rel| !rel.as_os_str().is_empty())
        })
        .map(|rel| format!("project/{}", rel.to_string_lossy()))
        .unwrap_or_else(|| raw_target.to_string());
    let clean = sanitize_inline(&rel_or_raw, MAX_TEXT_BYTES, &mut truncated);
    match (is_dsh, dsh_package) {
        (true, Some(pkg)) => format!("{clean} (DSH package {pkg})"),
        (true, None) => format!("{clean} (DSH package)"),
        (false, _) => clean,
    }
}

fn path_has_dot_git_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(part) => part.to_string_lossy().eq_ignore_ascii_case(".git"),
        _ => false,
    })
}

enum BlockingPolicyJob {
    PlanChange {
        parsed: ParsedChangeInput,
        workspace: PathBuf,
        base_registry: Arc<PluginRegistry>,
        policy: bool,
        #[cfg(any(test, feature = "test-support"))]
        env_scope: Option<crate::test_support::EnvScopeTicket>,
        #[cfg(any(test, feature = "test-support"))]
        test_offered: Vec<HostService>,
    },
    EnsureNoLinks {
        root: PathBuf,
    },
    CheckSymlinkAncestors {
        path: PathBuf,
    },
    DirInUserPlugins {
        dir: PathBuf,
        user_plugins_dir: PathBuf,
    },
}

enum BlockingPolicyResult {
    Plan(Box<Result<CreatorGatePlan, ToolError>>),
    Unit(Result<(), String>),
    BoolIo(Result<bool, std::io::Error>),
    Bool(bool),
}

async fn run_blocking_policy_job(job: BlockingPolicyJob) -> Result<BlockingPolicyResult, String> {
    spawn_blocking_policy_job(job)
        .await
        .map_err(|_| "blocking plugin policy task failed".to_string())
}

fn spawn_blocking_policy_job(
    job: BlockingPolicyJob,
) -> tokio::task::JoinHandle<BlockingPolicyResult> {
    tokio::task::spawn_blocking(move || {
        fn plan_local_install_blocking(
            parsed: ParsedChangeInput,
            workspace: &Path,
            base_registry: &PluginRegistry,
        ) -> Result<CreatorGatePlan, ToolError> {
            let source = parse_install_target_syntax(&parsed.target)
                .map_err(|(_, msg)| ToolError::invalid_input(msg))?;
            let (raw_path, explicit_dsh) = match source {
                PluginInstallSource::LocalPath(dir) => (dir, false),
                PluginInstallSource::Dsh(dir) => (dir, true),
                PluginInstallSource::Remote(_) => {
                    return Err(ToolError::invalid_input("unexpected remote install source"));
                }
            };

            let fresh = base_registry.rediscover_for_workspace(workspace);
            let canon_ws = std::fs::canonicalize(workspace).ok();
            let canon_user_plugins = fresh
                .user_plugins_dir()
                .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));

            let src_meta = std::fs::metadata(&raw_path).map_err(|error| {
                let scrubbed = scrub_path_error(
                    &error.to_string(),
                    workspace,
                    canon_ws.as_deref(),
                    None,
                    fresh.user_plugins_dir(),
                    fresh.state_path(),
                );
                ToolError::invalid_input(format!("cannot inspect local install folder: {scrubbed}"))
            })?;
            if !src_meta.is_dir() {
                return Err(ToolError::invalid_input(
                    "local plugin install target must be a directory",
                ));
            }

            let canon_src = std::fs::canonicalize(&raw_path).map_err(|error| {
                let scrubbed = scrub_path_error(
                    &error.to_string(),
                    workspace,
                    canon_ws.as_deref(),
                    None,
                    fresh.user_plugins_dir(),
                    fresh.state_path(),
                );
                ToolError::invalid_input(format!("cannot resolve local install folder: {scrubbed}"))
            })?;

            scan_no_links_bounded(&canon_src, MAX_PREVIEW_TREE_BYTES, MAX_PREVIEW_TREE_ENTRIES)
                .map_err(ToolError::invalid_input)?;

            let has_native_manifest = clean_root_manifest_sync(&canon_src).is_some();
            let is_dsh = if explicit_dsh {
                true
            } else {
                match dsh::try_is_dsh_package(&canon_src) {
                    Ok(true) => true,
                    Ok(false) if has_native_manifest => false,
                    Ok(false) => {
                        return Err(ToolError::invalid_input(
                            "source directory has no plugin.json, .claude-plugin/plugin.json, kimi.plugin.json, plugin.toml, or DSH package.json",
                        ));
                    }
                    Err(error) => {
                        let scrubbed = scrub_path_error(
                            &error.to_string(),
                            workspace,
                            canon_ws.as_deref(),
                            None,
                            fresh.user_plugins_dir(),
                            fresh.state_path(),
                        );
                        return Err(ToolError::invalid_input(scrubbed));
                    }
                }
            };

            let scrub = ScrubContext {
                workspace: fresh.workspace(),
                canon_workspace: canon_ws.as_deref(),
                plugin_root: Some(&canon_src),
                user_plugins_dir: fresh.user_plugins_dir(),
                state_path: fresh.state_path(),
            };

            if is_dsh {
                let has_npm_deps =
                    dsh::has_unresolved_npm_dependencies(&canon_src).map_err(|err| {
                        ToolError::invalid_input(scrub_message(&err.to_string(), &scrub))
                    })?;
                if has_npm_deps {
                    let mut truncated = false;
                    let safe_target =
                        sanitize_inline(&parsed.target, MAX_TEXT_BYTES, &mut truncated);
                    let pkg_note = read_dsh_source_package_sync(&canon_src)
                        .map(|pkg| {
                            format!(
                                "\nPackage: {}",
                                sanitize_inline(&pkg, MAX_TEXT_BYTES, &mut truncated)
                            )
                        })
                        .unwrap_or_default();
                    let card_description = format!(
                        "Action: install\n\
                 Folder: {safe_target}{pkg_note}\n\
                 Remote host: registry.npmjs.org\n\
                 Outside agent scope: unresolved npm dependencies will fetch unseen bytes from registry.npmjs.org (no network fetch or conversion runs before you confirm).\n\
                 Fetched bytes will land disabled and untrusted until you review and enable the plugin on the Plugins page or via `/plugin trust <name>`."
                    );
                    return Ok(CreatorGatePlan {
                        action: CreatorAction::Install,
                        in_agent_scope: false,
                        unseen_network_fetch: true,
                        outside_scope_reason: Some(
                            "DSH bundle has unresolved npm dependencies from registry.npmjs.org"
                                .to_string(),
                        ),
                        card_description,
                        auto_review_summary: None,
                        preview: PlannedChangePreview::UnseenFetchInstall {
                            raw_target: parsed.target,
                            is_remote: false,
                            canon_source: Some(canon_src),
                            remote_label: safe_target,
                            remote_host: "registry.npmjs.org".to_string(),
                        },
                    });
                }
            }

            let (validated, dsh_package, extra_warnings) = if is_dsh {
                let (_scratch, bundle, conversion) =
                    dsh::convert_to_scratch(&canon_src).map_err(|err| {
                        ToolError::invalid_input(scrub_message(&err.to_string(), &scrub))
                    })?;
                let manifest_path = resolve_manifest_path(&bundle).ok_or_else(|| {
                    ToolError::invalid_input(
                        "converted DSH bundle has no supported plugin manifest",
                    )
                })?;
                let validated = PluginManifest::validate_from_path(&manifest_path)
                    .map_err(|err| ToolError::invalid_input(scrub_message(&err, &scrub)))?;
                let mut diag_warnings = Vec::new();
                for note in conversion.diagnostics {
                    diag_warnings.push(scrub_warning(&note, &scrub));
                }
                (validated, conversion.source_package, diag_warnings)
            } else {
                let manifest_path = clean_root_manifest_sync(&canon_src).ok_or_else(|| {
                    ToolError::invalid_input("source directory has no valid plugin manifest")
                })?;
                let validated = PluginManifest::validate_from_path(&manifest_path)
                    .map_err(|err| ToolError::invalid_input(scrub_message(&err, &scrub)))?;
                (validated, None, Vec::new())
            };

            let (in_agent_scope, outside_scope_reason) = check_local_install_scope(
                &raw_path,
                &canon_src,
                canon_ws.as_deref(),
                fresh.user_plugins_dir(),
                canon_user_plugins.as_deref(),
            );

            let mut truncated = false;
            let raw_name = validated.manifest.plugin.name.clone();
            let safe_name = sanitize_inline(&raw_name, MAX_VERSION_BYTES, &mut truncated);
            let version = non_empty_version(&validated.manifest.plugin.version)
                .map(|v| sanitize_inline(&v, MAX_VERSION_BYTES, &mut truncated));
            let safe_dsh_pkg = dsh_package
                .as_deref()
                .map(|p| sanitize_inline(p, MAX_TEXT_BYTES, &mut truncated));

            let mut warnings = Vec::new();
            for w in validated.warnings.into_iter().chain(extra_warnings) {
                if warnings.len() >= MAX_WARNING_ENTRIES {
                    break;
                }
                let cleaned = scrub_warning(&w, &scrub);
                if !cleaned.is_empty() && !warnings.contains(&cleaned) {
                    warnings.push(cleaned);
                }
            }
            if parsed.registry_ignored && warnings.len() < MAX_WARNING_ENTRIES {
                warnings.push(
                    "registry was ignored; Codewhale installs from the configured source"
                        .to_string(),
                );
            }

            let person_review = host_service_review_from_inventory(
                &fresh,
                "",
                &raw_name,
                &validated.content_hash,
                &validated.capability_hash,
                &validated.inventory,
                &PluginCohort::Reviewed,
            );
            let person_review_lines = person_review.text();

            let (agent_review_lines, auto_review_summary) = if in_agent_scope {
                let ws = canon_ws.as_deref().unwrap_or_else(|| fresh.workspace());
                let agent_cohort = PluginCohort::agent(ws);
                let agent_review = host_service_review_from_inventory(
                    &fresh,
                    "",
                    &raw_name,
                    &validated.content_hash,
                    &validated.capability_hash,
                    &validated.inventory,
                    &agent_cohort,
                );
                let agent_lines = agent_review.text();
                let summary = build_auto_review_summary(
                    "install",
                    &safe_name,
                    &parsed.target,
                    "local_project",
                    version.as_deref(),
                    &source_display_for_review(&canon_src, canon_ws.as_deref(), is_dsh),
                    &validated.content_hash,
                    &validated.capability_hash,
                    Some(parsed.want_enabled),
                    safe_dsh_pkg.as_deref(),
                    &validated.inventory,
                    &agent_lines,
                    &warnings,
                );
                (agent_lines, Some(summary))
            } else {
                (Vec::new(), None)
            };

            let source_label = source_display_for_card(
                &parsed.target,
                &canon_src,
                canon_ws.as_deref(),
                is_dsh,
                safe_dsh_pkg.as_deref(),
            );
            let card_description = format_local_install_card(
                &safe_name,
                version.as_deref(),
                parsed.want_enabled,
                &source_label,
                &validated.content_hash,
                &validated.capability_hash,
                in_agent_scope,
                outside_scope_reason.as_deref(),
                &validated.inventory,
                &person_review_lines,
                &warnings,
            );
            let expected_gate_target =
                format!("{raw_name} {} {}", validated.content_hash, parsed.target);

            Ok(CreatorGatePlan {
                action: CreatorAction::Install,
                in_agent_scope,
                unseen_network_fetch: false,
                outside_scope_reason,
                card_description,
                auto_review_summary,
                preview: PlannedChangePreview::LocalInstall {
                    raw_target: parsed.target,
                    canon_source: canon_src,
                    is_dsh,
                    plugin_name: raw_name,
                    want_enabled: parsed.want_enabled,
                    pre_hash: validated.content_hash,
                    capability_hash: validated.capability_hash,
                    expected_gate_target,
                    person_review_lines,
                    agent_review_lines,
                    warnings,
                },
            })
        }

        fn plan_existing_blocking(
            parsed: ParsedChangeInput,
            workspace: &Path,
            base_registry: &PluginRegistry,
        ) -> Result<CreatorGatePlan, ToolError> {
            let fresh = base_registry.rediscover_for_workspace(workspace);
            let canon_ws = std::fs::canonicalize(workspace).ok();
            let canon_user_plugins = fresh
                .user_plugins_dir()
                .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
            let mut truncated = false;
            let clean_target = sanitize_inline(&parsed.target, MAX_TEXT_BYTES, &mut truncated);

            let plugin = match resolve_plugin_by_target(&fresh, &parsed.target) {
                TargetLookup::Missing => {
                    return Err(ToolError::invalid_input(format!(
                        "Plugin bundle `{clean_target}` was not found"
                    )));
                }
                TargetLookup::Ambiguous { message, .. } => {
                    return Err(ToolError::invalid_input(message));
                }
                TargetLookup::Found(plugin) => plugin,
            };

            let raw_name = plugin.name().to_string();
            let safe_name = sanitize_inline(&raw_name, MAX_VERSION_BYTES, &mut truncated);
            let plugin_id = plugin.id.as_str().to_string();
            let version = non_empty_version(&plugin.manifest.plugin.version)
                .map(|v| sanitize_inline(&v, MAX_VERSION_BYTES, &mut truncated));

            if parsed.action == CreatorAction::Remove {
                match plugin.scope {
                    PluginScope::Builtin => {
                        return Err(ToolError::invalid_input(format!(
                            "Built-in plugin `{safe_name}` cannot be removed; use disable instead"
                        )));
                    }
                    PluginScope::Workspace => {
                        return Err(ToolError::invalid_input(format!(
                            "Project plugin `{safe_name}` is a workspace file and cannot be removed with plugin_manager"
                        )));
                    }
                    PluginScope::User => {
                        if !has_regular_install_marker(&plugin.canonical_root) {
                            return Err(ToolError::invalid_input(format!(
                                "Plugin `{safe_name}` was not installed via plugin install and cannot be removed here"
                            )));
                        }
                    }
                }
            }

            let (in_agent_scope, outside_scope_reason) = check_existing_scope(
                &fresh,
                plugin,
                canon_ws.as_deref(),
                canon_user_plugins.as_deref(),
            );

            if parsed.action == CreatorAction::Remove {
                let expected_gate_target = format!("{raw_name} {plugin_id}");
                let scope_note = if in_agent_scope {
                    "Scope: installed by agent from this project".to_string()
                } else {
                    format!(
                        "Outside agent scope: {}",
                        outside_scope_reason
                            .as_deref()
                            .unwrap_or("requires a person's approval")
                    )
                };
                let version_line = version
                    .as_deref()
                    .map(|v| format!("\nVersion: {v}"))
                    .unwrap_or_default();
                let card_description = format!(
                    "Action: remove\n\
             Plugin: {safe_name} ({plugin_id}){version_line}\n\
             Content hash: {}\n\
             {scope_note}",
                    plugin.content_hash
                );
                let auto_review_summary = if in_agent_scope {
                    Some(build_auto_review_summary(
                        "remove",
                        &safe_name,
                        &parsed.target,
                        "user_from_project",
                        version.as_deref(),
                        "installed bundle",
                        &plugin.content_hash,
                        &plugin.capability_hash,
                        None,
                        None,
                        &plugin.inventory,
                        &[],
                        &[],
                    ))
                } else {
                    None
                };
                return Ok(CreatorGatePlan {
                    action: CreatorAction::Remove,
                    in_agent_scope,
                    unseen_network_fetch: false,
                    outside_scope_reason,
                    card_description,
                    auto_review_summary,
                    preview: PlannedChangePreview::ExistingRemove {
                        plugin_id,
                        plugin_name: raw_name,
                        expected_gate_target,
                    },
                });
            }

            let enabled = parsed.action == CreatorAction::Enable;
            let expected_gate_target = if enabled {
                format!("{raw_name} {} {plugin_id}", plugin.content_hash)
            } else {
                format!("{raw_name} {plugin_id}")
            };

            let (person_review_lines, person_review_token, agent_review_lines, agent_review_token) =
                if enabled {
                    let person_approver = PluginApprover::person(PersonVia::ApprovalCard);
                    let person_review = fresh.host_service_review_for(&plugin_id, &person_approver);
                    let p_lines = person_review.as_ref().map(|r| r.text()).unwrap_or_default();
                    let p_token = person_review.map(|r| r.token);

                    let (a_lines, a_token) = if in_agent_scope {
                        let ws = canon_ws.as_deref().unwrap_or_else(|| fresh.workspace());
                        let agent_approver = PluginApprover::Agent(AgentApproval {
                            posture: AgentPosture::AutoReview,
                            workspace: ws.to_string_lossy().into_owned(),
                            thread_id: None,
                            turn_id: None,
                            tool_call_id: None,
                        });
                        let agent_review =
                            fresh.host_service_review_for(&plugin_id, &agent_approver);
                        (
                            agent_review.as_ref().map(|r| r.text()).unwrap_or_default(),
                            agent_review.map(|r| r.token),
                        )
                    } else {
                        (Vec::new(), None)
                    };
                    (p_lines, p_token, a_lines, a_token)
                } else {
                    (Vec::new(), None, Vec::new(), None)
                };

            let card_description = format_toggle_card(
                enabled,
                &safe_name,
                &plugin_id,
                version.as_deref(),
                plugin.scope,
                &plugin.content_hash,
                &plugin.capability_hash,
                in_agent_scope,
                outside_scope_reason.as_deref(),
                &plugin.inventory,
                &person_review_lines,
            );

            let auto_review_summary = if in_agent_scope {
                let lines = if enabled {
                    &agent_review_lines
                } else {
                    &[][..]
                };
                Some(build_auto_review_summary(
                    if enabled { "enable" } else { "disable" },
                    &safe_name,
                    &parsed.target,
                    scope_label(plugin.scope),
                    version.as_deref(),
                    "installed bundle",
                    &plugin.content_hash,
                    &plugin.capability_hash,
                    Some(enabled),
                    None,
                    &plugin.inventory,
                    lines,
                    &[],
                ))
            } else {
                None
            };

            Ok(CreatorGatePlan {
                action: parsed.action,
                in_agent_scope,
                unseen_network_fetch: false,
                outside_scope_reason,
                card_description,
                auto_review_summary,
                preview: PlannedChangePreview::ExistingToggle {
                    enabled,
                    plugin_id,
                    plugin_name: raw_name,
                    content_hash: plugin.content_hash.clone(),
                    capability_hash: plugin.capability_hash.clone(),
                    expected_gate_target,
                    person_review_lines,
                    person_review_token,
                    agent_review_lines,
                    agent_review_token,
                },
            })
        }

        fn check_local_install_scope(
            raw_path: &Path,
            canon_src: &Path,
            canon_ws: Option<&Path>,
            raw_user_plugins: Option<&Path>,
            canon_user_plugins: Option<&Path>,
        ) -> (bool, Option<String>) {
            let Some(canon_ws) = canon_ws else {
                return (
                    false,
                    Some("project directory could not be resolved".to_string()),
                );
            };
            if workspace_is_root_or_home_sync(canon_ws) {
                return (
                    false,
                    Some("project directory is your home folder or filesystem root".to_string()),
                );
            }
            if path_has_dot_git_component(canon_ws)
                || path_has_dot_git_component(raw_path)
                || path_has_dot_git_component(canon_src)
            {
                return (false, Some("path sits inside .git".to_string()));
            }
            if canon_src == canon_ws {
                return (
                    false,
                    Some("cannot install the project root itself".to_string()),
                );
            }
            if raw_path
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                return (
                    false,
                    Some("path uses parent-directory traversal".to_string()),
                );
            }
            if canon_user_plugins.is_some_and(|dir| path_is_within(dir, canon_src, true))
                || raw_user_plugins.is_some_and(|dir| {
                    std::fs::symlink_metadata(dir)
                        .ok()
                        .is_some_and(|m| metadata_is_link_or_reparse(&m))
                        || path_is_within(dir, raw_path, true)
                        || path_is_within(dir, canon_src, true)
                })
            {
                return (
                    false,
                    Some("path sits inside the installed plugins folder".to_string()),
                );
            }
            if !matches!(check_symlink_ancestors_sync(raw_path), Ok(false)) {
                return (false, Some("path traverses a symbolic link".to_string()));
            }
            if !clean_dir_is_strictly_inside(raw_path, canon_ws) {
                return (false, Some("folder is outside this project".to_string()));
            }
            (true, None)
        }

        fn check_existing_scope(
            registry: &PluginRegistry,
            plugin: &LoadedPlugin,
            canon_ws: Option<&Path>,
            canon_user_plugins: Option<&Path>,
        ) -> (bool, Option<String>) {
            if plugin.scope == PluginScope::Builtin {
                return (
                    false,
                    Some("built-in plugins always require a person's approval".to_string()),
                );
            }
            if registry.state_error().is_some() {
                return (
                    false,
                    Some("plugin state is fail-closed or unreadable".to_string()),
                );
            }
            let Some(canon_ws) = canon_ws else {
                return (
                    false,
                    Some("project directory could not be resolved".to_string()),
                );
            };
            if workspace_is_root_or_home_sync(canon_ws) || path_has_dot_git_component(canon_ws) {
                return (
                    false,
                    Some("this project root cannot establish agent ownership".to_string()),
                );
            }
            match plugin.approved_by.as_ref() {
                Some(PluginApprover::Person { .. }) => {
                    return (
                        false,
                        Some("this plugin was approved by a person".to_string()),
                    );
                }
                Some(PluginApprover::CommandLine) => {
                    return (
                        false,
                        Some("this plugin was approved from the command line".to_string()),
                    );
                }
                Some(PluginApprover::Agent(agent)) => {
                    if agent.posture == AgentPosture::CommandLine {
                        return (
                            false,
                            Some("this plugin was approved from the command line".to_string()),
                        );
                    }
                    if !agent_workspace_matches_sync(&agent.workspace, canon_ws) {
                        return (
                            false,
                            Some("this plugin was approved in another project".to_string()),
                        );
                    }
                }
                None => {
                    return (
                        false,
                        Some("this plugin has not been reviewed by this agent".to_string()),
                    );
                }
            }
            if !plugin.trusted() {
                return (
                    false,
                    Some(
                        "this plugin's review record does not match the files on disk".to_string(),
                    ),
                );
            }
            if !installed_by_agent_strict_sync(&plugin.canonical_root, canon_ws, canon_user_plugins)
            {
                return (
                    false,
                    Some(
                        "this plugin was not installed from a clean subfolder of this project"
                            .to_string(),
                    ),
                );
            }
            (true, None)
        }

        fn agent_workspace_matches_sync(recorded: &str, canon_ws: &Path) -> bool {
            let rec_path = Path::new(recorded);
            let rec_canon =
                std::fs::canonicalize(rec_path).unwrap_or_else(|_| rec_path.to_path_buf());
            path_is_within(canon_ws, &rec_canon, true) && path_is_within(&rec_canon, canon_ws, true)
        }

        fn workspace_is_root_or_home_sync(canon_ws: &Path) -> bool {
            if canon_ws.parent().is_none() {
                return true;
            }
            let equal_canon = |candidate: PathBuf| {
                let canon_cand = std::fs::canonicalize(&candidate).unwrap_or(candidate);
                path_is_within(&canon_cand, canon_ws, true)
                    && path_is_within(canon_ws, &canon_cand, true)
            };
            if let Some(home) = user_home()
                && equal_canon(home)
            {
                return true;
            }
            if let Ok(Some(home)) = codewhale_home_override()
                && equal_canon(home)
            {
                return true;
            }
            false
        }

        fn installed_by_agent_strict_sync(
            bundle_root: &Path,
            canon_ws: &Path,
            canon_user_plugins: Option<&Path>,
        ) -> bool {
            if !installed_by_the_agent(bundle_root, canon_ws) {
                return false;
            }
            let marker_path = bundle_root.join(INSTALLED_FROM_MARKER);
            let Ok(file) = codewhale_util::bundle_file::open_bundle_file(&marker_path) else {
                return false;
            };
            let mut body = String::new();
            if file.take(64 * 1024 + 1).read_to_string(&mut body).is_err() || body.len() > 64 * 1024
            {
                return false;
            }
            let Ok(marker) = serde_json::from_str::<InstalledFromMarker>(&body) else {
                return false;
            };
            let Ok(source) = PluginInstallSource::parse_syntax(&marker.spec) else {
                return false;
            };
            match source {
                PluginInstallSource::LocalPath(dir) | PluginInstallSource::Dsh(dir) => {
                    if path_has_dot_git_component(&dir) {
                        return false;
                    }
                    let Ok(canon_src) = std::fs::canonicalize(&dir) else {
                        return false;
                    };
                    if path_has_dot_git_component(&canon_src) {
                        return false;
                    }
                    if canon_user_plugins.is_some_and(|u| path_is_within(u, &canon_src, true)) {
                        return false;
                    }
                    if !matches!(check_symlink_ancestors_sync(&dir), Ok(false)) {
                        return false;
                    }
                    if matches!(dsh::has_unresolved_npm_dependencies(&canon_src), Ok(true)) {
                        return false;
                    }
                    true
                }
                PluginInstallSource::Remote(_) => false,
            }
        }

        fn clean_root_manifest_sync(dir: &Path) -> Option<PathBuf> {
            let manifest = resolve_manifest_path(dir)?;
            let mut cursor = manifest.parent();
            while let Some(current) = cursor {
                if let Ok(meta) = std::fs::symlink_metadata(current)
                    && metadata_is_link_or_reparse(&meta)
                {
                    return None;
                }
                if current == dir {
                    break;
                }
                cursor = current.parent();
            }
            let meta = std::fs::symlink_metadata(&manifest).ok()?;
            (!metadata_is_link_or_reparse(&meta) && meta.is_file()).then_some(manifest)
        }

        fn scan_no_links_bounded(
            root: &Path,
            max_bytes: u64,
            max_entries: usize,
        ) -> Result<(), String> {
            let root_meta = std::fs::symlink_metadata(root)
                .map_err(|err| format!("failed to inspect install source: {err}"))?;
            if metadata_is_link_or_reparse(&root_meta) || root_meta.file_type().is_symlink() {
                return Err("plugin bundles cannot contain symbolic links".to_string());
            }
            if !root_meta.is_dir() {
                return Err("not-bundle: install source must be a directory".to_string());
            }
            let mut entries = 0usize;
            let mut total_bytes = 0u64;
            let mut stack = vec![root.to_path_buf()];
            while let Some(dir) = stack.pop() {
                let dir_entries = std::fs::read_dir(&dir)
                    .map_err(|err| format!("failed to inspect bundle: {err}"))?;
                for entry in dir_entries {
                    let entry =
                        entry.map_err(|err| format!("failed to read bundle entry: {err}"))?;
                    entries = entries.saturating_add(1);
                    if entries > max_entries {
                        return Err(format!("plugin bundle has more than {max_entries} entries"));
                    }
                    let path = entry.path();
                    let meta = std::fs::symlink_metadata(&path)
                        .map_err(|err| format!("failed to inspect entry: {err}"))?;
                    if metadata_is_link_or_reparse(&meta) {
                        return Err("plugin bundles cannot contain symbolic links".to_string());
                    }
                    if meta.is_dir() {
                        stack.push(path);
                    } else if meta.is_file() {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt as _;
                            if meta.nlink() != 1 {
                                return Err("plugin bundles cannot contain hard links".to_string());
                            }
                        }
                        total_bytes = total_bytes.saturating_add(meta.len());
                        if total_bytes > max_bytes {
                            return Err(format!(
                                "plugin bundle exceeds the {} MiB size limit",
                                max_bytes / (1024 * 1024)
                            ));
                        }
                    }
                }
            }
            Ok(())
        }

        fn check_symlink_ancestors_sync(path: &Path) -> Result<bool, std::io::Error> {
            let mut prefix = PathBuf::new();
            for component in path.components() {
                prefix.push(component);
                if matches!(
                    component,
                    Component::Prefix(_) | Component::RootDir | Component::CurDir
                ) {
                    continue;
                }
                if is_macos_system_root_link(&prefix) {
                    continue;
                }
                let metadata = std::fs::symlink_metadata(&prefix)?;
                if metadata_is_link_or_reparse(&metadata) {
                    return Ok(true);
                }
            }
            Ok(false)
        }

        fn read_dsh_source_package_sync(root: &Path) -> Option<String> {
            let pkg = root.join("package.json");
            let meta = std::fs::symlink_metadata(&pkg).ok()?;
            if metadata_is_link_or_reparse(&meta) || !meta.is_file() {
                return None;
            }
            let file = codewhale_util::bundle_file::open_bundle_file(&pkg).ok()?;
            let mut raw = String::new();
            file.take(1024 * 1024).read_to_string(&mut raw).ok()?;
            let json: Value = serde_json::from_str(&raw).ok()?;
            let name = json.get("name")?.as_str()?.trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        }

        match job {
            BlockingPolicyJob::PlanChange {
                parsed,
                workspace,
                base_registry,
                policy,
                #[cfg(any(test, feature = "test-support"))]
                env_scope,
                #[cfg(any(test, feature = "test-support"))]
                test_offered,
            } => {
                #[cfg(any(test, feature = "test-support"))]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                let _policy_scope = PolicyScope::propagate(policy);
                #[cfg(any(test, feature = "test-support"))]
                let _offer = TestOffer::only(&test_offered);
                let res = match parsed.action {
                    CreatorAction::Install => {
                        plan_local_install_blocking(parsed, &workspace, &base_registry)
                    }
                    CreatorAction::Remove | CreatorAction::Enable | CreatorAction::Disable => {
                        plan_existing_blocking(parsed, &workspace, &base_registry)
                    }
                };
                BlockingPolicyResult::Plan(Box::new(res))
            }
            BlockingPolicyJob::EnsureNoLinks { root } => BlockingPolicyResult::Unit(
                scan_no_links_bounded(&root, MAX_PREVIEW_TREE_BYTES, MAX_PREVIEW_TREE_ENTRIES),
            ),
            BlockingPolicyJob::CheckSymlinkAncestors { path } => {
                BlockingPolicyResult::BoolIo(check_symlink_ancestors_sync(&path))
            }
            BlockingPolicyJob::DirInUserPlugins {
                dir,
                user_plugins_dir,
            } => {
                let inside = std::fs::canonicalize(&dir).ok().is_some_and(|canon_dir| {
                    let canon_user =
                        std::fs::canonicalize(&user_plugins_dir).unwrap_or(user_plugins_dir);
                    path_is_within(&canon_user, &canon_dir, true)
                });
                BlockingPolicyResult::Bool(inside)
            }
        }
    })
}

fn scrub_path_error(
    raw: &str,
    workspace: &Path,
    canon_workspace: Option<&Path>,
    plugin_root: Option<&Path>,
    user_plugins_dir: Option<&Path>,
    state_path: Option<&Path>,
) -> String {
    let scrub = ScrubContext {
        workspace,
        canon_workspace,
        plugin_root,
        user_plugins_dir,
        state_path,
    };
    scrub_message(raw, &scrub)
}

/// Build a [`HostServiceReview`] for an uninstalled bundle using the current discovered peer plugins.
#[must_use]
pub fn host_service_review_from_inventory(
    registry: &PluginRegistry,
    plugin_id: &str,
    plugin_name: &str,
    content_hash: &str,
    capability_hash: &str,
    inventory: &PluginInventory,
    cohort: &PluginCohort,
) -> HostServiceReview {
    let mut peers: Vec<&LoadedPlugin> = registry
        .list()
        .into_iter()
        .filter(|peer| peer.id.as_str() != plugin_id && peer.name() != plugin_name)
        .collect();
    peers.sort_by(|left, right| left.id.cmp(&right.id));

    let collect_peers = |holds: bool, service: HostService| -> Vec<ReviewPeer> {
        peers
            .iter()
            .filter(|peer| peer.trusted())
            .filter(|peer| cohort_of(peer.approved_by.as_ref()) == *cohort)
            .filter(|peer| {
                if holds {
                    peer.host_services.contains(&service)
                } else {
                    peer.enabled
                        && peer.inventory.native > 0
                        && !peer.host_services.contains(&service)
                }
            })
            .map(|peer| ReviewPeer {
                id: peer.id.as_str().to_string(),
                name: peer.name().to_string(),
            })
            .collect()
    };

    let lines = inventory
        .host_services
        .iter()
        .map(|service| HostServiceLine {
            service: *service,
            offered: offered(*service),
            shared_with: collect_peers(true, *service),
            withheld_by: collect_peers(false, *service),
        })
        .collect();

    HostServiceReview::new(plugin_id, content_hash, capability_hash, cohort, lines)
}

/// Return true on macOS for system root symlinks `/var`, `/tmp`, `/etc`.
#[must_use]
pub(crate) fn is_macos_system_root_link(prefix: &Path) -> bool {
    cfg!(target_os = "macos")
        && (prefix == Path::new("/var")
            || prefix == Path::new("/tmp")
            || prefix == Path::new("/etc"))
}

/// Read the converted DSH source npm package name from `CONVERSION.json`, if present.
pub(crate) fn read_dsh_source_package(root: &Path) -> Option<String> {
    let file = codewhale_util::bundle_file::open_bundle_file(&root.join("CONVERSION.json")).ok()?;
    let mut body = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut body).ok()?;
    if body.len() > 64 * 1024 {
        return None;
    }
    let parsed: Value = serde_json::from_str(&body).ok()?;
    parsed
        .get("source")
        .and_then(|source| source.get("package"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Inspect tree links within bounds in a blocking worker.
pub(crate) async fn ensure_no_links_in_tree(root: &Path) -> Result<(), String> {
    let root = root.to_path_buf();
    match run_blocking_policy_job(BlockingPolicyJob::EnsureNoLinks { root }).await {
        Ok(BlockingPolicyResult::Unit(res)) => res,
        Ok(_) => Err("unexpected blocking job result".to_string()),
        Err(err) => Err(err),
    }
}

/// Check in a blocking worker whether any path component up to `path` is a symlink or reparse point.
pub(crate) async fn has_symlink_or_reparse_ancestor_or_target(
    path: &Path,
) -> Result<bool, std::io::Error> {
    let path = path.to_path_buf();
    match run_blocking_policy_job(BlockingPolicyJob::CheckSymlinkAncestors { path }).await {
        Ok(BlockingPolicyResult::BoolIo(res)) => res,
        Ok(_) => Err(std::io::Error::other("unexpected blocking job result")),
        Err(err) => Err(std::io::Error::other(err)),
    }
}

/// Check in a blocking worker whether `dir` lives inside `user_plugins_dir`.
pub(crate) async fn dir_is_in_user_plugins(dir: &Path, user_plugins_dir: Option<&Path>) -> bool {
    let Some(user_plugins_dir) = user_plugins_dir.map(Path::to_path_buf) else {
        return false;
    };
    let dir = dir.to_path_buf();
    matches!(
        run_blocking_policy_job(BlockingPolicyJob::DirInUserPlugins {
            dir,
            user_plugins_dir,
        })
        .await,
        Ok(BlockingPolicyResult::Bool(true))
    )
}

/// Return true for `http://127.0.0.1:<port>...` loopback specs during tests.
#[must_use]
pub(crate) fn is_test_loopback_http_url(spec: &str) -> bool {
    cfg!(any(test, feature = "test-support"))
        && spec.strip_prefix("http://127.0.0.1:").is_some_and(|rest| {
            rest.split('/')
                .next()
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Parse an install spec strictly from its string syntax without hitting the network or filesystem.
pub(crate) fn parse_install_target_syntax(
    spec: &str,
) -> Result<PluginInstallSource, (&'static str, String)> {
    if spec.starts_with("http://") && !is_test_loopback_http_url(spec) {
        return Err((
            "invalid-spec",
            "plugin archive URLs must use https://".to_string(),
        ));
    }
    let parsed = PluginInstallSource::parse_syntax(spec)
        .map_err(|error| ("invalid-spec", format!("invalid install spec: {error}")))?;
    match &parsed {
        PluginInstallSource::LocalPath(path) | PluginInstallSource::Dsh(path) => {
            if !path.is_absolute() {
                return Err((
                    "invalid-spec",
                    "local or DSH install target must be an absolute directory path, or a valid npm/git/github/https spec".to_string(),
                ));
            }
        }
        PluginInstallSource::Remote(remote) => {
            if let codewhale_skill_install::install::InstallSource::DirectUrl(url) = remote
                && !url.starts_with("https://")
                && !is_test_loopback_http_url(url)
            {
                return Err((
                    "invalid-spec",
                    "plugin archive URLs must use https://".to_string(),
                ));
            }
        }
    }
    Ok(parsed)
}

/// Extract display label and host name for a remote [`PluginInstallSource`].
#[must_use]
pub(crate) fn remote_target_display(source: &PluginInstallSource, raw: &str) -> (String, String) {
    match source {
        PluginInstallSource::Remote(InstallSource::GitHubRepo(repo)) => {
            (repo.clone(), "github.com".to_string())
        }
        PluginInstallSource::Remote(InstallSource::DirectUrl(url)) => {
            let host = reqwest::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
                .unwrap_or_else(|| "unknown-host".to_string());
            (raw.to_string(), host)
        }
        _ => (raw.to_string(), "unknown-host".to_string()),
    }
}

/// Resolve an exact plugin id, plugin name, or converted DSH source package in `registry`.
#[must_use]
pub(crate) fn resolve_plugin_by_target<'a>(
    registry: &'a PluginRegistry,
    target: &str,
) -> TargetLookup<'a> {
    if let Some(plugin) = registry.get(target) {
        return TargetLookup::Found(plugin);
    }
    let matches: Vec<&LoadedPlugin> = registry
        .list()
        .into_iter()
        .filter(|plugin| {
            read_dsh_source_package(&plugin.canonical_root)
                .as_deref()
                .is_some_and(|pkg| pkg == target)
        })
        .collect();
    match matches.len() {
        0 => TargetLookup::Missing,
        1 => TargetLookup::Found(matches[0]),
        _ => {
            let names: Vec<_> = matches.iter().map(|plugin| plugin.name()).collect();
            TargetLookup::Ambiguous {
                message: format!(
                    "target `{target}` matches multiple installed plugins ({}); specify the plugin name or id",
                    names.join(", ")
                ),
                first: matches[0],
            }
        }
    }
}

/// Return trimmed manifest version if non-empty.
#[must_use]
pub(crate) fn non_empty_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Wire string for [`PersonVia`].
#[must_use]
pub(crate) const fn person_via_wire(via: PersonVia) -> &'static str {
    match via {
        PersonVia::Terminal => "terminal",
        PersonVia::App => "app",
        PersonVia::ApprovalCard => "approval_card",
    }
}

/// Wire string for [`AgentPosture`].
#[must_use]
pub(crate) const fn agent_posture_wire(posture: AgentPosture) -> &'static str {
    match posture {
        AgentPosture::FullAccess => "full_access",
        AgentPosture::AutoReview => "auto_review",
        AgentPosture::CommandLine => "command_line",
    }
}

/// Remove or scrub `host`-private prefixes cleanly for model-visible diagnostics.
pub(crate) fn scrub_message(raw: &str, scrub: &ScrubContext<'_>) -> String {
    let mut text = raw.to_string();
    if let Some(root) = scrub.plugin_root {
        text = strip_path_prefix_clean(&text, root);
    }
    if let Some(user_dir) = scrub.user_plugins_dir {
        text = strip_path_with_replacement(&text, user_dir, "<plugins>");
    }
    if let Some(state_path) = scrub.state_path {
        text = strip_path_with_replacement(&text, state_path, "plugin-state.json");
        if let Some(parent) = state_path.parent() {
            text = strip_path_prefix_clean(&text, parent);
        }
    }
    text = strip_path_prefix_clean(&text, scrub.workspace);
    if let Some(canon_ws) = scrub.canon_workspace {
        text = strip_path_prefix_clean(&text, canon_ws);
    }
    text = scrub_outside_paths(&text);
    let cleaned = clean_diagnostic_fragment(&text);
    let mut truncated = false;
    cap_text(cleaned, MAX_DIAGNOSTIC_BYTES, &mut truncated)
}

/// Scrub and cap one plugin warning line.
pub(crate) fn scrub_warning(raw: &str, scrub: &ScrubContext<'_>) -> String {
    let mut text = raw.to_string();
    if let Some(root) = scrub.plugin_root {
        text = strip_path_prefix_clean(&text, root);
    }
    if let Some(user_dir) = scrub.user_plugins_dir {
        text = strip_path_with_replacement(&text, user_dir, "<plugins>");
    }
    if let Some(state_path) = scrub.state_path {
        text = strip_path_with_replacement(&text, state_path, "plugin-state.json");
        if let Some(parent) = state_path.parent() {
            text = strip_path_prefix_clean(&text, parent);
        }
    }
    text = strip_path_prefix_clean(&text, scrub.workspace);
    if let Some(canon_ws) = scrub.canon_workspace {
        text = strip_path_prefix_clean(&text, canon_ws);
    }
    let cleaned = clean_diagnostic_fragment(&text);
    let mut truncated = false;
    cap_text(cleaned, MAX_DIAGNOSTIC_BYTES, &mut truncated)
}

/// Strip ANSI, outside paths, control characters, bidi controls, and model-bound secret tokens from `raw`.
#[must_use]
pub(crate) fn clean_diagnostic_fragment(raw: &str) -> String {
    let mut no_ansi = String::with_capacity(raw.len());
    strip_ansi_into(raw, &mut no_ansi);
    let paths_scrubbed = scrub_outside_paths(&no_ansi);
    let redacted = redact_model_bound_secrets(&sanitize_text(&paths_scrubbed));
    collapse_controls(&redacted)
}

/// Clean and bound one inline display string.
pub(crate) fn sanitize_inline(raw: &str, max_bytes: usize, truncated: &mut bool) -> String {
    let mut no_ansi = String::with_capacity(raw.len());
    strip_ansi_into(raw, &mut no_ansi);
    let redacted = redact_model_bound_secrets(&sanitize_text(&no_ansi));
    cap_text(collapse_controls(&redacted), max_bytes, truncated)
}

/// Collapse control, newline, and Unicode bidi-control runes into single spaces.
#[must_use]
pub(crate) fn collapse_controls(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_space = false;
    for ch in raw.chars() {
        if matches!(ch, '\r' | '\n' | '\t') {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        if ch.is_control() || is_bidi_control(ch) {
            continue;
        }
        last_was_space = ch == ' ';
        out.push(ch);
    }
    out.trim().to_string()
}

/// Truncate `cleaned` on a valid UTF-8 boundary to at most `max_bytes`.
pub(crate) fn cap_text(mut cleaned: String, max_bytes: usize, truncated: &mut bool) -> String {
    if cleaned.len() <= max_bytes {
        return cleaned;
    }
    *truncated = true;
    const ELLIPSIS: char = '…';
    let ellipsis_len = ELLIPSIS.len_utf8();
    if max_bytes < ellipsis_len {
        return String::new();
    }
    let mut cut = max_bytes - ellipsis_len;
    while cut > 0 && !cleaned.is_char_boundary(cut) {
        cut -= 1;
    }
    cleaned.truncate(cut);
    cleaned.push(ELLIPSIS);
    cleaned
}

pub(crate) fn strip_path_with_replacement(text: &str, target: &Path, replacement: &str) -> String {
    let raw = target.to_string_lossy();
    if raw.is_empty() {
        return text.to_string();
    }
    let mut out = text.replace(raw.as_ref(), replacement);
    if let Some(unprefixed) = raw.strip_prefix("/private/") {
        out = out.replace(&format!("/{unprefixed}"), replacement);
    }
    if let Some(unprefixed) = raw.strip_prefix(r"\\?\") {
        out = out.replace(unprefixed, replacement);
    }
    out
}

pub(crate) fn strip_path_prefix_clean(text: &str, root: &Path) -> String {
    let raw = root.to_string_lossy();
    if raw.is_empty() {
        return text.to_string();
    }
    let mut out = strip_single_root(text, raw.as_ref());
    if let Some(unprefixed) = raw.strip_prefix("/private/") {
        out = strip_single_root(&out, &format!("/{unprefixed}"));
    }
    if let Some(unprefixed) = raw.strip_prefix(r"\\?\") {
        out = strip_single_root(&out, unprefixed);
    }
    out
}

fn strip_single_root(text: &str, root: &str) -> String {
    let trimmed = root.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return text.to_string();
    }
    let with_slash = format!("{trimmed}/");
    let with_backslash = format!("{trimmed}\\");
    text.replace(&with_slash, "")
        .replace(&with_backslash, "")
        .replace(trimmed, "<root>")
}

fn scrub_outside_paths(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(^|[\s(\['"=:<`{,|])((?:file://[^\s)\]'";,>`}|]+)|(?:/(?:etc|usr|var|tmp|home|Users|Volumes|private|opt|root|proc|sys|dev|mnt|media|srv|bin|sbin|lib|lib64|System|Library|Applications)(?:/[^\s)\]'";,>`}|]*)?)|(?:/[A-Za-z0-9._~@-]+/[^\s)\]'";,>`}|]*)|(?:[A-Za-z]:[/\\][^\s)\]'";,>`}|]*)|(?:\\\\[^\s)\]'";,>`}|]+))"#,
        )
        .expect("outside-path regex")
    });
    re.replace_all(text, "${1}<outside-path>").into_owned()
}

/// Also check if `plugin` has an explicit [`PluginApprover::Agent`] reason string for diagnostics.
#[must_use]
pub(crate) fn agent_denial_reason(
    plugin: &LoadedPlugin,
    state_error: Option<&str>,
) -> &'static str {
    if state_error.is_some() {
        return "this plugin has no valid review record";
    }
    match plugin.approved_by.as_ref() {
        Some(PluginApprover::Person { .. }) => {
            "this plugin was approved by a person (ask the person: step to Ask, or use the Plugins page)"
        }
        Some(PluginApprover::CommandLine) => {
            "this plugin was approved from the command line (ask the person: step to Ask, or use the Plugins page)"
        }
        Some(PluginApprover::Agent(_)) => {
            "the agent can only change plugins it installed from this project (ask the person: step to Ask, or use the Plugins page)"
        }
        None => {
            "this plugin does not have an agent review record from this project (ask the person: step to Ask, or use the Plugins page)"
        }
    }
}
