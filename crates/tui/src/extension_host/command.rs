//! Extension slash commands: owned registrations in the user command registry.
//!
//! A command mirrors a tool. The host proposes it with `registry/register`
//! (`kind = "command"`); [`super::registry::OwnerRegistry`] admits or refuses
//! it under the same owner/generation/handle rules; the admitted commands of
//! the owners an engine's workspace desires are loaded into the existing user
//! command registry (`commands::user_registry`, at the lowest
//! precedence, so a built-in or a markdown command always wins the spelling);
//! and a user invocation becomes `command/run` to the host. Revocation,
//! deactivation, crash and generation change remove the registrations the
//! same way they remove tools, and each change bumps [`epoch`] so the user
//! registry reloads.
//!
//! What a command can return, chosen from what the user-command machinery
//! already does: text shown to the user and/or a prompt submitted through the
//! normal turn path. The terminal uses a `System` transcript cell and
//! `AppAction::SendMessage`; the native Runtime API persists a command receipt
//! and admits generated prompts with Runtime provenance.
//! The host never calls the model or a tool itself: anything the model does
//! after a `submit` goes through the normal turn, tool approval included.
//! Invoking a command is the user's own action, so it needs no approval of
//! its own; it still re-checks that the owner is live, that its reviewed
//! receipt is current, and that the host is running before anything is sent.
//!
//! **The built-in command table is a port.** This module is runtime-side and
//! may not reach into `commands`, so registration asks
//! `command_catalog::CommandCatalog` whether a built-in command answers to a name. The
//! host command table implements it and the
//! composition root installs it once at startup (`command_catalog::install`).
//! With none installed a command registration is refused, never accepted
//! unchecked.
//!
//! Known limitations:
//! * The terminal event loop awaits the command (like `/balance`), bounded by
//!   `SupervisionOptions::command_run_deadline` (30 s) and then cancelled with
//!   `$/cancel`. A terminal keypress does not cancel it earlier; the native
//!   Runtime command endpoint supports explicit cancellation.
//! * A clash with a user, workspace or manifest (markdown) command is not a
//!   registration-time refusal, because only the user registry knows the
//!   workspace: the markdown command wins and the extension command is left
//!   out with a load error.
//! * The handler gets no mutable agent or session handle, and no attachments. It is
//!   told the workspace the command was loaded for (`ExtensionCommandRef`) and
//!   its plugin's data directory, plus the invoking session id when known,
//!   all read-only strings. User commands have no Engine turn or agent identity.
//! * A command's result text is bounded and stripped of terminal escapes; a
//!   prompt over [`MAX_PROMPT_BYTES`] is refused, never truncated.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::ManagerShared;
use super::protocol::{CommandResultWire, CommandRunParams, CoreRequest};
use super::registry::CommandRegistration;
use super::supervisor::HostCallError;
use codewhale_config::PluginAuthority;

/// How long the user waits for one command before it is cancelled.
pub const COMMAND_RUN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
/// Longest result text shown; the rest is cut with a marker.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Longest prompt a command may submit. Over this it is refused outright.
pub const MAX_PROMPT_BYTES: usize = 128 * 1024;

static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Something that may have changed which commands are live: a registration,
/// a revocation, an activation, a host exit, an attachment's desired set.
/// Spurious bumps are harmless (one extra registry reload).
pub(crate) fn bump_epoch() {
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

/// The user registry reloads when this moves.
#[must_use]
pub(crate) fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

/// A reference to one admitted command, carried by the user-registry entry
/// and the dispatch action. It names the exact registration: a handle is
/// never reused, so a stale reference can only fail, never run a newer one.
/// It holds no owner token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandRef {
    pub handle: u64,
    pub plugin_id: String,
    pub generation: u64,
    /// `extension:<plugin>`, the origin shown beside the command's output.
    pub origin: String,
    /// The workspace whose user registry loaded the command: what the handler
    /// is told as the place the user ran it.
    pub workspace: std::path::PathBuf,
    pub selection: Option<super::composition_scope::SelectionRevision>,
    pub scope: Option<codewhale_config::EntryRef>,
    pub content_hash: String,
}

impl ExtensionCommandRef {
    pub(crate) fn receipt(&self) -> Value {
        serde_json::json!({
            "plugin_id": self.plugin_id,
            "content_hash": self.content_hash,
            "entry": self.scope,
            "generation": self.generation,
            "selection": self.selection,
            "handle": self.handle,
            "origin": self.origin,
            "boot_id": crate::session_manager::current_session_boot_id(),
        })
    }

    pub(crate) fn registration_id(&self, session_id: &str) -> String {
        codewhale_runtime::hashing::sha256_hex(codewhale_runtime::hashing::canonical_json(
            &serde_json::json!({
                "registration": self.receipt(),
                "workspace": self.workspace,
                "session_id": session_id,
            }),
        ))
    }
}

pub(crate) fn validate_selected_registration(
    command: &ExtensionCommandRef,
    session_id: &str,
) -> Result<(), String> {
    let manager = super::manager();
    let caller = super::caller_view(&command.workspace, Some(session_id), None)
        .ok_or_else(|| "Native command caller is no longer available".to_string())?;
    if command.selection != caller.caller_selection() {
        return Err("Native command belongs to a different caller; select it again".into());
    }
    manager.shared.check_selection(
        command.selection,
        Some(caller.as_ref()),
        &command.plugin_id,
        &command.content_hash,
        command.scope.as_ref(),
    )?;
    let registry = manager.shared.registry.lock().expect("registry lock");
    let current = registry
        .live_command(command.handle, &command.plugin_id, command.generation)
        .ok_or_else(|| "Native command registration is no longer available".to_string())?;
    if current.content_hash != command.content_hash || current.scope != command.scope {
        return Err("Native command registration changed; select it again".into());
    }
    Ok(())
}

