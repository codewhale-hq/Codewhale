//! Agent focus — one child's full conversation takes over the main
//! transcript area and the composer addresses that child's fork.
//!
//! Selecting a worker anywhere it is listed (the Agents rail panel, the
//! sub-agent cards, `/agents`) focuses it: the transcript area shows the
//! child's complete chat rendered with the same history cells as the main
//! conversation and scrolls the same way, the focused rail row carries a
//! left-edge marker, and the composer grows a chip naming the worker so it is
//! unmistakable that the next message goes to *that* fork. Esc on an empty
//! composer returns to the main conversation.
//!
//! Follow-ups are real runtime work, never a UI illusion: a running child
//! receives the text on its live input channel; an interrupted or completed
//! child is continued from its checkpoint on a new fork (the terminal record
//! is an immutable receipt), and focus follows the fork. Failed and cancelled
//! children answer with the exact reason they cannot continue.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::tui::app::App;
use crate::tui::history::{HistoryCell, history_cells_from_message};
use codewhale_engine::tools::subagent::SubAgentStatus;
use codewhale_localization::MessageId;
use codewhale_models::Message;

/// How often the focused transcript re-reads the child's durable artifact.
/// The rail's live activity line already ticks per event; the full chat only
/// needs to catch up at a human cadence.
const REFRESH_INTERVAL: Duration = Duration::from_millis(400);

/// Focus state for one child.
#[derive(Debug, Clone)]
pub struct AgentFocus {
    /// The worker whose fork the composer addresses.
    pub agent_id: String,
    /// Stable user-facing name (dispatch name, generated whale, or label).
    pub label: String,
    /// The child's transcript rendered as ordinary history cells.
    pub cells: Vec<HistoryCell>,
    /// Number of source messages the cells were built from.
    pub source_message_count: usize,
    /// Messages omitted from the resident tail (durable artifact absent).
    pub omitted_messages: usize,
    /// A settled agent's full result (or the reason it stopped), shown after
    /// the transcript when the transcript does not already end with it, and
    /// on its own when there is no transcript at all (#6565).
    /// The text and whether it is a stop reason rather than a result.
    pub result: Option<(String, bool)>,
    /// Local receipts appended after the source transcript: the user's own
    /// follow-ups echoed immediately and delivery notes.
    pub local_cells: Vec<HistoryCell>,
    /// Scroll position from the top in visual lines; `None` follows the tail.
    pub scroll_top: Option<usize>,
    /// Last visible-line count, so paging keys move a screen at a time.
    pub last_visible: usize,
    /// Last total line count after wrapping (for scrollbar/clamping).
    pub last_total: usize,
    /// Number of permission receipts folded into `cells`, so a new receipt
    /// on an unchanged message count still triggers a rebuild.
    receipt_count: usize,
    last_refresh: Instant,
    settled_refreshed: bool,
    wrapped: Option<(u16, codewhale_palette::UiTheme, Vec<Line<'static>>)>,
}

impl AgentFocus {
    fn new(agent_id: String, label: String) -> Self {
        Self {
            agent_id,
            label,
            cells: Vec::new(),
            source_message_count: 0,
            omitted_messages: 0,
            result: None,
            local_cells: Vec::new(),
            scroll_top: None,
            last_visible: 0,
            last_total: 0,
            receipt_count: 0,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            settled_refreshed: false,
            wrapped: None,
        }
    }

    /// Whether the given agent is the focused one.
    pub fn is(&self, agent_id: &str) -> bool {
        self.agent_id == agent_id
    }
}

/// Load a child's transcript messages: the durable on-disk artifact first
/// (the whole chat), else the bounded resident handle. Returns the messages and
/// how many earlier messages the resident tail omitted.
pub(crate) fn resolve_agent_transcript_messages(
    app: &App,
    agent_id: &str,
) -> (Vec<Message>, usize) {
    if let Ok(display) =
        codewhale_engine::tools::subagent::load_subagent_transcript_display_artifact(
            &app.workspace,
            agent_id,
        )
        && display.conversation_count > 0
    {
        return (display.messages, 0);
    }
    use codewhale_engine::tools::handle::{HandleValue, VarHandle};
    let lookup = VarHandle {
        kind: "var_handle".to_string(),
        session_id: format!("agent:{agent_id}"),
        name: "full_transcript".to_string(),
        type_name: String::new(),
        length: 0,
        repr_preview: String::new(),
        sha256: String::new(),
    };
    let Ok(store) = app.runtime_services.handle_store.try_lock() else {
        return (Vec::new(), 0);
    };
    let Some(record) = store.get(&lookup) else {
        return (Vec::new(), 0);
    };
    let HandleValue::Json(payload) = &record.value else {
        return (Vec::new(), 0);
    };
    let omitted = payload
        .get("omitted_messages")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let messages = payload
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(|raw| {
            raw.iter()
                .filter_map(|value| serde_json::from_value::<Message>(value.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (messages, omitted)
}

/// The same name the rail shows for a worker: its dispatch/session name when
/// it has one, else the generated or labelled display name.
pub(crate) fn agent_display_label(app: &App, agent_id: &str) -> String {
    app.agent_given_name(agent_id)
        .unwrap_or_else(|| crate::tui::agent_details::safe_agent_display_name(app, agent_id))
}

/// Render a child's messages as history cells, folding in the permission
/// receipts recorded for that child: each receipt lands right after the
/// message that carries the tool's result (or, while the call is still
/// running, after the tool-use block itself), so a decision reads in place.
fn cells_for_messages(messages: &[Message], receipts: &[(String, String)]) -> Vec<HistoryCell> {
    use codewhale_models::ContentBlock;
    let mut calls = std::collections::HashMap::new();
    let mut results = std::collections::HashMap::new();
    let mut raw_ids = std::collections::HashMap::<&str, usize>::new();
    for block in messages.iter().flat_map(|message| &message.content) {
        let Some(key) = block
            .tool_call_key()
            .filter(|key| !key.as_str().trim().is_empty())
        else {
            continue;
        };
        match block {
            ContentBlock::ToolUse { id, .. } => {
                *raw_ids.entry(key.as_str()).or_default() += 1;
                calls
                    .entry(key)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(id.as_str()));
            }
            ContentBlock::ToolResult { tool_use_id, .. } => {
                results
                    .entry(key)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(tool_use_id.as_str()));
            }
            _ => {}
        }
    }
    let mut cells = Vec::new();
    for message in messages {
        cells.extend(history_cells_from_message(message));
        for block in &message.content {
            let Some(key) = block.tool_call_key() else {
                continue;
            };
            let Some(Some(provider)) = calls.get(&key) else {
                continue;
            };
            if raw_ids.get(key.as_str()) != Some(&1) {
                continue;
            }
            let anchor = match block {
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    results.get(&key) == Some(&Some(*provider)) && tool_use_id.as_str() == *provider
                }
                ContentBlock::ToolUse { .. } => !results.contains_key(&key),
                _ => false,
            };
            if anchor {
                for (_, text) in receipts.iter().filter(|(id, _)| id == key.as_str()) {
                    cells.push(HistoryCell::System {
                        content: text.clone(),
                    });
                }
            }
        }
    }
    cells
}

fn child_receipts<'a>(app: &'a App, agent_id: &str) -> &'a [(String, String)] {
    app.child_gate_receipts
        .get(agent_id)
        .map_or(&[], Vec::as_slice)
}

