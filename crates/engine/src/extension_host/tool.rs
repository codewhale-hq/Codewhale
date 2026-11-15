//! `HostToolSpec`: an extension tool as an ordinary registry `ToolSpec`.
//!
//! Because it is a registry tool, every existing gate applies unchanged:
//! plan mode, the authority envelope, deferral, hooks, approval, and code
//! mode (which suspends gated calls for approval and refuses ungated calls
//! before any host call). Four rules are specific to extension tools:
//!
//! * **A plugin's tool is always `ApprovalRequirement::Required`.** A plugin's
//!   own read-only hint (`presentCall` `kind: 'read'`, MCP-style annotations)
//!   is display data at most. Honouring it would let a plugin switch approval
//!   off for a tool whose body runs arbitrary Node — self-approval. A tool of
//!   a built-in module (tier 0, owner `host:<module>`) gets the approval the
//!   Rust table [`super::tier::BUILTIN_MODULES`] lists for it and nothing the
//!   module says; unlisted is `Required`. It is still never read-only for
//!   plan mode.
//! * **Approval grants are receipt-bound.** Keys are
//!   `ext:<plugin_id>@<content_hash>:<name>:<hash(input)>` for both the exact
//!   and the session-grant key ([`ToolSpec::approval_scope`]), so an updated
//!   plugin, or a different plugin that later takes the same tool name, never
//!   inherits a grant.
//! * **Input is checked against the tool's registered JSON Schema** before
//!   approval and again before anything is sent to the host
//!   ([`super::registry::InputValidator`]). A schema that cannot be compiled is
//!   refused at registration.
//! * **Liveness is re-checked at call time**: the plugin's reviewed receipt,
//!   the Native adapter in this build's policy, and the exact owner
//!   generation. A revocation mid-turn fails the call closed.
//!
//! A fifth thing is not a rule but a capability: when the turn loop serves a
//! permission gate for exactly this call (the model called the tool directly),
//! the call carries an invocation ticket and the tool may ask the core to run
//! core tools through `core/call` (`super::core_call`). The call's deadline
//! then stops while one of those waits on an approval card, and what the tool
//! asked the core to run is attached to its result metadata (`core_calls`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::ManagerShared;
use super::core_call::{HeldImage, InvocationGuard};
use super::protocol::{ContentBlockWire, CoreRequest, DshService, ToolCallParams, ToolResultWire};
use super::registry::ToolRegistration;
use super::supervisor::HostCallError;
use super::tier::{self, HostTier};
use crate::tools::codemode::ExtensionCaller;
use crate::tools::spec::{
    ApprovalRequirement, PreparedToolCall, RichToolResult, ToolCapability, ToolContext, ToolError,
    ToolResult, ToolSpec,
};
use codewhale_tools::ToolResultContentBlock;

/// Default per-call deadline, matching script tools
/// (`SupervisionOptions::tool_call_deadline`).
pub const TOOL_CALL_DEADLINE: Duration = Duration::from_secs(120);

pub struct HostToolSpec {
    registration: ToolRegistration,
    manager: Arc<ManagerShared>,
    selection: Option<codewhale_plugins::composition_scope::SelectionRevision>,
}

impl HostToolSpec {
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(registration: ToolRegistration, manager: Arc<ManagerShared>) -> Self {
        Self {
            registration,
            manager,
            selection: None,
        }
    }

    pub fn for_selection(
        registration: ToolRegistration,
        manager: Arc<ManagerShared>,
        selection: Option<codewhale_plugins::composition_scope::SelectionRevision>,
    ) -> Self {
        Self {
            registration,
            manager,
            selection,
        }
    }
    fn check_caller(&self, context: &ToolContext) -> Result<(), ToolError> {
        self.manager
            .check_selection(
                self.selection,
                context.plugin_registry.as_deref(),
                &self.registration.owner.plugin_id,
                &self.registration.content_hash,
                self.registration.scope.as_ref(),
            )
            .map_err(ToolError::not_available)
    }

    /// `extension:<plugin>` (or the module's owner id, `host:<module>`): the
    /// origin shown in approval cards and diagnostics.
    #[must_use]
    pub fn origin(&self) -> String {
        match self.registration.tier() {
            HostTier::Plugin => format!("extension:{}", self.registration.plugin_name),
            HostTier::Builtin => self.registration.owner.plugin_id.clone(),
        }
    }