pub(crate) async fn validate_reference(
    command: &ExtensionCommandRef,
    session_id: &str,
) -> Result<(), String> {
    validate_selected_registration(command, session_id)?;
    super::manager()
        .shared
        .live_host_for_command(command)
        .await?;
    validate_selected_registration(command, session_id)
}

/// One live command as the user registry loads it.
#[derive(Debug, Clone)]
pub struct ExtensionCommandEntry {
    pub registration: CommandRegistration,
    /// The owner's reviewed authority, so the user registry hides the command
    /// the moment the plugin is disabled or loses trust.
    pub authority: PluginAuthority,
    /// The workspace this entry was loaded for.
    pub workspace: std::path::PathBuf,
    pub selection: Option<super::composition_scope::SelectionRevision>,
}

impl ExtensionCommandEntry {
    #[must_use]
    pub fn reference(&self) -> ExtensionCommandRef {
        ExtensionCommandRef {
            handle: self.registration.handle,
            plugin_id: self.registration.owner.plugin_id.clone(),
            generation: self.registration.owner.generation,
            origin: format!("extension:{}", self.registration.plugin_name),
            workspace: self.workspace.clone(),
            selection: self.selection,
            scope: self.registration.scope.clone(),
            content_hash: self.registration.content_hash.clone(),
        }
    }
}

/// What a command asked the core to do with its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Show `text` (possibly empty) to the user.
    Show { text: String },
    /// Submit `prompt` through the invoking client's normal turn path; show `note` beside it.
    Submit {
        prompt: String,
        note: Option<String>,
    },
}

fn strip_escapes(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    codewhale_secrets::sanitize::strip_ansi_into(text, &mut clean);
    clean
}

/// Strip terminal escapes and bound the text. A command's output is
/// plugin-controlled and goes straight into the transcript.
fn display_text(text: &str) -> String {
    let mut clean = strip_escapes(text);
    if clean.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
        clean.push_str("\n… (output truncated)");
    }
    clean
}

/// The phrase shown after `/<name> (extension:<plugin>):` on failure.
fn map_call_error(error: HostCallError) -> String {
    match error {
        HostCallError::Cancelled(reason) => format!("cancelled: {reason}"),
        HostCallError::Timeout { after, .. } => {
            format!("timed out after {}s and was cancelled", after.as_secs())
        }
        HostCallError::Exited(reason) => format!("extension host is down: exited: {reason}"),
        HostCallError::Busy => "extension host is busy; try again".to_string(),
        HostCallError::Rpc { code, message } => {
            if code == super::protocol::error_code::NOT_AVAILABLE {
                format!("not available: {message}")
            } else {
                format!("failed: {message}")
            }
        }
    }
}

/// Run one command for the user. Errors are the text to show as a failure.
pub(crate) async fn run(
    shared: &ManagerShared,
    command: &ExtensionCommandRef,
    raw_input: &str,
    session_id: Option<&str>,
) -> Result<CommandOutcome, String> {
    let bound = command
        .selection
        .and_then(|revision| shared.plugins_for_selection(revision, session_id));
    let caller = bound.as_ref().map(|(plugins, _)| Arc::clone(plugins));
    let agent_id = bound.and_then(|(_, agent_id)| agent_id);
    shared.check_selection(
        command.selection,
        caller.as_deref(),
        &command.plugin_id,
        &command.content_hash,
        command.scope.as_ref(),
    )?;
    let (host, registration) = shared.live_host_for_command(command).await?;
    shared.check_selection(
        command.selection,
        caller.as_deref(),
        &command.plugin_id,
        &command.content_hash,
        command.scope.as_ref(),
    )?;
    let deadline = shared.options.supervision.command_run_deadline;
    let request = CoreRequest::CommandRun(CommandRunParams {
        handle: registration.handle,
        command_id: uuid::Uuid::new_v4().simple().to_string(),
        raw_input: raw_input.to_string(),
        deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
        workspace: command.workspace.to_str().map(str::to_owned),
        session_id: session_id.filter(|id| !id.is_empty()).map(str::to_owned),
        agent_id,
        origin_turn_id: None,
    });
    let value: Value = host
        .call(request, Some(registration.owner.plugin_id.clone()))
        .await
        .map_err(map_call_error)?;
    shared.check_selection(
        command.selection,
        caller.as_deref(),
        &command.plugin_id,
        &command.content_hash,
        command.scope.as_ref(),
    )?;
    shared.live_host_for_command(command).await?;
    shared.check_selection(
        command.selection,
        caller.as_deref(),
        &command.plugin_id,
        &command.content_hash,
        command.scope.as_ref(),
    )?;
    let wire: CommandResultWire = serde_json::from_value(value)
        .map_err(|error| format!("returned a malformed result: {error}"))?;
    match wire {
        CommandResultWire::Success { text } => Ok(CommandOutcome::Show {
            text: text.as_deref().map(display_text).unwrap_or_default(),
        }),
        CommandResultWire::Error { text } => Err(display_text(&text)),
        CommandResultWire::Submit { prompt, text } => {
            let prompt = strip_escapes(&prompt);
            if prompt.trim().is_empty() {
                return Err("submitted an empty prompt".to_string());
            }
            if prompt.len() > MAX_PROMPT_BYTES {
                return Err(format!(
                    "returned a prompt of {} bytes, over the {MAX_PROMPT_BYTES}-byte limit; it was not submitted",
                    prompt.len()
                ));
            }
            Ok(CommandOutcome::Submit {
                prompt,
                note: text.as_deref().map(display_text).filter(|t| !t.is_empty()),
            })
        }
    }
}