/// What a settled agent produced, in full, and whether it is a failure
/// reason rather than a result. The durable worker record (the roster) holds
/// the complete text; the manager snapshot is the fallback. `None` while the
/// agent is live or when it left nothing. Not redacted; see [`settled_result`].
pub(crate) fn settled_result_raw(app: &App, agent_id: &str) -> Option<(String, bool)> {
    let from_roster = app
        .current_agent_roster()
        .iter()
        .find(|row| row.worker_id == agent_id)
        .and_then(|row| {
            let failed = matches!(
                row.state,
                codewhale_engine::agent_roster::RosterState::Failed
                    | codewhale_engine::agent_roster::RosterState::Cancelled
            );
            row.outcome.clone().map(|text| (text, failed))
        });
    let from_snapshot = || {
        let agent = app
            .subagent_cache
            .iter()
            .find(|agent| agent.agent_id == agent_id)?;
        match &agent.status {
            SubAgentStatus::Running => None,
            SubAgentStatus::Completed => agent.result.clone().map(|text| (text, false)),
            SubAgentStatus::Failed(reason) | SubAgentStatus::Interrupted(reason) => {
                Some((reason.clone(), true))
            }
            SubAgentStatus::Cancelled | SubAgentStatus::BudgetExhausted => {
                agent.result.clone().map(|text| (text, true))
            }
        }
    };
    from_roster
        .or_else(from_snapshot)
        .filter(|(text, _)| !text.trim().is_empty())
}

/// Strip terminal escapes and redact secrets before agent text is shown.
fn visible_agent_text(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    codewhale_tui_kit::osc8::strip_ansi_into(text, &mut visible);
    codewhale_config::persistence::redact_secrets(&visible)
}

/// [`settled_result_raw`], safe to render.
pub(crate) fn settled_result(app: &App, agent_id: &str) -> Option<(String, bool)> {
    settled_result_raw(app, agent_id)
        .map(|(text, failed)| (visible_agent_text(&text).trim().to_string(), failed))
        .filter(|(text, _)| !text.is_empty())
}

/// One line saying what a settled agent produced (its result headline, or
/// why it stopped), bounded for a row with a visible `…` when cut. The full
/// text is in the agent's focused view.
pub(crate) fn settled_headline(app: &App, agent_id: &str) -> Option<String> {
    let (text, _) = settled_result_raw(app, agent_id)?;
    codewhale_engine::agent_roster::result_headline(&text)
        .map(|headline| codewhale_engine::agent_roster::one_line(&visible_agent_text(&headline)))
}