    /// The approval this tool needs: always `Required` for a plugin's tool;
    /// for a built-in module's, what the Rust table says and `Required` when
    /// it says nothing.
    fn approval(&self) -> ApprovalRequirement {
        match self.registration.tier() {
            HostTier::Plugin => ApprovalRequirement::Required,
            HostTier::Builtin => tier::tool_approval(
                self.manager.builtin_modules,
                &self.registration.owner.plugin_id,
                &self.registration.name,
            ),
        }
    }

    fn is_creator_plugin_manager(&self) -> bool {
        self.registration.owner.plugin_id == super::creator_policy::CREATOR_PLUGIN_ID
            && self.registration.name == super::creator_policy::PLUGIN_MANAGER_TOOL_NAME
    }

    /// Check `input` against the schema the plugin registered the tool with.
    /// The error is the ordinary invalid-input tool error, so the model sees
    /// what to correct; the host is never reached.
    fn check_input(&self, input: &Value) -> Result<(), ToolError> {
        self.registration
            .input_validator
            .check(input)
            .map_err(|reason| {
                ToolError::invalid_input(format!(
                    "extension tool `{}` rejected the input: {reason}",
                    self.registration.name
                ))
            })
    }

    /// Who this tool is to the turn loop's gate: composed here, from the
    /// registration, never from anything the host says.
    fn caller(&self) -> ExtensionCaller {
        ExtensionCaller {
            origin: self.origin(),
            tool: self.registration.name.clone(),
            scope: format!(
                "ext:{}@{}:{}:{}:{}",
                self.registration.owner.plugin_id,
                self.registration.content_hash,
                self.registration
                    .scope
                    .as_ref()
                    .map(|entry| format!("{}@{}", entry.path, entry.sha256))
                    .unwrap_or_default(),
                self.selection
                    .map(|selection| format!("{}:{}", selection.attachment_id, selection.revision))
                    .unwrap_or_default(),
                crate::session_manager::current_session_boot_id()
            ),
        }
    }

    /// The approval-card text. Rust composes it; the extension supplies none.
    /// Plugins in one host share a process and can interfere with each other,
    /// so the card says when this one is not alone (design §4.4, threat 3).
    #[must_use]
    pub fn approval_text(&self) -> String {
        if self.registration.tier() == HostTier::Builtin {
            return format!(
                "Tool `{}` of built-in host module `{}` ({}) runs on Codewhale's built-in extension host",
                self.registration.name,
                self.registration.plugin_name,
                self.origin()
            );
        }
        let others = self
            .manager
            .registry
            .lock()
            .expect("registry lock")
            .other_active_owners(&self.registration.owner.plugin_id);
        let sharing = match others {
            0 => String::new(),
            1 => "; it shares one host process with 1 other plugin, which can alter its behaviour"
                .to_string(),
            n => format!(
                "; it shares one host process with {n} other plugins, which can alter its behaviour"
            ),
        };
        format!(
            "Extension tool `{}` from plugin `{}` ({}) runs JavaScript on this computer with the extension host's permissions{sharing}",
            self.registration.name,
            self.registration.plugin_name,
            self.origin()
        )
    }
}

impl std::fmt::Debug for HostToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostToolSpec")
            .field("name", &self.registration.name)
            .field("plugin", &self.registration.plugin_name)
            .field("handle", &self.registration.handle)
            .finish()
    }
}

fn map_call_error(tool: &str, error: HostCallError) -> ToolError {
    match error {
        HostCallError::Cancelled(reason) => ToolError::Cancelled {
            message: format!("extension tool `{tool}`: {reason}"),
        },
        HostCallError::Timeout { after, .. } => ToolError::Timeout {
            seconds: after.as_secs(),
        },
        HostCallError::Exited(reason) => {
            ToolError::not_available(format!("extension host exited: {reason}"))
        }
        HostCallError::Busy => {
            ToolError::not_available("extension host is busy; try again".to_string())
        }
        HostCallError::Rpc { code, message } => {
            if code == super::protocol::error_code::NOT_AVAILABLE {
                ToolError::not_available(format!("extension tool `{tool}`: {message}"))
            } else {
                ToolError::execution_failed(format!("extension tool `{tool}` failed: {message}"))
            }
        }
    }
}

pub(super) fn failed_with_acts(
    error: ToolError,
    receipts: Option<&Value>,
) -> Result<ToolResultWire, ToolError> {
    let holds_acts = receipts.is_some_and(|receipts| receipts.get("broker_acts").is_some());
    if !holds_acts {
        return Err(error);
    }
    Ok(ToolResultWire {
        content: vec![ContentBlockWire::Text {
            text: error.to_string(),
        }],
        is_error: true,
        structured: None,
    })
}