/// The result block for a settled agent, unless the transcript already ends
/// with that answer. The completion envelope is machine data and is dropped.
fn result_block_for(app: &App, agent_id: &str, messages: &[Message]) -> Option<(String, bool)> {
    let (text, failed) = settled_result(app, agent_id)?;
    let text = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("<codewhale:"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let answered = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(message_plain_text)
        .is_some_and(|last| {
            codewhale_engine::agent_roster::result_headline(text)
                .is_some_and(|headline| last.contains(headline.as_str()))
        });
    (!answered || failed).then(|| (text.to_string(), failed))
}

/// The heading and body cells for a result block.
fn result_cells(app: &App, result: Option<&(String, bool)>) -> Vec<HistoryCell> {
    let Some((text, failed)) = result else {
        return Vec::new();
    };
    let heading = if *failed {
        MessageId::AgentFocusStopReason
    } else {
        MessageId::AgentFocusResult
    };
    vec![
        HistoryCell::System {
            content: app.tr(heading).into_owned(),
        },
        HistoryCell::Assistant {
            content: text.clone(),
            streaming: false,
        },
    ]
}

/// Focus a child: its full transcript owns the main area and the composer
/// addresses its fork. Re-focusing the same child is a no-op that keeps the
/// scroll position; focusing another child replaces the focus.
pub(crate) fn focus_agent(app: &mut App, agent_id: &str) {
    if app
        .agent_focus
        .as_ref()
        .is_some_and(|focus| focus.is(agent_id))
    {
        app.needs_redraw = true;
        return;
    }
    let label = agent_display_label(app, agent_id);
    let mut focus = AgentFocus::new(agent_id.to_string(), label.clone());
    let (messages, omitted) = resolve_agent_transcript_messages(app, agent_id);
    let receipts = child_receipts(app, agent_id);
    focus.cells = cells_for_messages(&messages, receipts);
    focus.receipt_count = receipts.len();
    focus.source_message_count = messages.len();
    focus.omitted_messages = omitted;
    focus.result = result_block_for(app, agent_id, &messages);
    focus.last_refresh = Instant::now();
    app.agent_focus = Some(focus);
    codewhale_engine::extension_host::command::bump_epoch();
    // The composer is now the natural owner: the next keys address the
    // worker, and Esc leaves focus rather than the rail.
    crate::tui::work_surface::release_focus(app);
    app.scroll_to_bottom();
    let status = app
        .tr(MessageId::AgentFocusOpened)
        .replace("{agent}", &label);
    app.status_message = Some(status.clone());
    app.push_status_toast(status, crate::tui::app::StatusToastLevel::Info, Some(4_000));
    app.needs_redraw = true;
}

/// Return to the main conversation. Returns whether a focus was active.
pub(crate) fn exit_focus(app: &mut App) -> bool {
    codewhale_engine::extension_host::command::bump_epoch();
    let Some(focus) = app.agent_focus.take() else {
        return false;
    };
    // The rail tracked this worker as its opened row while focused.
    crate::tui::work_surface::agent_details_closed(app, &focus.agent_id);
    app.scroll_to_bottom();
    let status = app.tr(MessageId::AgentFocusClosed).into_owned();
    app.status_message = Some(status);
    app.needs_redraw = true;
    true
}

/// Re-read the focused child's transcript at a human cadence so live workers
/// stream into the focused view. Local echoes of the user's own follow-ups are
/// dropped once the child's own transcript carries them.
pub(crate) fn refresh_focus(app: &mut App) {
    let Some(focus) = app.agent_focus.as_ref() else {
        return;
    };
    if focus.last_refresh.elapsed() < REFRESH_INTERVAL {
        return;
    }
    let agent_id = focus.agent_id.clone();
    let settled = app
        .subagent_cache
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .is_some_and(|agent| !matches!(agent.status, SubAgentStatus::Running));
    if settled && focus.settled_refreshed {
        return;
    }
    let (messages, omitted) = resolve_agent_transcript_messages(app, &agent_id);
    let receipts = child_receipts(app, &agent_id).to_vec();
    // The result lands when the agent settles, often with no new message.
    let result = result_block_for(app, &agent_id, &messages);
    let Some(focus) = app.agent_focus.as_mut() else {
        return;
    };
    focus.last_refresh = Instant::now();
    focus.settled_refreshed = settled;
    if focus.result != result {
        focus.result = result;
        focus.wrapped = None;
        app.needs_redraw = true;
    }
    let Some(focus) = app.agent_focus.as_mut() else {
        return;
    };
    if messages.len() == focus.source_message_count
        && omitted == focus.omitted_messages
        && receipts.len() == focus.receipt_count
    {
        return;
    }
    let previous_count = focus.source_message_count;
    focus.wrapped = None;
    focus.cells = cells_for_messages(&messages, &receipts);
    focus.receipt_count = receipts.len();
    focus.source_message_count = messages.len();
    focus.omitted_messages = omitted;
    // Drop local user echoes that the transcript now carries itself.
    for message in messages.iter().skip(previous_count) {
        if message.role != "user" {
            continue;
        }
        let text = message_plain_text(message);
        if let Some(index) = focus.local_cells.iter().position(
            |cell| matches!(cell, HistoryCell::User { content } if content.trim() == text.trim()),
        ) {
            focus.local_cells.remove(index);
        }
    }
    app.needs_redraw = true;
}

fn message_plain_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            codewhale_models::ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Record the user's follow-up in the focused view immediately so the send is
/// visible before the child's transcript catches up.
pub(crate) fn echo_user_follow_up(app: &mut App, text: &str) {
    if let Some(focus) = app.agent_focus.as_mut() {
        focus.local_cells.push(HistoryCell::User {
            content: text.to_string(),
        });
        focus.wrapped = None;
        focus.scroll_top = None;
        app.needs_redraw = true;
    }
}

/// Apply a delivery receipt from the engine. When the child was continued on
/// a new fork, focus follows the fork so the conversation stays in one place.
pub(crate) fn apply_follow_up_receipt(
    app: &mut App,
    agent_id: &str,
    outcome: &Result<codewhale_engine::tools::subagent::UserFollowUpOutcome, String>,
) {
    let label = agent_display_label(app, agent_id);
    let note = match outcome {
        Ok(receipt) if receipt.resumed && receipt.target_agent_id != agent_id => {
            let target_label = agent_display_label(app, &receipt.target_agent_id);
            if app
                .agent_focus
                .as_ref()
                .is_some_and(|focus| focus.is(agent_id))
            {
                // Carry the local echoes across so the send stays visible on
                // the fork until its transcript includes it.
                let carried = app
                    .agent_focus
                    .as_ref()
                    .map(|focus| focus.local_cells.clone())
                    .unwrap_or_default();
                app.agent_focus = None;
                codewhale_engine::extension_host::command::bump_epoch();
                focus_agent(app, &receipt.target_agent_id);
                if let Some(focus) = app.agent_focus.as_mut() {
                    focus.local_cells = carried;
                    focus.wrapped = None;
                }
            }
            app.tr(MessageId::AgentFocusFollowUpContinued)
                .replace("{agent}", &label)
                .replace("{target}", &target_label)
        }
        Ok(receipt) if receipt.delivered => app
            .tr(MessageId::AgentFocusFollowUpDelivered)
            .replace("{agent}", &label),
        Ok(receipt) => app
            .tr(MessageId::AgentFocusFollowUpFailed)
            .replace("{agent}", &label)
            .replace("{reason}", &receipt.note),
        Err(reason) => app
            .tr(MessageId::AgentFocusFollowUpFailed)
            .replace("{agent}", &label)
            .replace("{reason}", reason),
    };
    let level = if matches!(outcome, Ok(receipt) if receipt.delivered) {
        crate::tui::app::StatusToastLevel::Info
    } else {
        crate::tui::app::StatusToastLevel::Warning
    };
    // The receipt belongs to the agent it names (or the fork focus followed
    // above). A delayed receipt for another agent must not land in whichever
    // transcript happens to be focused now; the toast still reports it.
    let receipt_target = outcome
        .as_ref()
        .ok()
        .map(|receipt| receipt.target_agent_id.as_str());
    if let Some(focus) = app
        .agent_focus
        .as_mut()
        .filter(|focus| focus.is(agent_id) || receipt_target.is_some_and(|id| focus.is(id)))
    {
        focus.local_cells.push(HistoryCell::System {
            content: note.clone(),
        });
        focus.wrapped = None;
        focus.scroll_top = None;
    }
    app.status_message = Some(note.clone());
    app.push_status_toast(note, level, Some(5_000));
    app.needs_redraw = true;
}

/// Status word for the focused child from the live cache (glyph + word rule:
/// the word is always shown; color is secondary).
pub(crate) fn focused_status(app: &App) -> Option<(char, String)> {
    let focus = app.agent_focus.as_ref()?;
    let agent = app
        .subagent_cache
        .iter()
        .find(|agent| agent.agent_id == focus.agent_id)?;
    Some(match &agent.status {
        SubAgentStatus::Running => ('●', "running".to_string()),
        SubAgentStatus::Completed => ('✓', "done".to_string()),
        SubAgentStatus::Interrupted(_) => ('⏸', "interrupted".to_string()),
        SubAgentStatus::Failed(_) => ('✕', "failed".to_string()),
        SubAgentStatus::Cancelled => ('✕', "cancelled".to_string()),
        SubAgentStatus::BudgetExhausted => ('◆', "budget exhausted".to_string()),
    })
}

/// One short line naming the focused worker's effective posture: its role,
/// whether it may write the workspace, reach the network, and run shell —
/// from the runtime's persisted permission snapshot, never guessed.
pub(crate) fn focused_posture(app: &App) -> Option<String> {
    let focus = app.agent_focus.as_ref()?;
    let agent = app
        .subagent_cache
        .iter()
        .find(|agent| agent.agent_id == focus.agent_id)?;
    let permissions = agent.runtime_permissions.as_ref()?;
    let write = app.tr(if permissions.write {
        MessageId::AgentFocusPostureWrites
    } else {
        MessageId::AgentFocusPostureReadOnly
    });
    let network = app.tr(if permissions.network {
        MessageId::AgentFocusPostureNetwork
    } else {
        MessageId::AgentFocusPostureNoNetwork
    });
    let shell = app.tr(match permissions.shell.as_str() {
        "full" => MessageId::AgentFocusPostureShellFull,
        "read_only" => MessageId::AgentFocusPostureShellReadOnly,
        _ => MessageId::AgentFocusPostureShellNone,
    });
    Some(
        app.tr(MessageId::AgentFocusPosture)
            .replace("{role}", agent.agent_type.as_str())
            .replace("{write}", &write)
            .replace("{network}", &network)
            .replace("{shell}", &shell),
    )
}

/// Whether any worker exists to list, focus, or manage this session.
pub(crate) fn agents_exist(app: &App) -> bool {
    !app.subagent_cache.is_empty() || !app.agent_progress.is_empty()
}

/// Shell action owned by the agent shortcuts advertised above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentShellShortcut {
    FocusAgents,
    ManageAgents,
}

/// Whether the composer currently owns the two agent shortcuts.
///
/// This is deliberately stricter than [`agents_exist`]. A visible worker is
/// not enough to advertise a key when a modal, attachment, or focused inline
/// surface owns that same arrow. Rendering and dispatch both consume this
/// predicate so the footer cannot promise an action that another owner will
/// swallow.
///
/// A workflow run on the workbar counts as agents to manage: `↓` then opens
/// `/workflows` instead of the agent register.
pub(crate) fn shell_shortcuts_available(app: &App, completion_menu_open: bool) -> bool {
    (agents_exist(app) || !app.workflow_runs.is_empty())
        && !completion_menu_open
        && app.input.is_empty()
        && app.view_stack.is_empty()
        && app.selected_composer_attachment_index().is_none()
        && !app.work_surface.focused
}

/// Resolve a key only while the agent shortcut contract is actually active.
pub(crate) fn shell_shortcut(
    app: &App,
    key: &KeyEvent,
    completion_menu_open: bool,
) -> Option<AgentShellShortcut> {
    if key.modifiers != KeyModifiers::NONE || !shell_shortcuts_available(app, completion_menu_open)
    {
        return None;
    }
    match key.code {
        KeyCode::Left => Some(AgentShellShortcut::FocusAgents),
        KeyCode::Down => Some(AgentShellShortcut::ManageAgents),
        _ => None,
    }
}

/// Footer hint chain fragment `← for agents · ↓ to manage` (ASCII-safe:
/// `<- for agents · v to manage`). Words are localized; the glyphs follow the
/// shell's ASCII-safe switch.
pub(crate) fn footer_agent_hints(app: &App) -> String {
    let ascii = crate::tui::color_compat::ascii_safe_enabled();
    let (left, down) = if ascii { ("<-", "v") } else { ("←", "↓") };
    format!(
        "{left} {} · {down} {}",
        app.tr(MessageId::FooterHintForAgents),
        app.tr(MessageId::FooterHintToManage)
    )
}

/// `· N queued` suffix for a rail row: follow-ups the running child has not
/// yet taken at its next round boundary. `None` when nothing is queued.
pub(crate) fn queued_suffix(app: &App, agent_id: &str) -> Option<String> {
    let count = *app.agent_queued_follow_ups.get(agent_id)?;
    if count == 0 {
        return None;
    }
    Some(
        app.tr(MessageId::AgentRailQueuedCount)
            .replace("{count}", &count.to_string()),
    )
}

/// The composer chip naming the addressed fork.
pub(crate) fn composer_chip_text(app: &App) -> Option<String> {
    let focus = app.agent_focus.as_ref()?;
    Some(
        app.tr(MessageId::AgentFocusComposerChip)
            .replace("{agent}", &focus.label),
    )
}

/// Empty-composer hint while focused.
pub(crate) fn composer_placeholder(app: &App) -> Option<String> {
    let focus = app.agent_focus.as_ref()?;
    Some(
        app.tr(MessageId::AgentFocusPlaceholder)
            .replace("{agent}", &focus.label),
    )
}