fn record_held_image_outcome(
    invocation: Option<&InvocationGuard>,
    method: &str,
    outcome: &str,
    reason: Option<&str>,
    held: &HeldImage,
) {
    let Some(invocation) = invocation else {
        return;
    };
    let mut entry = serde_json::json!({
        "method": method,
        "outcome": outcome,
        "attachment_id": held.attachment_id,
        "media_type": held.media_type,
        "bytes": held.bytes,
        "width": held.width,
        "height": held.height,
    });
    if let (Some(reason), Some(map)) = (reason, entry.as_object_mut()) {
        map.insert("reason".to_string(), Value::String(reason.to_string()));
    }
    let _ = invocation.record_service_act(DshService::Attachments, entry);
}

pub(super) fn resolve_held_image(
    held_image: Option<HeldImage>,
    attachments_granted: bool,
    settled: &mut Result<ToolResultWire, ToolError>,
    invocation: Option<&InvocationGuard>,
) -> Vec<ToolResultContentBlock> {
    if !attachments_granted {
        if let Some(held) = held_image.as_ref() {
            record_held_image_outcome(invocation, "save", "dropped", Some("grant_revoked"), held);
        }
        if let Ok(wire) = settled.as_ref()
            && wire
                .content
                .iter()
                .any(|block| matches!(block, ContentBlockWire::ImageRef { .. }))
        {
            *settled = Err(ToolError::execution_failed(
                "tool returned an image reference after `attachments` grant was revoked",
            ));
        }
        return Vec::new();
    }
    match settled {
        Ok(wire) => {
            let refs: Vec<&str> = wire
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlockWire::ImageRef { attachment_id } => Some(attachment_id.as_str()),
                    ContentBlockWire::Text { .. } => None,
                })
                .collect();
            if wire.is_error {
                let unheld = refs.iter().find(|id| {
                    held_image
                        .as_ref()
                        .is_none_or(|held| held.attachment_id != **id)
                });
                if let Some(held) = held_image.as_ref() {
                    let reason = if unheld.is_some() {
                        "unheld_reference"
                    } else {
                        "tool_failed"
                    };
                    record_held_image_outcome(invocation, "save", "dropped", Some(reason), held);
                }
                if let Some(bad) = unheld {
                    let shown: String = bad.chars().take(80).collect();
                    *settled = Err(ToolError::execution_failed(format!(
                        "tool returned an image reference `{shown}` not held by this call"
                    )));
                }
                return Vec::new();
            }
            if refs.len() > crate::image_attach::MAX_TOOL_RESULT_IMAGES {
                if let Some(held) = held_image.as_ref() {
                    record_held_image_outcome(
                        invocation,
                        "save",
                        "dropped",
                        Some("too_many_images"),
                        held,
                    );
                }
                *settled = Err(ToolError::execution_failed(
                    "this Codewhale keeps one image per tool result",
                ));
                return Vec::new();
            }
            if let [wanted_id] = refs.as_slice() {
                match held_image {
                    Some(held) if held.attachment_id == *wanted_id => {
                        record_held_image_outcome(invocation, "result", "delivered", None, &held);
                        return vec![ToolResultContentBlock::Image {
                            mime_type: held.media_type,
                            data: held.data_base64,
                        }];
                    }
                    Some(held) => {
                        record_held_image_outcome(
                            invocation,
                            "save",
                            "dropped",
                            Some("unheld_reference"),
                            &held,
                        );
                        let shown: String = wanted_id.chars().take(80).collect();
                        *settled = Err(ToolError::execution_failed(format!(
                            "tool returned an image reference `{shown}` not held by this call"
                        )));
                        return Vec::new();
                    }
                    None => {
                        let shown: String = wanted_id.chars().take(80).collect();
                        *settled = Err(ToolError::execution_failed(format!(
                            "tool returned an image reference `{shown}` not held by this call"
                        )));
                        return Vec::new();
                    }
                }
            }
            if let Some(held) = held_image.as_ref() {
                record_held_image_outcome(
                    invocation,
                    "save",
                    "dropped",
                    Some("not_referenced"),
                    held,
                );
            }
            Vec::new()
        }
        Err(_) => {
            if let Some(held) = held_image.as_ref() {
                record_held_image_outcome(invocation, "save", "dropped", Some("tool_failed"), held);
            }
            Vec::new()
        }
    }
}

pub fn wire_to_result(wire: ToolResultWire, origin: &str) -> ToolResult {
    let content = wire
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlockWire::Text { text } => Some(text.as_str()),
            ContentBlockWire::ImageRef { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut metadata = serde_json::Map::new();
    metadata.insert("origin".to_string(), Value::String(origin.to_string()));
    if let Some(structured) = wire.structured {
        metadata.insert("structured".to_string(), structured);
    }
    ToolResult {
        content,
        success: !wire.is_error,
        metadata: Some(Value::Object(metadata)),
    }
}

#[async_trait]
impl ToolSpec for HostToolSpec {
    fn name(&self) -> &str {
        &self.registration.name
    }

    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        self.origin().into()
    }

    fn description(&self) -> &str {
        &self.registration.description
    }

    fn input_schema(&self) -> Value {
        self.registration.input_schema.clone()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        let mut capabilities = vec![ToolCapability::ExecutesCode];
        if self.approval() != ApprovalRequirement::Auto {
            capabilities.push(ToolCapability::RequiresApproval);
        }
        capabilities
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        self.approval()
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        if self.is_creator_plugin_manager() && super::creator_policy::is_list_action(input) {
            ApprovalRequirement::Auto
        } else {
            self.approval()
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        self.is_creator_plugin_manager() && super::creator_policy::is_list_action(input)
    }

    fn defer_loading(&self) -> bool {
        true
    }

    /// Grants are bound to the plugin's reviewed receipt (design §4.3).
    fn approval_scope(&self) -> Option<String> {
        Some(self.caller().scope)
    }

    /// The turn loop serves a permission gate for this tool's call, through
    /// which its `core/call`s are planned and approved.
    fn extension_caller(&self) -> Option<ExtensionCaller> {
        Some(self.caller())
    }

    fn prepare(&self, input: Value, context: &ToolContext) -> Result<PreparedToolCall, ToolError> {
        self.check_caller(context)?;
        // Before the user is asked to approve it: a call the schema refuses is
        // returned to the model to correct and never becomes an approval card.
        self.check_input(&input)?;
        let read_only = self.is_read_only_for(&input);
        let approval = self.approval_requirement_for(&input);
        let description = if self.is_creator_plugin_manager() {
            super::creator_policy::initial_prepared_description(&input)
        } else {
            self.approval_text()
        };
        Ok(PreparedToolCall {
            name: self.registration.name.clone(),
            // Rust composes the card; the extension cannot supply approval text.
            description,
            read_only,
            supports_parallel: false,
            starts_detached: false,
            approval,
            resources: if read_only {
                Vec::new()
            } else {
                vec![crate::tools::spec::ResourceClaim::GlobalExclusive]
            },
            input,
        })
    }

    async fn execute_rich(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        self.check_caller(context)?;
        let registration = &self.registration;
        // Again here: `execute` is also reached without `prepare`, and nothing
        // that fails the schema may be sent to the host.
        self.check_input(&input)?;
        let host = self
            .manager
            .live_host_for(registration)
            .await
            .map_err(ToolError::not_available)?;
        self.check_caller(context)?;
        let call_id = context
            .execution
            .owner_agent_id
            .clone()
            .map(|agent| format!("{agent}:{}", uuid::Uuid::new_v4().simple()))
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        // The deadline travels with the call: the host is told the bound
        // `HostProcess::call` enforces (and cancels at).
        let deadline = self.manager.options.supervision.tool_call_deadline;
        // An invocation ticket (so the tool may ask the core to run tools for
        // it) exists only when the turn loop is serving a permission gate for
        // exactly this tool's call. Otherwise (a call nested in
        // `execute_tools`, a test) the tool has no way to ask for anything.
        let invocation: Option<InvocationGuard> = context
            .execution
            .nested_call_gate
            .as_ref()
            .and_then(|gate| {
                self.manager.core_calls.begin_scoped(
                    &registration.cohort,
                    host.generation,
                    &registration.owner,
                    &call_id,
                    &self.caller(),
                    context,
                    gate,
                    registration.scope.clone(),
                    registration.content_hash.clone(),
                    &self.manager.broker_services(&registration.owner),
                )
            });
        let request = CoreRequest::ToolCall(ToolCallParams {
            handle: registration.handle,
            call_id,
            input,
            deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            // The calling session's workspace and no other: the plugin never
            // learns where else this process has workspaces.
            workspace: context.workspace.to_str().map(str::to_owned),
            ticket: invocation.as_ref().map(|i| i.ticket().to_string()),
            checkpoint_ticket: invocation
                .as_ref()
                .and_then(|i| i.checkpoint_ticket().map(str::to_string)),
            question_ticket: invocation
                .as_ref()
                .and_then(|i| i.question_ticket().map(str::to_string)),
            subagent_ticket: invocation
                .as_ref()
                .and_then(|i| i.subagent_ticket().map(str::to_string)),
            broker_tickets: invocation.as_ref().and_then(|i| i.broker_tickets()),
            caller_tools: invocation.as_ref().and_then(|i| i.caller_tools()),
            session_id: context
                .execution
                .session_objects
                .as_ref()
                .map(|session| session.session_id.clone())
                .filter(|id| !id.is_empty()),
            agent_id: context.execution.owner_agent_id.clone(),
            origin_turn_id: context.execution.origin_turn_id.clone(),
        });
        // The deadline stops while one of this call's `core/call`s waits on
        // the gate (an approval card), so a person's time is not the tool's.
        let outcome = host
            .call_with_clock(
                request,
                Some(registration.owner.plugin_id.clone()),
                invocation.as_ref().map(InvocationGuard::clock),
            )
            .await;
        let held_image = invocation
            .as_ref()
            .and_then(InvocationGuard::seal_held_image);
        let mut settled: Result<ToolResultWire, ToolError> = async {
            match outcome {
                Ok(value) => {
                    self.check_caller(context)?;
                    self.manager
                        .live_host_for(registration)
                        .await
                        .map_err(ToolError::not_available)?;
                    self.check_caller(context)?;
                    serde_json::from_value(value).map_err(|error| {
                        ToolError::execution_failed(format!(
                            "extension tool `{}` returned a malformed result: {error}",
                            registration.name
                        ))
                    })
                }
                Err(error) => Err(map_call_error(&registration.name, error)),
            }
        }
        .await;
        let needs_attachment_grant = held_image.is_some()
            || matches!(
                settled.as_ref(),
                Ok(wire)
                    if wire
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlockWire::ImageRef { .. }))
            );
        let attachments_granted = if needs_attachment_grant {
            self.manager
                .broker_granted(&registration.owner, DshService::Attachments)
                .await
        } else {
            true
        };
        let content_blocks = resolve_held_image(
            held_image,
            attachments_granted,
            &mut settled,
            invocation.as_ref(),
        );
        let receipts = invocation.as_ref().and_then(InvocationGuard::seal_receipts);
        let wire = match settled {
            Ok(wire) => wire,
            Err(error) => {
                let wire = failed_with_acts(error, receipts.as_ref())?;
                match self.check_caller(context) {
                    Ok(()) => wire,
                    Err(stale) => failed_with_acts(stale, receipts.as_ref())?,
                }
            }
        };
        let mut result = wire_to_result(wire, &self.origin());
        if let Some(Value::Object(metadata)) = result.metadata.as_mut() {
            metadata.insert(
                "extension".to_string(),
                serde_json::json!({
                    "plugin_id": registration.owner.plugin_id,
                    "content_hash": registration.content_hash,
                    "entry": registration.scope,
                    "generation": registration.owner.generation,
                    "host_generation": host.generation,
                    "selection": self.selection,
                    "handle": registration.handle,
                    "boot_id": crate::session_manager::current_session_boot_id(),
                }),
            );
        }
        // What the tool asked the core to run, so the persisted record shows it.
        if let (Some(receipts), Some(Value::Object(metadata))) =
            (receipts, result.metadata.as_mut())
        {
            metadata.insert("core_calls".to_string(), receipts);
        }
        Ok(RichToolResult::with_content_blocks(result, content_blocks))
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        self.execute_rich(input, context)
            .await
            .map(RichToolResult::into_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_result_and_soft_failure_survive_the_host_boundary() {
        let value = json!({"count": 2, "items": ["a", "b"]});
        let result = wire_to_result(
            ToolResultWire {
                content: vec![ContentBlockWire::Text {
                    text: "two items".to_string(),
                }],
                is_error: true,
                structured: Some(value.clone()),
            },
            "extension:fixture",
        );
        assert_eq!(result.content, "two items");
        assert!(!result.success);
        let metadata = result.metadata.expect("result metadata");
        assert_eq!(metadata["origin"], "extension:fixture");
        assert_eq!(metadata["structured"], value);
    }
}