/// Render the focused child's transcript into the main conversation area.
///
/// Consumes the shared `pending_scroll_delta` so PageUp/PageDown, wheel, and
/// the jump-to-latest affordance behave exactly as they do on the main
/// transcript.
pub(crate) fn render_focus(app: &mut App, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    let Some(focus) = app.agent_focus.as_ref() else {
        return;
    };
    let theme = app.ui_theme;
    let background = Style::default().bg(theme.surface_bg);
    buf.set_style(area, background);
    if area.height == 0 || area.width == 0 {
        return;
    }
    let (status_glyph, status_word) = focused_status(app).unwrap_or(('○', "unknown".to_string()));
    let banner = app
        .tr(MessageId::AgentFocusBanner)
        .replace("{agent}", &focus.label)
        .replace("{status}", &status_word);
    let mut banner_spans = vec![
        Span::styled(
            format!("{status_glyph} "),
            Style::default().fg(theme.accent_action),
        ),
        Span::styled(
            banner,
            Style::default()
                .fg(theme.accent_action)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    // The worker's effective posture, in the same dot chain: what it may do
    // is stated where its conversation is read, not hidden in a role name.
    if let Some(posture) = focused_posture(app) {
        banner_spans.push(Span::styled(
            format!(" · {posture}"),
            Style::default().fg(theme.text_muted),
        ));
    }
    let banner_line = Line::from(banner_spans);
    let width = area.width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if focus.omitted_messages > 0 {
        lines.push(Line::from(Span::styled(
            app.tr(MessageId::AgentFocusOmitted)
                .replace("{count}", &focus.omitted_messages.to_string()),
            Style::default().fg(theme.text_muted),
        )));
    }
    let result_cells = result_cells(app, focus.result.as_ref());
    if focus.cells.is_empty() && result_cells.is_empty() && focus.local_cells.is_empty() {
        lines.push(Line::from(Span::styled(
            app.tr(MessageId::AgentFocusNoTranscript)
                .replace("{agent}", &focus.label),
            Style::default().fg(theme.text_muted),
        )));
    }
    if !focus
        .wrapped
        .as_ref()
        .is_some_and(|(cached_width, cached_theme, _)| {
            *cached_width == width && *cached_theme == theme
        })
    {
        let mut body = Vec::new();
        for cell in focus
            .cells
            .iter()
            .chain(result_cells.iter())
            .chain(focus.local_cells.iter())
        {
            body.extend(cell.transcript_lines(width));
            body.push(Line::default());
        }
        if let Some(focus) = app.agent_focus.as_mut() {
            focus.wrapped = Some((width, theme, body));
        }
    }
    let Some(focus) = app.agent_focus.as_ref() else {
        return;
    };
    if let Some((_, _, body)) = focus.wrapped.as_ref() {
        lines.extend(body.iter().cloned());
    }
    let visible = usize::from(area.height.saturating_sub(1)).max(1);
    let total = lines.len();
    let max_top = total.saturating_sub(visible);
    let delta = app.viewport.pending_scroll_delta;
    app.viewport.pending_scroll_delta = 0;
    let Some(focus) = app.agent_focus.as_mut() else {
        return;
    };
    let current = focus.scroll_top.unwrap_or(max_top);
    let next = if delta < 0 {
        current.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        current.saturating_add(delta as usize)
    }
    .min(max_top);
    focus.scroll_top = if next >= max_top { None } else { Some(next) };
    focus.last_visible = visible;
    focus.last_total = total;
    let top = focus.scroll_top.unwrap_or(max_top);

    lines.insert(0, banner_line);
    let mut viewport = codewhale_ratatui::TranscriptViewport::new(&lines);
    viewport.offset = top;
    viewport.pinned_rows = 1;
    viewport.style = background;
    let plan = viewport.render(area, buf);
    app.viewport.ocean_semantic_surfaces = codewhale_ratatui::ocean::ocean_semantic_surfaces(
        plan.display_rows(),
        plan.area,
        codewhale_tui_kit::ui_text::grapheme_display_width,
    );
    // The focused view owns the transcript geometry for paging keys.
    app.viewport.last_transcript_area = Some(plan.body_area);
    app.viewport.last_transcript_visible = visible;
    app.viewport.last_transcript_total = total;
    app.viewport.last_transcript_top = top;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn test_app(workspace: PathBuf) -> App {
        App::new(
            TuiOptions {
                model: "test-model".to_string(),
                use_mouse_capture: true,
                max_subagents: 4,
                ..crate::tui::test_support::test_tui_options(workspace)
            },
            &Config::default(),
        )
    }

    #[test]
    fn child_receipts_anchor_only_to_unique_execution_pairs() {
        let messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"wire","execution_id":"first","name":"read_file","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire","execution_id":"first","content":"one"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"wire","execution_id":"second","name":"read_file","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire","execution_id":"second","content":"two"}]}
        ])).unwrap();
        let receipts = vec![
            ("first".into(), "receipt one".into()),
            ("second".into(), "receipt two".into()),
        ];
        let rendered = cells_for_messages(&messages, &receipts);
        let anchored: Vec<_> = rendered
            .iter()
            .filter_map(|cell| match cell {
                HistoryCell::System { content } if content.starts_with("receipt ") => {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(anchored, ["receipt one", "receipt two"]);

        for duplicate in [messages[0].clone(), serde_json::from_value(json!({
            "role":"assistant","content":[{"type":"tool_use","id":"first","name":"read_file","input":{}}]
        })).unwrap()] {
            let mut ambiguous = messages.clone();
            ambiguous.push(duplicate);
            let rendered = cells_for_messages(&ambiguous, &receipts);
            assert!(!rendered.iter().any(|cell| matches!(cell, HistoryCell::System { content } if content == "receipt one")));
            assert_eq!(rendered.iter().filter(|cell| matches!(cell, HistoryCell::System { content } if content == "receipt two")).count(), 1);
        }
    }

    #[test]
    fn agent_shell_shortcuts_only_claim_an_unowned_empty_composer() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);

        assert_eq!(
            shell_shortcut(&app, &left, false),
            None,
            "no agents: cursor owns Left"
        );
        assert_eq!(
            shell_shortcut(&app, &down, false),
            None,
            "no agents: cursor owns Down"
        );

        app.agent_progress
            .insert("agent_one".to_string(), "working".to_string());
        assert_eq!(
            shell_shortcut(&app, &left, false),
            Some(AgentShellShortcut::FocusAgents)
        );
        assert_eq!(
            shell_shortcut(&app, &down, false),
            Some(AgentShellShortcut::ManageAgents)
        );

        app.input = "draft".to_string();
        assert_eq!(
            shell_shortcut(&app, &left, false),
            None,
            "text cursor keeps Left"
        );
        app.input.clear();
        app.work_surface.focused = true;
        assert_eq!(
            shell_shortcut(&app, &down, false),
            None,
            "focused work surface keeps Down for row navigation"
        );
    }

    #[test]
    fn open_completion_menu_keeps_agent_shortcuts_out_of_its_arrows() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        app.agent_progress
            .insert("agent_one".to_string(), "working".to_string());

        assert_eq!(
            shell_shortcut(
                &app,
                &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
                true,
            ),
            None
        );
        assert_eq!(
            shell_shortcut(
                &app,
                &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                true,
            ),
            None
        );
        assert!(!shell_shortcuts_available(&app, true));
    }

    fn seed_resident_transcript(app: &mut App, agent_id: &str, messages: serde_json::Value) {
        let mut store = app
            .runtime_services
            .handle_store
            .try_lock()
            .expect("handle store");
        let count = messages.as_array().map(|m| m.len()).unwrap_or(0);
        let _ = store.insert_json(
            format!("agent:{agent_id}"),
            "full_transcript",
            json!({ "message_count": count, "messages": messages }),
        );
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render_focus(app, area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn focus_renders_the_childs_full_transcript_and_the_composer_addresses_it() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        seed_resident_transcript(
            &mut app,
            "agent_alpha",
            json!([
                {"role": "user", "content": [{"type": "text", "text": "Investigate the flaky test", "cache_control": null}]},
                {"role": "assistant", "content": [{"type": "text", "text": "Found the race in the pool", "cache_control": null}]}
            ]),
        );
        focus_agent(&mut app, "agent_alpha");
        let focus = app.agent_focus.as_ref().expect("focused");
        assert_eq!(focus.source_message_count, 2);
        assert_eq!(focus.cells.len(), 2);
        let screen = render(&mut app, 80, 12);
        assert!(screen.contains("Investigate the flaky test"), "{screen}");
        assert!(screen.contains("Found the race in the pool"), "{screen}");
        assert!(
            composer_chip_text(&app)
                .expect("chip")
                .contains(&focus_label(&app)),
            "chip names the focused worker"
        );
        assert!(composer_placeholder(&app).is_some());
        assert!(exit_focus(&mut app));
        assert!(app.agent_focus.is_none());
        assert!(composer_chip_text(&app).is_none());
        assert!(!exit_focus(&mut app));
    }

    #[test]
    fn focus_banner_states_the_workers_effective_posture_from_the_runtime_snapshot() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        seed_resident_transcript(
            &mut app,
            "agent_scout",
            json!([{"role": "user", "content": [{"type": "text", "text": "look around", "cache_control": null}]}]),
        );
        app.subagent_cache
            .push(codewhale_engine::tools::subagent::SubAgentResult {
                usage: None,
                name: "agent_scout".to_string(),
                agent_id: "agent_scout".to_string(),
                context_mode: "fresh".to_string(),
                fork_context: false,
                workspace: None,
                git_branch: None,
                agent_type: codewhale_agent_contract::role::FleetRole::Scout,
                assignment: codewhale_engine::tools::subagent::SubAgentAssignment {
                    native_preset: None,
                    objective: "look around".to_string(),
                    role: Some("explore".to_string()),
                },
                model: "deepseek-v4-flash".to_string(),
                nickname: None,
                status: SubAgentStatus::Running,
                worker_status: None,
                runtime_permissions: Some(codewhale_protocol::fleet::FleetEffectivePermissions {
                    write: false,
                    network: true,
                    shell: "read_only".to_string(),
                    tool_scope: "inherit".to_string(),
                    tools: Vec::new(),
                    background: true,
                    max_spawn_depth: 1,
                    profile_id: None,
                    profile_origin: None,
                    source: "built_in".to_string(),
                }),
                parent_run_id: None,
                spawn_depth: 1,
                child_route: None,
                result: None,
                steps_taken: 0,
                checkpoint: None,
                needs_input: None,
                duration_ms: 0,
                started_at: None,
                from_prior_session: false,
                idle_ms: None,
                heartbeat_timeout_ms: None,
            });
        focus_agent(&mut app, "agent_scout");
        let posture = focused_posture(&app).expect("posture line from the snapshot");
        assert_eq!(posture, "explore · read-only · network · read-only shell");
        let screen = render(&mut app, 100, 8);
        assert!(
            screen.contains("read-only · network · read-only shell"),
            "{screen}"
        );
        // No snapshot, no guess.
        app.subagent_cache[0].runtime_permissions = None;
        assert!(focused_posture(&app).is_none());
    }

    fn focus_label(app: &App) -> String {
        app.agent_focus.as_ref().map(|f| f.label.clone()).unwrap()
    }

    #[test]
    fn empty_transcript_explains_instead_of_dead_ending() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        focus_agent(&mut app, "agent_quiet");
        let screen = render(&mut app, 120, 8);
        let expected = app
            .tr(MessageId::AgentFocusNoTranscript)
            .replace("{agent}", &focus_label(&app));
        let head: String = expected.chars().take(24).collect();
        assert!(screen.contains(head.trim_end()), "{screen}");
    }

    fn settled_agent(
        agent_id: &str,
        status: SubAgentStatus,
        result: Option<&str>,
    ) -> codewhale_engine::tools::subagent::SubAgentResult {
        codewhale_engine::tools::subagent::SubAgentResult {
            usage: None,
            name: agent_id.to_string(),
            agent_id: agent_id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: codewhale_agent_contract::role::FleetRole::Scout,
            assignment: codewhale_engine::tools::subagent::SubAgentAssignment {
                native_preset: None,
                objective: "audit the docs".to_string(),
                role: Some("explore".to_string()),
            },
            model: "deepseek-v4-flash".to_string(),
            nickname: None,
            status,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 1,
            child_route: None,
            result: result.map(str::to_string),
            steps_taken: 3,
            checkpoint: None,
            needs_input: None,
            duration_ms: 1_000,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }

    #[test]
    fn a_finished_agent_without_a_transcript_shows_its_full_result() {
        // #6565: with no live capture the focused view explained the missing
        // capture instead of showing the answer the engine had kept.
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let body = (0..80)
            .map(|n| format!("Finding {n} is documented in section {n}."))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(body.len() > 2_500);
        let result = format!("## Summary\n\n{body}\n\nEND-OF-RESULT");
        app.subagent_cache.push(settled_agent(
            "agent_done",
            SubAgentStatus::Completed,
            Some(&result),
        ));
        focus_agent(&mut app, "agent_done");
        let (text, failed) = app
            .agent_focus
            .as_ref()
            .and_then(|focus| focus.result.clone())
            .expect("the stored result is the focused view");
        assert!(!failed);
        assert_eq!(text, result);
        let screen = render(&mut app, 100, 200);
        assert!(screen.contains("Finding 0 is documented"), "{screen}");
        assert!(screen.contains("Finding 79 is documented"), "{screen}");
        assert!(screen.contains("END-OF-RESULT"), "{screen}");
        let heading = app.tr(MessageId::AgentFocusResult).into_owned();
        assert!(screen.contains(&heading), "{screen}");
        let missing = app
            .tr(MessageId::AgentFocusNoTranscript)
            .replace("{agent}", &focus_label(&app));
        let head: String = missing.chars().take(12).collect();
        assert!(!screen.contains(head.trim_end()), "{screen}");
    }

    #[test]
    fn a_failed_agent_shows_why_it_stopped_in_full() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let reason = format!(
            "provider returned 429 after 5 retries; {} last-line",
            "context ".repeat(80)
        );
        app.subagent_cache.push(settled_agent(
            "agent_failed",
            SubAgentStatus::Failed(reason.clone()),
            None,
        ));
        focus_agent(&mut app, "agent_failed");
        assert_eq!(
            app.agent_focus
                .as_ref()
                .and_then(|focus| focus.result.clone()),
            Some((reason.trim().to_string(), true))
        );
        let screen = render(&mut app, 100, 60);
        assert!(screen.contains("last-line"), "{screen}");
        let heading = app.tr(MessageId::AgentFocusStopReason).into_owned();
        assert!(screen.contains(&heading), "{screen}");
    }

    #[test]
    fn a_transcript_that_already_ends_with_the_answer_gets_no_result_block() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        seed_resident_transcript(
            &mut app,
            "agent_told",
            json!([
                {"role": "user", "content": [{"type": "text", "text": "audit", "cache_control": null}]},
                {"role": "assistant", "content": [{"type": "text", "text": "## Summary\n\nThree links are stale. Details follow.", "cache_control": null}]}
            ]),
        );
        app.subagent_cache.push(settled_agent(
            "agent_told",
            SubAgentStatus::Completed,
            Some("## Summary\n\nThree links are stale. Details follow."),
        ));
        focus_agent(&mut app, "agent_told");
        assert_eq!(app.agent_focus.as_ref().unwrap().result, None);

        // A capture that stops before the answer gets the answer appended,
        // and the refresh picks it up when the agent settles later.
        seed_resident_transcript(
            &mut app,
            "agent_partial",
            json!([{"role": "user", "content": [{"type": "text", "text": "audit", "cache_control": null}]}]),
        );
        app.subagent_cache.push(settled_agent(
            "agent_partial",
            SubAgentStatus::Running,
            None,
        ));
        focus_agent(&mut app, "agent_partial");
        assert_eq!(app.agent_focus.as_ref().unwrap().result, None);
        let agent = app
            .subagent_cache
            .iter_mut()
            .find(|agent| agent.agent_id == "agent_partial")
            .unwrap();
        agent.status = SubAgentStatus::Completed;
        agent.result = Some("Two links are stale.".to_string());
        app.agent_focus.as_mut().unwrap().last_refresh = Instant::now() - REFRESH_INTERVAL;
        refresh_focus(&mut app);
        assert_eq!(
            app.agent_focus.as_ref().unwrap().result,
            Some(("Two links are stale.".to_string(), false))
        );
    }

    #[test]
    fn user_echo_is_replaced_once_the_child_transcript_carries_it() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        seed_resident_transcript(
            &mut app,
            "agent_echo",
            json!([{"role": "assistant", "content": [{"type": "text", "text": "ready", "cache_control": null}]}]),
        );
        focus_agent(&mut app, "agent_echo");
        echo_user_follow_up(&mut app, "please continue");
        assert_eq!(app.agent_focus.as_ref().unwrap().local_cells.len(), 1);
        seed_resident_transcript(
            &mut app,
            "agent_echo",
            json!([
                {"role": "assistant", "content": [{"type": "text", "text": "ready", "cache_control": null}]},
                {"role": "user", "content": [{"type": "text", "text": "please continue", "cache_control": null}]}
            ]),
        );
        // Force the cadence gate open.
        app.agent_focus.as_mut().unwrap().last_refresh = Instant::now() - REFRESH_INTERVAL;
        refresh_focus(&mut app);
        let focus = app.agent_focus.as_ref().unwrap();
        assert_eq!(focus.source_message_count, 2);
        assert!(focus.local_cells.is_empty(), "echo dropped once carried");
    }

    #[test]
    fn scrolling_pages_through_the_focused_transcript_and_returns_to_tail() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let messages: Vec<serde_json::Value> = (0..40)
            .map(|i| json!({"role": "assistant", "content": [{"type": "text", "text": format!("line {i}"), "cache_control": null}]}))
            .collect();
        seed_resident_transcript(&mut app, "agent_long", json!(messages));
        focus_agent(&mut app, "agent_long");
        let tail = render(&mut app, 60, 10);
        assert!(tail.contains("line 39"), "{tail}");
        app.scroll_up(1000);
        let head = render(&mut app, 60, 10);
        assert!(head.contains("line 0"), "{head}");
        assert!(app.agent_focus.as_ref().unwrap().scroll_top.is_some());
        app.scroll_down(100_000);
        let back = render(&mut app, 60, 10);
        assert!(back.contains("line 39"), "{back}");
        assert!(app.agent_focus.as_ref().unwrap().scroll_top.is_none());
    }

    #[test]
    fn continued_fork_moves_focus_to_the_new_agent_and_reports_it() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        focus_agent(&mut app, "agent_done");
        echo_user_follow_up(&mut app, "one more thing");
        let outcome = Ok(codewhale_engine::tools::subagent::UserFollowUpOutcome {
            agent_id: "agent_done".to_string(),
            target_agent_id: "agent_fork".to_string(),
            delivered: true,
            resumed: true,
            note: "continued".to_string(),
        });
        apply_follow_up_receipt(&mut app, "agent_done", &outcome);
        let focus = app.agent_focus.as_ref().expect("focus follows the fork");
        assert_eq!(focus.agent_id, "agent_fork");
        assert!(
            focus.local_cells.iter().any(
                |cell| matches!(cell, HistoryCell::User { content } if content == "one more thing")
            ),
            "echo carried across the fork"
        );
        assert!(
            focus
                .local_cells
                .iter()
                .any(|cell| matches!(cell, HistoryCell::System { .. })),
            "receipt shown in the focused view"
        );
        let failed: Result<codewhale_engine::tools::subagent::UserFollowUpOutcome, String> =
            Err("status is cancelled".to_string());
        apply_follow_up_receipt(&mut app, "agent_fork", &failed);
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|status| status.contains("status is cancelled"))
        );
    }

    /// U05-05: a delayed follow-up receipt for another agent must not be
    /// appended to whichever transcript happens to be focused when it lands.
    #[test]
    fn delayed_follow_up_receipt_stays_out_of_another_agents_focus() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        focus_agent(&mut app, "agent_b");
        let outcome = Ok(codewhale_engine::tools::subagent::UserFollowUpOutcome {
            agent_id: "agent_a".to_string(),
            target_agent_id: "agent_a".to_string(),
            delivered: true,
            resumed: false,
            note: "delivered".to_string(),
        });
        apply_follow_up_receipt(&mut app, "agent_a", &outcome);
        let focus = app.agent_focus.as_ref().expect("focus kept");
        assert_eq!(focus.agent_id, "agent_b");
        assert!(
            focus.local_cells.is_empty(),
            "another agent's receipt leaked into this transcript: {:?}",
            focus.local_cells
        );
        assert!(app.status_message.is_some(), "the toast still reports it");
    }

    #[test]
    fn scroll_to_bottom_returns_the_focused_transcript_to_its_tail() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let messages: Vec<serde_json::Value> = (0..40)
            .map(|i| json!({"role": "assistant", "content": [{"type": "text", "text": format!("line {i}"), "cache_control": null}]}))
            .collect();
        seed_resident_transcript(&mut app, "agent_tail", json!(messages));
        focus_agent(&mut app, "agent_tail");
        let _ = render(&mut app, 60, 10);
        app.scroll_up(5);
        let _ = render(&mut app, 60, 10);
        assert!(app.agent_focus.as_ref().unwrap().scroll_top.is_some());

        // The main transcript's jump-to-bottom affordances (Ctrl+End, the
        // jump-to-latest button) route through `App::scroll_to_bottom`; the
        // focused pane shares those keys, so it must return to its own tail.
        app.scroll_to_bottom();
        let screen = render(&mut app, 60, 10);
        assert!(
            app.agent_focus.as_ref().unwrap().scroll_top.is_none(),
            "jump-to-bottom must release the focused pane's pin"
        );
        assert!(screen.contains("line 39"), "{screen}");
    }

    #[test]
    fn main_conversation_activity_keeps_a_pinned_focused_transcript_pinned() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let messages: Vec<serde_json::Value> = (0..40)
            .map(|i| json!({"role": "assistant", "content": [{"type": "text", "text": format!("line {i}"), "cache_control": null}]}))
            .collect();
        seed_resident_transcript(&mut app, "agent_pin", json!(messages));
        focus_agent(&mut app, "agent_pin");
        let _ = render(&mut app, 60, 10);
        app.scroll_up(10);
        let _ = render(&mut app, 60, 10);
        assert!(app.agent_focus.as_ref().unwrap().scroll_top.is_some());

        // Turn completion clears the per-turn scroll lock while the focused
        // pane keeps its pin — the state the auto-follow guard must respect.
        app.user_scrolled_during_stream = false;
        app.add_message(HistoryCell::System {
            content: "worker finished".to_string(),
        });

        assert!(
            app.agent_focus.as_ref().unwrap().scroll_top.is_some(),
            "main-conversation activity must not yank the pinned focused pane to its tail"
        );
    }

    #[test]
    fn focused_transcript_follows_new_child_activity_while_at_tail() {
        let tmp = tempdir().expect("tempdir");
        let mut app = test_app(tmp.path().to_path_buf());
        let seed = |count: usize| {
            let messages: Vec<serde_json::Value> = (0..count)
                .map(|i| json!({"role": "assistant", "content": [{"type": "text", "text": format!("line {i}"), "cache_control": null}]}))
                .collect();
            json!(messages)
        };
        seed_resident_transcript(&mut app, "agent_live", seed(40));
        focus_agent(&mut app, "agent_live");
        let first = render(&mut app, 60, 10);
        assert!(first.contains("line 39"), "{first}");
        assert!(app.agent_focus.as_ref().unwrap().scroll_top.is_none());

        // The child streams on; while the pane sits at its tail the new
        // activity must pull the viewport down with it.
        seed_resident_transcript(&mut app, "agent_live", seed(60));
        app.agent_focus.as_mut().unwrap().last_refresh = Instant::now() - REFRESH_INTERVAL;
        refresh_focus(&mut app);
        let second = render(&mut app, 60, 10);
        assert!(second.contains("line 59"), "{second}");
        assert!(
            app.agent_focus.as_ref().unwrap().scroll_top.is_none(),
            "following the tail must not flip into a pinned offset"
        );
    }
}

#[cfg(test)]
#[path = "agent_focus/transcript_legacy.rs"]
mod transcript_legacy;

#[cfg(test)]
mod mounted_transcript_acceptance {
    use super::*;
    use codewhale_localization::Locale;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn fixture(locale: Locale, offset: Option<usize>) -> App {
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(PathBuf::from(".")),
            &Config::default(),
        );
        app.launch.visible = false;
        app.ui_locale = locale;
        app.low_motion = true;
        let mut focus = AgentFocus::new("worker".into(), "Builder 中".into());
        focus.omitted_messages = 2;
        focus.cells = vec![
            HistoryCell::User { content: "Review the changed source".into() },
            HistoryCell::Assistant { content: "A child receipt with 鲸鱼 cafe\u{0301} and wrapped source.\n\n```rust\nlet x = 1;\n```".into(), streaming: true },
        ];
        focus.local_cells.push(HistoryCell::User {
            content: "Keep this local follow-up".into(),
        });
        focus.scroll_top = offset;
        app.agent_focus = Some(focus);
        app
    }
    #[test]
    fn mounted_transcript_focused_child_banner_and_scroll_match_frozen_painter() {
        for &locale in Locale::shipped() {
            for offset in [None, Some(0), Some(2)] {
                let (mut actual_app, mut old_app) =
                    (fixture(locale, offset), fixture(locale, offset));
                for width in [20, 40, 80, 120] {
                    for height in [1, 3, 6, 12] {
                        let area = Rect::new(7, 5, width, height);
                        let mut actual = Buffer::empty(Rect::new(2, 3, width + 12, height + 8));
                        for cell in &mut actual.content {
                            cell.set_symbol("~");
                        }
                        let mut expected = actual.clone();
                        transcript_legacy::render_focus(&mut old_app, area, &mut expected);
                        render_focus(&mut actual_app, area, &mut actual);
                        assert_eq!(
                            actual, expected,
                            "locale={locale:?} offset={offset:?} area={area:?}"
                        );
                        assert_eq!(
                            actual_app.viewport.last_transcript_area,
                            old_app.viewport.last_transcript_area
                        );
                        assert_eq!(
                            actual_app.viewport.last_transcript_top,
                            old_app.viewport.last_transcript_top
                        );
                        assert_eq!(
                            actual_app.viewport.last_transcript_total,
                            old_app.viewport.last_transcript_total
                        );
                        assert_eq!(
                            actual_app.viewport.last_transcript_visible,
                            old_app.viewport.last_transcript_visible
                        );
                        assert_eq!(
                            actual_app.agent_focus.as_ref().unwrap().scroll_top,
                            old_app.agent_focus.as_ref().unwrap().scroll_top
                        );
                    }
                }
            }
        }
    }
}
