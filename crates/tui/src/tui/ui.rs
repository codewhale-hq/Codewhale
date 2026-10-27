//! TUI event loop and rendering logic for `DeepSeek` CLI.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::future::Future;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope, ErrorSeverity};
use anyhow::{Context, Result};
use codewhale_config::AppMode;
use codewhale_core::ContextReference;
use codewhale_execpolicy::ApprovalMode;
use codewhale_release::InstallMethod;
use codewhale_runtime::resource_telemetry::estimate_output_tokens_from_text;
// On Windows the push/pop helpers write the escapes directly; crossterm's
// PushKeyboardEnhancementFlags / PopKeyboardEnhancementFlags commands are
// never referenced, so the imports are gated to avoid -D warnings failures.
#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout, Rect, Size},
    prelude::Widget,
    style::Style,
    widgets::Block,
};
use tracing;
#[cfg(target_os = "windows")]
use windows::Win32::System::Console::{GetConsoleMode, GetStdHandle, SetConsoleMode};

use crate::automation_manager::{AutomationManager, AutomationSchedulerConfig, spawn_scheduler};
use crate::client::{
    CACHE_WARMUP_MAX_TOKENS, CacheWarmupKey, CodewhaleClient, PromptInspection,
    build_cache_warmup_request, inspect_prompt_for_request,
};
use crate::commands;
use crate::compaction::CompactionConfig;
use crate::compaction::{estimate_input_tokens_conservative, estimate_tokens};
use crate::core::engine::{EngineConfig, EngineHandle, spawn_engine};
use crate::core::events::Event as EngineEvent;
use crate::core::ops::{Op, ProviderRuntimeStatus, UserInputProvenance};
use crate::hooks::{HookExecutor, TurnEndPayloadInput, TurnEndTotals};
use crate::llm_client::LlmClient;
use crate::prompts;
#[cfg(test)]
use crate::route_runtime::resolve_runtime_route;
use crate::route_runtime::resolve_runtime_route_for_identity;
#[cfg(test)]
use crate::session_manager::create_saved_session_with_id_and_mode;
use crate::session_manager::{
    OfflineQueueState, QueuedSessionMessage, SavedSession, SessionManager,
};
use crate::task_manager::{
    NewTaskRequest, SharedTaskManager, TaskManager, TaskManagerConfig, TaskStatus, TaskSummary,
};
use crate::tools::goal::{GoalSnapshot, GoalStatus};
use crate::tools::shell::{ShellJobSnapshot, ShellStatus};
use crate::tools::spec::{RuntimeToolServices, ToolResult};
use crate::tools::subagent::{MailboxMessage, SubAgentStatus, subagent_progress_tool_display_name};
use crate::tui::auto_router;
use crate::tui::clipboard::ClipboardContent;
use crate::tui::color_compat::ColorCompatBackend;
use crate::tui::command_palette::{
    CommandPaletteView, build_entries_with_plugins as build_command_palette_entries,
};
use crate::tui::composer_ui::*;
use crate::tui::context_inspector::ContextInspectorView;
use crate::tui::event_broker::EventBroker;
use crate::tui::file_picker_relevance;
use crate::tui::footer_ui::friendly_subagent_progress;
use crate::tui::format_helpers;
use crate::tui::hotbar::actions::HotbarDispatch;
use crate::tui::key_shortcuts;
use crate::tui::live_transcript::LiveTranscriptOverlay;
use crate::tui::mcp_routing::{add_mcp_message, open_mcp_extensions};
use crate::tui::mouse_ui::*;
use crate::tui::notifications;
use crate::tui::onboarding;
use crate::tui::pager::PagerView;
use crate::tui::persistence_actor::{self, PersistRequest};
use crate::tui::scrolling::TranscriptScroll;
use crate::turn_route_plan::{PlannedTurnRoute, TurnRoutePlanRequest, plan_turn_route};
use crate::work_graph::task_owner_snapshot;
use codewhale_config::ProviderKind;
use codewhale_config::hooks::HookEvent;
use codewhale_localization::{MessageId, tr};
use codewhale_models::{ContentBlock, Message, MessageRequest, SystemPrompt, Usage};
use codewhale_palette as palette;
use codewhale_runtime::audit::log_sensitive_event;
#[cfg(test)]
use codewhale_runtime::config::ProviderConfig;
use codewhale_runtime::config::{
    Config, ProviderIdentity, ProvidersConfig, StatusItem, UpdateConfig,
    persist_external_credential_consent_for_at, revoke_external_credential_consent_for_at,
};
use codewhale_runtime::settings::Settings;
// SelectionAutoscroll unused
use crate::tui::motion::{FrameRequester, MotionMode};
use crate::tui::session_picker::SessionPickerView;
use crate::tui::shell_job_routing::{
    add_shell_job_message, format_shell_job_list, format_shell_poll, open_shell_job_pager,
};
use crate::tui::streaming::StreamDisplayClock;
use crate::tui::streaming_thinking;
use crate::tui::subagent_routing::{
    apply_subagent_terminal_projection, format_task_list, handle_subagent_mailbox_for_turn,
    open_task_pager, parent_stop_status, reconcile_subagent_activity_state, running_agent_count,
    sort_subagents_in_place, subagent_message_refreshes_workspace_context, task_mode_label,
    task_summary_to_panel_entry,
};
#[cfg(test)]
use crate::tui::subagent_routing::{handle_subagent_mailbox, reconcile_subagent_activity_state_at};
#[cfg(test)]
use crate::tui::tool_routing::exploring_label;
use crate::tui::tool_routing::{
    apply_owned_workflow_ui_event, handle_tool_call_complete, handle_tool_call_started,
};
use crate::tui::ui_text::history_cell_to_text;
use crate::tui::user_input::UserInputView;
use crate::tui::views::subagent_view_agents;
use crate::tui::vim_mode;
use crate::tui::workspace_context;

use crate::reasoning_preference::{EffectiveReasoningEffort, ReasoningEffort};

use super::key_actions;

use super::app::{
    ActiveCompaction, ActiveTurnMetadata, AgentCurrentActivity, App, AppAction,
    ComposerSubmitAction, ComposerSubmitChord, GoalControlIntent, OnboardingState,
    PendingGoalControl, PendingProviderSwitch, QueuedMessage, RedactionGateNotice, ScreenMode,
    StatusToast, StatusToastLevel, SubmitDisposition, TaskPanelEntry, TaskPanelEntryKind,
    ToolEvidence, TuiOptions, bound_agent_activity_text, is_stop_word,
    looks_like_slash_command_input, shell_command_from_bang_input,
};
use super::approval::{
    ApprovalRequest, ApprovalView, ElevationRequest, ElevationView, ReviewDecision,
};
use super::history::{
    CellFoldAction, ExecCell, HistoryCell, ToolCell, ToolStatus, TranscriptFold,
    history_cells_from_message, summarize_tool_output,
};
use super::slash_menu::{
    apply_slash_menu_selection, partial_inline_skill_mention_at_cursor,
    try_autocomplete_slash_command, visible_slash_menu_entries,
};
use super::views::{ConfigView, HelpView, ModalKind, ViewAction, ViewEvent};
use super::widgets::pending_input_preview::{ContextPreviewItem, PendingInputPreview};
use super::widgets::{ChatWidget, ComposerWidget, Renderable};

// Activity Detail / raw-detail / pager-text helpers extracted into `activity_detail`
// (issue #4103). Re-export the cross-module entry points so existing
// `crate::tui::ui::{...}` importers (mouse_ui, footer_ui) keep resolving, and
// import the ui-internal entry points used from this file's own body.
pub(crate) use self::activity_detail::{
    completed_assistant_answer_text, copy_cell_to_clipboard, detail_target_label,
    open_details_pager_for_cell, open_focused_cell_pager, turn_handoff_markdown,
};
use self::activity_detail::{
    copy_focused_cell, detail_target_cell_index, extract_reasoning_header,
    open_reasoning_detail_pager, open_tool_details_pager, open_turn_inspector_pager,
};
// Ctrl+O now opens the full recorded Reasoning Detail for the selected or
// current reasoning block. The whole-turn Turn Inspector moved to Ctrl+Alt+O
// and `/turn inspect`. (`v` raw leaf detail keeps using `open_tool_details_pager`.)

// === Constants ===

/// Upper bound on slash-menu entries returned to the renderer. The composer's
/// render path already paginates with center-tracking (see
/// `widgets::ComposerWidget::render`), so this only needs to be high enough to
/// encompass the full filtered command list — never the visible-row budget.
/// Bumped from 6 to 128 to fix #64 (selection couldn't reach commands beyond
/// the visible window because the source list itself was capped).
const SLASH_MENU_LIMIT: usize = 128;
const MIN_CHAT_HEIGHT: u16 = 3;
const MIN_COMPOSER_HEIGHT: u16 = 2;
use codewhale_runtime::context_budget::{
    CONTEXT_CRITICAL_THRESHOLD_PERCENT, CONTEXT_WARNING_THRESHOLD_PERCENT,
};
const CONTEXT_SUGGEST_COMPACT_THRESHOLD_PERCENT: f64 = 60.0;
const UI_IDLE_POLL_MS: u64 = 48;
const UI_ACTIVE_POLL_MS: u64 = 24;
const SUBAGENT_HOOK_PREVIEW_LIMIT: usize = 2_048;
const DISPATCH_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall-clock time a turn may stay in `"in_progress"` with no activity before
/// the UI assumes the engine stalled (sub-agent hang, lost completion event,
/// engine panic) — unless the engine heartbeat reports a live bounded wait.
const TURN_STALL_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(300);
/// Running tools can legitimately exceed the silent-turn timeout, but a tool
/// with no progress heartbeat or output beyond this ceiling is treated as hung.
// Must stay comfortably above `turn_stall_watchdog_timeout` so a running tool
// gets extra grace beyond the turn-stall threshold (#1862 trimmed 15m → 10m).
const TOOL_HANG_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(600);
// Forced repaint cadence while a turn is live (model loading, compacting,
// sub-agents running). Drives the footer water-spout animation as well as
// the per-tool spinner pulse — keep this fast enough that the whale-spout
// braille pattern reads as continuous motion instead of teleport-frames.
const UI_STATUS_ANIMATION_MS: u64 = crate::tui::spinner::BRAILLE_SPINNER_FRAME_MS;
const UI_QUIET_STATUS_ANIMATION_MS: u64 = 1_000;
/// Ambient fish, the idle-mark caustic, and the completion wake use a modest
/// ~12.5fps clock by default. On measured high-Hz displays the adaptive probe
/// may raise this (still bounded); low_motion always freezes the cadence.
/// Active markers run at 8fps; atmosphere stays subordinate.
pub(crate) const UI_UNDERWATER_ANIMATION_MS: u64 = 80;
/// Full-motion compatibility cadence for VTE, tmux, and other terminals that
/// explicitly request the 30 FPS safety cap.
pub(crate) const UI_CONSTRAINED_UNDERWATER_ANIMATION_MS: u64 = 34;
/// 30 FPS Ghostty atmosphere clock. Input, streaming, and other interactive
/// state still request immediate frames up to the separate 60 FPS draw cap;
/// idle water no longer forces a full-screen repaint at that rate.
pub(crate) const UI_GHOSTTY_UNDERWATER_ANIMATION_MS: u64 = 34;
// Minimum chat-host width at which the file-tree pane renders. At an
// 80-column terminal the file tree owns 20 columns, leaving a 60-column chat
// host; below this floor the tree is hidden rather than squeezing the
// transcript under 40 columns. (Named for the file tree — the legacy sidebar
// this constant once described no longer gates on it.)
pub(crate) const FILE_TREE_MIN_HOST_WIDTH: u16 = 60;
const SESSION_TITLE_MAX_CHARS: usize = 32;
const VERSION_HINT_TOAST_TTL_MS: u64 = 12_000;

const REQUIRED_RELEASE_ASSETS: &[&str] = &[
    "codewhale-linux-x64",
    "codew-linux-x64",
    "codewhale-linux-arm64",
    "codew-linux-arm64",
    "codewhale-android-arm64",
    "codew-android-arm64",
    "codewhale-macos-x64",
    "codew-macos-x64",
    "codewhale-macos-arm64",
    "codew-macos-arm64",
    "codewhale-windows-x64.exe",
    "codew-windows-x64.exe",
    "codewhale.bat",
    "codewhale-windows-arm64.exe",
    "codew-windows-arm64.exe",
    "codewhale-linux-x64.tar.gz",
    "codewhale-linux-arm64.tar.gz",
    "codewhale-android-arm64.tar.gz",
    "codewhale-macos-x64.tar.gz",
    "codewhale-macos-arm64.tar.gz",
    "codewhale-windows-x64.zip",
    "codewhale-windows-x64-portable.zip",
    "codewhale-windows-arm64.zip",
    "codewhale-windows-arm64-portable.zip",
    "CodeWhaleSetup.exe",
    "codewhale-bundles-sha256.txt",
    "codewhale-artifacts-sha256.txt",
];

type AppTerminal = Terminal<ColorCompatBackend<Stdout>>;

type PendingToolUses = Vec<ContentBlock>;

#[derive(Debug)]
enum TranslationEvent {
    AssistantMessage {
        origin_session_fingerprint: Option<String>,
        origin_turn_fingerprint: Option<String>,
        history_index: Option<usize>,
        original_text: String,
        translated: anyhow::Result<String>,
        usage: Option<codewhale_models::Usage>,
        thinking: Option<String>,
        tool_uses: PendingToolUses,
    },
    Thinking {
        origin_session_fingerprint: Option<String>,
        origin_turn_fingerprint: Option<String>,
        translation_id: u64,
        translated: anyhow::Result<String>,
        usage: Option<codewhale_models::Usage>,
    },
}

// Reset scroll region (`\x1b[r`), origin mode (`\x1b[?6l`), and home the cursor
// (`\x1b[H`) before letting ratatui's diff renderer repaint. The destructive
// `\x1b[2J\x1b[3J` pair was previously appended here to also wipe the visible
// screen and saved scrollback, but combined with the immediately-following
// `terminal.clear()` it produced a double-clear that several terminals
// (Ghostty, VSCode terminal, Win10 conhost) render as visible flicker on every
// TurnComplete / focus-gain / resize. The alt-screen buffer's double-buffering
// plus ratatui's `terminal.clear()` are sufficient to repaint cleanly.
const TERMINAL_ORIGIN_RESET: &[u8] = b"\x1b[r\x1b[?6l\x1b[H";
// Xterm alternate-scroll mode (DECSET 1007) converts wheel input into arrow
// keys. It is only meaningful when mouse reporting is unavailable; while
// mouse capture is active the terminal must deliver wheel events as mouse
// events, so 1007 stays off (iTerm2 converts anyway, breaking transcript
// wheel-scroll — #5223). `--no-mouse-capture` also keeps it off so the host
// terminal owns raw mouse selection behavior end-to-end (#4026).
const ENABLE_ALT_SCROLL_MODE: &[u8] = b"\x1b[?1007h";
const DISABLE_ALT_SCROLL_MODE: &[u8] = b"\x1b[?1007l";
/// Begin synchronized update (DEC 2026): tell the terminal to defer
/// rendering until END_SYNC_UPDATE is received. Best-effort —
/// terminals that don't support this silently ignore the sequence.
/// Reduces flicker on GPU-accelerated terminals (Ghostty, VSCode
/// Terminal, Kitty, WezTerm) by batching ratatui's incremental
/// diff writes into a single frame.
const BEGIN_SYNC_UPDATE: &[u8] = b"\x1b[?2026h";
/// End synchronized update (DEC 2026): tell the terminal to render
/// the complete frame now.
const END_SYNC_UPDATE: &[u8] = b"\x1b[?2026l";
/// Throttled in-progress checkpoint while a turn is live (#1830 progress loss).
const RECOVERY_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(45);

/// Where a key goes while onboarding owns the screen (#4763).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnboardingKeyRoute {
    /// Terminate the session. Ctrl+C is unconditional during onboarding.
    Quit,
    /// Hand the key to the provider picker on the view stack.
    ProviderPicker,
    /// Take the advertised offline exit (#3927). Reachable from Provider
    /// setup even while the provider picker owns the screen, so the choice is
    /// never hidden behind a modal the user cannot satisfy.
    ExploreOffline,
    /// Fall through to the legacy onboarding key switch.
    Legacy,
}

fn surface_prompt_override_notices(app: &mut App) {
    for notice in prompts::take_prompt_override_notices() {
        app.add_message(HistoryCell::System {
            content: format!("Warning: {notice}"),
        });
        app.push_status_toast(notice, StatusToastLevel::Warning, Some(12_000));
    }
}

#[cfg(test)]
#[test]
fn tui_launch_preflight_explains_non_tty_failure() {
    assert!(require_interactive_terminal(true, true).is_ok());
    for (stdin_is_tty, stdout_is_tty) in [(false, true), (true, false), (false, false)] {
        let err = require_interactive_terminal(stdin_is_tty, stdout_is_tty)
            .expect_err("a missing TTY must fail before raw mode");
        let message = err.to_string();
        assert!(message.contains("interactive terminal"), "{message}");
        assert!(message.contains("codewhale exec"), "{message}");
    }
}

#[cfg(unix)]
#[test]
fn tui_launch_preflight_rejects_background_process_group() {
    assert!(validate_foreground_process_group(41, 41).is_ok());
    let err = validate_foreground_process_group(41, 42)
        .expect_err("a background process group must fail before raw mode");
    let message = err.to_string();
    assert!(message.contains("background or suspended"), "{message}");
    assert!(message.contains("Run `fg`"), "{message}");
    assert!(message.contains("codewhale exec"), "{message}");
}

fn resume_hint_text(
    locale: codewhale_localization::Locale,
    session_id: Option<&str>,
    terminal_output: bool,
) -> Option<String> {
    use codewhale_localization::{MessageId, tr};
    if !terminal_output {
        return None;
    }
    let session_id = session_id.filter(|id| !id.trim().is_empty())?;
    // Reconstruct a canonical UUID rather than interpolating a stored string
    // into a shell command or terminal output. Legacy/noncanonical identities
    // get the existing picker, never an ambiguous "most recent" shortcut.
    let canonical = uuid::Uuid::parse_str(session_id)
        .ok()
        .map(|id| id.hyphenated().to_string())
        .filter(|id| id == session_id);
    let (message, command) = match canonical {
        Some(id) => (
            MessageId::ResumeExactSessionHint,
            format!("codewhale resume {id}"),
        ),
        None => (
            MessageId::ResumeSavedSessionHint,
            "codewhale resume".to_string(),
        ),
    };
    Some(tr(locale, message).replace("{command}", &command))
}

struct TerminalCleanupGuard {
    use_bracketed_paste: bool,
    defused: bool,
}

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        if self.defused {
            return;
        }

        let mut stdout = io::stdout();
        pop_keyboard_enhancement_flags(&mut stdout);
        disable_alternate_scroll_mode(&mut stdout);
        let _ = execute!(stdout, DisableFocusChange);
        let _ = disable_raw_mode();
        // The live screen, not the one startup chose: `/inline` moves it.
        if live_alt_screen() {
            let _ = leave_alt_screen(&mut stdout);
        }
        // Capture follows the screen at runtime too; disabling it when it was
        // never on is harmless (the emergency path already does).
        let _ = execute!(stdout, DisableMouseCapture);
        if self.use_bracketed_paste {
            disable_bracketed_paste_mode(&mut stdout);
        }
        let _ = execute!(stdout, crossterm::cursor::Show);
    }
}

/// Recognise composer input that is a `# foo` memory quick-add (#492).
///
/// Returns `true` for inputs that:
/// - start with `#`,
/// - have at least one non-whitespace character after the leading `#`,
/// - are a single line (no embedded `\n`), and
/// - are not a shebang (`#!`) or Markdown heading (`## …`, `### …`).
///
/// Multi-`#` prefixes are deliberately rejected so users can paste
/// Markdown headings into the composer without triggering the quick-add.
#[must_use]
fn is_memory_quick_add(input: &str) -> bool {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('#') {
        return false;
    }
    if trimmed.starts_with("##") || trimmed.starts_with("#!") {
        return false;
    }
    if input.contains('\n') {
        return false;
    }
    // Require something after the `#`.
    !trimmed.trim_start_matches('#').trim().is_empty()
}

fn should_intercept_memory_quick_add(config: &Config, input: &str) -> bool {
    config.memory_enabled() && is_memory_quick_add(input)
}

#[cfg(test)]
mod memory_quick_add_tests {
    use super::should_intercept_memory_quick_add;
    use codewhale_runtime::config::Config;

    #[test]
    fn memory_quick_add_interception_requires_memory_opt_in() {
        let enabled: Config = toml::from_str(
            r#"
            [memory]
            enabled = true
            "#,
        )
        .expect("parse enabled memory config");
        assert!(should_intercept_memory_quick_add(
            &enabled,
            "# remember this"
        ));

        let disabled: Config = Config::default();
        assert!(!should_intercept_memory_quick_add(
            &disabled,
            "# remember this"
        ));
        assert!(!should_intercept_memory_quick_add(
            &enabled,
            "## Markdown heading"
        ));
    }
}

fn spawn_tui_engine(
    app: &mut App,
    api_config: &Config,
    mcp_handoff: Option<crate::mcp::McpHandoff>,
) -> EngineHandle {
    let handle = spawn_engine(
        build_engine_config(app, api_config),
        api_config,
        mcp_handoff,
    );
    app.mcp_snapshot_generation = 0;
    app.mcp_snapshot_generation_invalidated = false;
    // Prime durable agent + coordination state through the same engine event
    // used by later refreshes. All TUI engine replacements use this wrapper,
    // so workspace switches and provider recovery cannot retain stale Work.
    let _ = handle.try_send(Op::ListSubAgents);
    handle
}

async fn retire_tui_engine(
    app: &mut App,
    engine_handle: &EngineHandle,
) -> Option<crate::mcp::McpHandoff> {
    let mcp_handoff = engine_handle.retire_mcp_for_handoff();
    let active = app
        .active_compaction
        .as_ref()
        .map(|active| (active.id.clone(), active.auto));
    let queued = (app.manual_compaction_queued && app.deferred_manual_compaction.is_none())
        .then(|| app.manual_compaction_id.clone().unwrap_or_default());
    for id in active.iter().map(|(id, _)| id).chain(queued.iter()) {
        let _ = engine_handle.cancel_compaction(id.clone());
    }
    let _ = engine_handle.send(Op::Shutdown).await;
    if active.is_some() || queued.is_some() {
        let (id, auto) = active.unwrap_or_else(|| (queued.unwrap_or_default(), false));
        app.manual_compaction_queued = false;
        app.manual_compaction_id = None;
        let text = app
            .tr(MessageId::ContextCompactionStoppedOnEngineSwitch)
            .into_owned();
        apply_compaction_cancelled(app, &id, auto, text);
    }
    if std::mem::take(&mut app.is_purging) {
        let text = app
            .tr(MessageId::ContextPurgeAbandonedOnEngineSwitch)
            .into_owned();
        add_compaction_receipt(app, &text);
        app.push_status_toast(text, StatusToastLevel::Warning, Some(5_000));
    }
    mcp_handoff
}

async fn replace_tui_engine(app: &mut App, api_config: &Config, engine_handle: &mut EngineHandle) {
    let mcp_handoff = retire_tui_engine(app, engine_handle).await;
    *engine_handle = spawn_tui_engine(app, api_config, mcp_handoff);
}

/// Startup and consent-triggered replacement restore the same conversation
/// before admitting any pending input. The existing engine remains the sole
/// owner of model-facing history and the frozen system prefix.
async fn spawn_tui_engine_with_session(
    app: &mut App,
    config: &Config,
    mcp_handoff: Option<crate::mcp::McpHandoff>,
) -> Result<EngineHandle> {
    let handle = spawn_tui_engine(app, config, mcp_handoff);
    let restored = async {
        if !app.api_messages.is_empty() {
            handle
                .send(Op::SyncSession {
                    session_id: app.current_session_id.clone(),
                    messages: app.api_messages.as_ref().clone(),
                    system_prompt: app.system_prompt.clone(),
                    system_prompt_override: false,
                    model: app.model.clone(),
                    workspace: app.workspace.clone(),
                    mode: app.mode,
                })
                .await?;
        }
        // FIFO snapshot acknowledgement also proves the restore was processed.
        let snapshot = handle.get_session_snapshot().await?;
        app.system_prompt = snapshot.system_prompt;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = restored {
        let _ = handle.send(Op::Shutdown).await;
        return Err(error);
    }
    Ok(handle)
}

fn configured_instruction_sources(config: &Config) -> Vec<prompts::InstructionSource> {
    config
        .instructions_paths()
        .into_iter()
        .map(Into::into)
        .collect()
}

/// Open the exact effective base-prompt preview (#3928).
///
/// Assembles the prompt through [`build_app_system_prompt_with_goal`] — the same
/// function the dispatch path calls — so the preview is the next turn's bytes,
/// not a reconstruction of them. Nothing is sent and no tool catalog is
/// expanded; the preview is a pure read.
fn preview_effective_base_prompt(app: &mut App, config: &Config) {
    use crate::prompts::base_preview;

    let prompt = build_app_system_prompt_with_goal(app, config, app.goal.objective.as_deref());
    let home = codewhale_config::codewhale_home().ok();
    let constitution_path = codewhale_config::UserConstitution::path().ok();
    let sources = base_preview::PreviewSources {
        base_prompt: Some(crate::prompts::effective_base_prompt_source(
            home.as_deref(),
        )),
        user_constitution_path: constitution_path.as_deref(),
        workspace: Some(app.workspace.as_path()),
        home: home.as_deref(),
    };
    let report = base_preview::render_report(&base_preview::preview(&prompt, &sources));
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    app.view_stack.push(crate::tui::pager::PagerView::from_text(
        crate::prompts::base_preview::PREVIEW_TITLE,
        &report,
        width.saturating_sub(2),
    ));
}

/// Minimum interval between balance API fetches to avoid flooding.
const BALANCE_FETCH_COOLDOWN: Duration = Duration::from_secs(60);

/// Shared `reqwest::Client` for balance fetches so connection pools are
/// reused across successive background polls.
static BALANCE_CLIENT: LazyLock<::reqwest::Client> = LazyLock::new(|| {
    codewhale_release::tls::reqwest_client_builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
});

#[derive(Debug)]
pub(crate) struct CacheWarmupOutcome {
    usage: Usage,
    provider_identity: String,
    model: String,
    base_url: String,
    inspection: PromptInspection,
}

/// Install a completed constitution draft into the setup wizard (if still on
/// top) and open its ratification preview, or surface a failure. Called from
/// the event loop when the background draft lands, and directly on the
/// pre-spawn provider-construction failure.
fn deliver_constitution_draft_result(
    app: &mut App,
    model_label: String,
    locale: codewhale_localization::Locale,
    outcome: Result<Box<codewhale_config::UserConstitution>, String>,
) {
    match outcome {
        Ok(constitution) => {
            if app.view_stack.top_kind() == Some(ModalKind::SetupWizard)
                && let Some(mut boxed) = app.view_stack.pop()
            {
                let preview = boxed
                    .as_any_mut()
                    .downcast_mut::<crate::tui::setup::SetupWizardView>()
                    .and_then(|wizard| {
                        wizard.install_model_draft(constitution, model_label.clone())
                    });
                app.view_stack.push_boxed(boxed);
                if let Some((title, content)) = preview {
                    open_text_pager(app, title, content);
                    app.status_message = Some(crate::tui::setup::model_draft_ready_message(
                        locale,
                        &model_label,
                    ));
                }
            }
        }
        Err(reason) => {
            app.status_message = Some(crate::tui::setup::model_draft_failed_message(
                locale,
                &model_label,
                &reason,
            ));
        }
    }
    app.needs_redraw = true;
}

/// Install a completed fleet-profile draft into the wizard (if it is still on
/// top), or surface a failure. Called from the event loop when the
/// background draft lands, and directly on the pre-spawn
/// provider-construction failure.
///
/// The preview renders inline on the wizard's own Review step — deliberately
/// NOT in a separate pager (#4093): a standalone pager view owns its own
/// `g`/`G` scroll bindings and would swallow the ratify keypress, forcing an
/// Esc-then-g round trip before the user could actually save.
fn deliver_fleet_draft_result(
    app: &mut App,
    model_label: String,
    picked_route: Option<(String, String)>,
    reasoning_effort: Option<String>,
    outcome: Result<Box<crate::fleet::profile::FleetProfileDraft>, String>,
    locale: codewhale_localization::Locale,
) {
    match outcome {
        Ok(draft) => {
            if app.view_stack.top_kind() == Some(ModalKind::FleetSetup)
                && let Some(mut boxed) = app.view_stack.pop()
            {
                let installed = boxed
                    .as_any_mut()
                    .downcast_mut::<crate::tui::views::fleet_setup::FleetSetupView>()
                    .and_then(|wizard| {
                        wizard.install_model_draft(
                            draft,
                            model_label.clone(),
                            picked_route.clone(),
                            reasoning_effort.clone(),
                        )
                    })
                    .is_some();
                app.view_stack.push_boxed(boxed);
                if installed {
                    app.status_message = Some(match locale {
                        codewhale_localization::Locale::ZhHans => {
                            format!("{model_label} 已起草配置。请查看下方 TOML，然后按 g 保存。")
                        }
                        _ => format!(
                            "{model_label} drafted the profile. Review the TOML below, then press g to save."
                        ),
                    });
                }
            }
        }
        Err(reason) => {
            app.status_message = Some(match locale {
                codewhale_localization::Locale::ZhHans => {
                    format!("{model_label} 未能起草配置（{reason}）。按 Enter 仍会插入编写提示。")
                }
                _ => format!(
                    "{model_label} could not draft the profile ({reason}). Enter still inserts the authoring prompt."
                ),
            });
        }
    }
    app.needs_redraw = true;
}

// `format_*` chip/message builders moved to `tui/format_helpers.rs`.

fn is_work_graph_mutation_tool(name: &str) -> bool {
    matches!(
        name,
        "update_plan"
            | "work_update"
            | "checklist_write"
            | "todo_write"
            | "checklist_add"
            | "todo_add"
            | "checklist_update"
            | "todo_update"
            | "task_create"
            | "task_cancel"
            // Unified durable-task tool (piagent phase B): covers the
            // create/cancel actions the legacy names above carried.
            | "tasks"
            | "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_cancel"
            | "agent"
            | "workflow"
    )
}

/// UI watchdog bound for an in-progress turn with no activity (#6184).
///
/// Decoupled from `stream_chunk_timeout_secs`: tying it to that budget made
/// the UI watchdog unable to fire before the 900s stream idle timeout. A
/// quiet model wait is protected by the engine heartbeat instead — while the
/// engine reports a bounded wait it has not flagged as overdue, the UI defers
/// to it (`reconcile_turn_liveness_with`).
fn turn_stall_watchdog_timeout(_app: &App) -> Duration {
    TURN_STALL_WATCHDOG_TIMEOUT
}

fn active_turn_has_running_tool(app: &App) -> bool {
    app.active_cell.as_ref().is_some_and(|active| {
        active.entries().iter().any(|cell| match cell {
            HistoryCell::Tool(tool) => tool.is_running(),
            _ => false,
        })
    })
}

// Per-turn notification composition (settings, message body, summary)
// moved to `tui/notifications.rs` alongside the dispatch primitives.

/// The TUI's copy of a tool result for its API-message mirror. It is the same
/// view the engine gives the model (#6508): whole within the route's inline
/// budget, otherwise cut around a footer that names the saved full output.
/// A separate receipt here used to replace anything over 12,000 characters
/// with a 240-character preview, and that copy is what `SyncSession` sends
/// back to the engine.
fn tool_result_content_for_api_message(app: &App, name: &str, output: &ToolResult) -> String {
    crate::core::engine::compact_tool_result_for_route(
        app.api_provider,
        &app.model,
        app.active_route_limits,
        name,
        output,
    )
}

// Streaming-thinking lifecycle helpers moved to `tui/streaming_thinking.rs`.

/// Data produced by the async dispatch phase that is needed to apply the
/// post-acceptance mutations to `App`.
#[derive(Debug, Clone)]
pub(crate) struct UserDispatchOutcome {
    turn_compaction: CompactionConfig,
    effective_provider: ProviderKind,
    effective_model: String,
    effective_provider_identity: String,
    effective_provider_label: String,
    effective_reasoning_effort: EffectiveReasoningEffort,
    auto_selection: Option<crate::model_routing::AutoRouteSelection>,
}

/// Tell the operator that an explicit "make this my default" request did not
/// take effect, instead of leaving a normal apply summary that reads like
/// success. Silence here is what made the sticky-default bug so confusing.
fn note_startup_default_not_saved(app: &mut App, save_as_startup_default: bool) {
    if !save_as_startup_default {
        return;
    }
    let existing = app.status_message.take();
    let note = "Startup default unchanged — the route was not applied.";
    app.status_message = Some(match existing {
        Some(message) if !message.trim().is_empty() => format!("{message} · {note}"),
        _ => note.to_string(),
    });
}

/// Route every Fleet-setup entry point to the storage surface that actually
/// controls the effective roster. A selected v2 Fleet always opens its exact
/// named editor; the legacy profile wizard is reachable only with no selected
/// Fleet. Selection resolution deliberately does not consult project trust.
fn open_fleet_setup_target(app: &mut App, config: &Config, member_id: Option<&str>) {
    use crate::tui::views::fleet_setup::{FleetSetupEditTarget, resolve_fleet_setup_edit_target};

    match resolve_fleet_setup_edit_target(&app.workspace) {
        Ok(FleetSetupEditTarget::SelectedFleet { name, scope }) => {
            if app.view_stack.top_kind() == Some(ModalKind::FleetDetail) {
                return;
            }
            let Some(mut view) = crate::tui::views::fleet_detail::FleetDetailView::open_for_member(
                app, config, &name, scope, member_id,
            ) else {
                app.set_sticky_status(
                    "Selected team is invalid or unreadable; open /fleet teams to repair or clear the selection. Legacy profiles were not opened."
                        .to_string(),
                    StatusToastLevel::Error,
                    None,
                );
                return;
            };
            let fleet_name = codewhale_runtime::safe_label::SafeLabel::phrase(&name);
            let picker = if member_id.is_some() {
                let (editor_id, target) = view.direct_assignment();
                let (role, scope) = view.assignment_context();
                view.route_selection(editor_id, target).map(|selection| {
                    crate::tui::model_picker::ModelPickerView::new_for_fleet_route(
                        app, config, target, editor_id, selection,
                    )
                    .with_assignment_context(role, scope)
                })
            } else {
                None
            };
            app.view_stack.push(view);
            if let Some(picker) = picker {
                app.view_stack.push(picker);
            }
            app.status_message = Some(format!(
                "Editing selected team `{fleet_name}` ({}) — legacy profiles will not be changed.",
                scope.label()
            ));
        }
        Ok(FleetSetupEditTarget::LegacyProfiles) => {
            if app.view_stack.top_kind() == Some(ModalKind::FleetSetup) {
                return;
            }
            if let Some(member_id) = member_id {
                match crate::tui::views::fleet_setup::FleetSetupView::new_for_route_assignment(
                    app, config, member_id,
                ) {
                    Ok(view) => {
                        if let ViewAction::Emit(ViewEvent::FleetProfileRoutePickRequested {
                            editor_id,
                        }) = view.route_pick_request()
                            && let Some(selection) = view.route_selection(editor_id)
                        {
                            let (role, scope) = view.assignment_context();
                            let picker =
                                crate::tui::model_picker::ModelPickerView::new_for_fleet_profile(
                                    app, config, editor_id, selection,
                                )
                                .with_assignment_context(role, scope);
                            app.view_stack.push(view);
                            app.view_stack.push(picker);
                        }
                    }
                    Err(reason) => app.set_sticky_status(reason, StatusToastLevel::Error, None),
                }
                return;
            }
            let _ = app.next_draft_gen();
            let view = match member_id {
                Some(member_id) => crate::tui::views::fleet_setup::FleetSetupView::new_for_role(
                    app, config, member_id,
                ),
                None => crate::tui::views::fleet_setup::FleetSetupView::new(app, config),
            };
            app.view_stack.push(view);
        }
        Err(message) => {
            app.set_sticky_status(message, StatusToastLevel::Error, None);
        }
    }
}

pub(crate) struct ProviderFallbackRollback {
    identity: ProviderIdentity,
    chain: Option<codewhale_config::ProviderChain>,
}

// File-picker relevance scoring moved to `tui/file_picker_relevance.rs`.

#[cfg(test)]
use std::process::{Command, Stdio};

// `ui.rs` had grown past 19k lines. These three modules hold the same code,
// moved verbatim, and are re-exported so every existing path still resolves.
mod apply;
mod approval_routing;
pub(crate) mod feedback_host;
use approval_routing::*;
mod event_loop;
mod handlers;

pub(crate) use apply::*;
pub(crate) use event_loop::*;
pub(crate) use handlers::*;
// The crate-wide glob would otherwise narrow this to `pub(crate)`; `tui/mod.rs`
// re-exports it as the binary's entry point.
pub use event_loop::run_tui;

mod compaction_flow;
pub(crate) use compaction_flow::*;
pub(crate) use provider_setup::*;
mod dispatch;
mod dispatch_prepare;
pub(crate) use dispatch_prepare::*;
pub(crate) mod fatal_signal_guard;
// #6169: runtime half of the foreground-ownership contract — restore on stop,
// rebuild on continue. Sits next to the fatal guard because both write the same
// teardown table.
pub(crate) mod job_control_guard;
mod motion;
mod observer_hooks;
mod provider_setup;
mod release_check;
mod remote_control_bridge;
mod task_projection;
mod terminal;
mod terminal_input;
use remote_control_bridge::*;
use terminal_input::*;
// #6165: `external_editor` is a sibling of `ui`, and the pump pause now lives
// inside its `with_suspended_tui` so no editor entry point can forget it.
pub(crate) use terminal_input::pause_terminal_input_for_child;

pub(crate) use dispatch::*;
pub(crate) use motion::*;
pub(crate) use release_check::*;
pub(crate) use terminal::*;

// `frame` is `pub(crate)` so sibling modules (e.g. the widgets ASCII-safety
// test) can reach the topbar builders that project `App` state.
pub(crate) mod frame;
mod overlays;
mod provider_routes;
mod session_state;

pub(crate) use frame::*;
pub(crate) use overlays::*;
pub(crate) use provider_routes::*;
pub(crate) use session_state::*;

#[cfg(test)]
fn spawn_external_url_command(mut command: Command) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("failed to launch browser command: {err}"))
}

async fn execute_command_input(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &mut Config,
    input: &str,
) -> Result<bool> {
    let _ = app.note_manual_command_for_tip(input);
    if let Some(parsed_index) = parse_queue_send_command(input) {
        match parsed_index {
            Ok(index) => {
                send_queued_message_at_index_now(app, config, engine_handle, index).await?;
            }
            Err(message) => {
                app.status_message = Some(message);
            }
        }
        return Ok(false);
    }

    let result = commands::execute_with_config(input, app, config);
    // The NOTES view reads the notes file off the render path on the
    // workspace-context tick; a `/note` change refreshes it at once (#6565).
    if input
        .split_whitespace()
        .next()
        .is_some_and(|command| command.eq_ignore_ascii_case("/note"))
    {
        workspace_context::refresh_now(app, Instant::now());
    }
    // After /logout: clear the in-memory api_key fields so the next
    // onboarding round entering a new key doesn't see the stale value
    // (#343). The on-disk side is handled by clear_api_key() inside
    // commands::config::logout.
    if input.trim().eq_ignore_ascii_case("/logout") {
        // Only clear the active provider's in-memory API key, not every
        // provider.  The on-disk clear_api_key() inside commands::config::logout
        // already removes all saved keys; clearing only the active slot here
        // prevents surprising side-effects when the user has multiple providers
        // configured.
        clear_active_provider_api_key_from_memory(app, config).map_err(anyhow::Error::msg)?;
        app.api_key_env_only =
            codewhale_runtime::config::active_provider_uses_env_only_api_key(config);
    }
    apply_command_result(terminal, app, engine_handle, task_manager, config, result).await
}

#[derive(Debug, Clone)]
pub(crate) struct SteerPausedSnapshot {
    paused: bool,
    pausable: bool,
    paused_goal_objective: Option<String>,
    objective: Option<String>,
    tokens_used: u64,
    time_used_seconds: u64,
    continuation_count: u32,
}

fn use_bundled_constitution(app: &mut App, config: &Config) {
    let mut state = crate::tui::setup::load_setup_state_for_app(app, config);
    state.complete_constitution_checkpoint(
        crate::prompts::CONSTITUTION_CHECKPOINT_VERSION,
        codewhale_config::ConstitutionChoice::Bundled,
    );
    state.constitution_source = codewhale_config::ConstitutionSource::Bundled;
    state.constitution_validity = codewhale_config::ConstitutionValidity::Unknown;
    state.constitution_preview_hash = None;
    state.set_step(
        codewhale_config::SetupStep::Constitution,
        codewhale_config::StepEntry::new(
            codewhale_config::StepStatus::Verified,
            true,
            crate::prompts::CONSTITUTION_CHECKPOINT_VERSION,
        )
        .with_result("bundled/default constitution"),
    );

    match state.save() {
        Ok(()) => {
            app.status_message = Some(
                "Using the bundled/default constitution; custom user-global law is inactive."
                    .to_string(),
            );
        }
        Err(err) => {
            app.status_message = Some(format!("Failed to save constitution choice: {err}"));
            app.add_message(HistoryCell::System {
                content: format!("Failed to save constitution choice: {err}"),
            });
        }
    }
    app.needs_redraw = true;
}

fn prepare_config_update_result(
    mut result: commands::CommandResult,
    persist: bool,
) -> commands::CommandResult {
    // Live previews can fire on every navigation tick. Suppress routine
    // confirmations, but preserve errors and AppAction so one canonical path
    // remains responsible for both user-visible output and side effects.
    if !persist && !result.is_error {
        result.message = None;
    }
    result
}

pub(crate) struct ApprovalDecisionEvent {
    tool_id: String,
    tool_name: String,
    decision: ReviewDecision,
    timed_out: bool,
    approval_key: String,
    approval_grouping_key: String,
    persistent_rules: Vec<codewhale_config::ToolAskRule>,
}

fn mark_active_turn_cancelled_locally(app: &mut App) {
    settle_pending_human_requests(app);
    app.retire_action_notices(None);
    // #2739: every local cancel surface (Esc, Ctrl+C, approval abort, paused
    // command abort) must snapshot before it clears turn state. Otherwise
    // --continue reloads the previous save and the interrupted turn vanishes.
    app.streaming_state.reset();
    app.finalize_active_cell_as_interrupted();
    app.finalize_streaming_assistant_as_interrupted();
    persist_recovery_snapshot(app);
    app.is_loading = false;
    // #6800: a dispatch still waiting on engine admission fails back now, not
    // after its 60 s bound; its closure retires `dispatch_in_flight`.
    app.cancel_in_flight_dispatch();
    app.dispatch_started_at = None;
    app.turn_started_at = None;
    app.turn_last_activity_at = None;
    app.runtime_turn_id = None;
    app.runtime_turn_status = None;
    app.suppress_stream_events_until_turn_complete = true;
    codewhale_runtime::retry_status::clear();
    crate::tui::notifications::clear_taskbar_progress();
    crate::tui::notifications::stop_title_animation_quietly();
}

/// The Esc-shaped "cancel the active turn" body, extracted verbatim from the
/// event loop's `EscapeAction::CancelRequest` arm so the session control
/// socket's `interrupt` verb and the Esc key cannot drift apart. Returns
/// `true` when the caller should stop handling the event (compaction cancel
/// or goal-continuation stop consumed it), `false` otherwise. The caller
/// keeps its own Esc-specific state (backtrack reset) outside this body.
pub(crate) fn escape_cancel_request(
    app: &mut App,
    engine_handle: &EngineHandle,
    current_streaming_text: &mut String,
    stream_display_clock: &mut StreamDisplayClock,
) -> bool {
    let compacting = app.is_compacting || app.manual_compaction_queued;
    if compacting {
        try_cancel_compaction(app, engine_handle);
        if !compact_interrupt_should_stop_turn(app) {
            return true;
        }
        // Mid-turn compact is collateral. Esc/interrupt stops the turn
        // (Codex/GrokBuild): cancel_compaction alone continues the loop.
    }
    if app.paused || app.paused_goal_objective.is_some() {
        clear_paused_command_state(app, engine_handle);
        if app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress")) {
            engine_handle.cancel();
            mark_active_turn_cancelled_locally(app);
            current_streaming_text.clear();
            stream_display_clock.reset();
        }
        app.active_allowed_tools = None;
        app.goal.objective = None;
        app.goal.tokens_used = 0;
        app.goal.time_used_seconds = 0;
        app.goal.continuation_count = 0;
        app.status_message = Some(parent_stop_status(app, "Paused command cancelled"));
        false
    } else {
        let was_waiting = app.goal_continuation_waiting;
        engine_handle.cancel();
        if was_waiting {
            app.goal_continuation_waiting = false;
            app.status_message = Some(app.tr(MessageId::GoalContinuationStopped).to_string());
            return true;
        }
        mark_active_turn_cancelled_locally(app);
        current_streaming_text.clear();
        stream_display_clock.reset();
        app.status_message = Some(parent_stop_status(app, "Request cancelled"));
        false
    }
}

/// Stream events a local cancel hides until the turn completes. Approval,
/// sandbox-elevation, and question requests are never silently hidden:
/// `resolve_stale_parent_request` answers a stale parent request explicitly,
/// and a child agent's request is always delivered (approvals C1).
fn suppress_engine_event_after_local_cancel(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::MessageStarted { .. }
            | EngineEvent::MessageDelta { .. }
            | EngineEvent::MessageComplete { .. }
            | EngineEvent::ThinkingStarted { .. }
            | EngineEvent::ThinkingDelta { .. }
            | EngineEvent::ThinkingComplete { .. }
            | EngineEvent::ToolCallStarted { .. }
            | EngineEvent::ToolCallHeartbeat
            | EngineEvent::ToolCallComplete { .. }
            | EngineEvent::SessionUpdated { .. }
    )
}

fn ignore_stale_stream_event_while_idle(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::MessageStarted { .. }
            | EngineEvent::MessageDelta { .. }
            | EngineEvent::MessageComplete { .. }
            | EngineEvent::ThinkingStarted { .. }
            | EngineEvent::ThinkingDelta { .. }
            | EngineEvent::ThinkingComplete { .. }
            | EngineEvent::ToolCallStarted { .. }
            | EngineEvent::ToolCallHeartbeat
            | EngineEvent::ToolCallComplete { .. }
    )
}

/// `Ok` carries the `/models` roster the probe already downloaded, when it
/// was one complete listing (see [`crate::client::verify_provider_api_key`]).
type ProviderKeyVerification<'a> = Pin<
    Box<
        dyn Future<Output = Result<Option<codewhale_config::catalog::ProviderCatalogDelta>, String>>
            + Send
            + 'a,
    >,
>;

pub(crate) fn request_shell_wait_detach(app: &mut App) {
    let (notice, level) = match request_active_shell_wait_detach(app) {
        Ok(true) => (
            app.tr(MessageId::ShellWaitReleased)
                .replace("{jobs}", "/jobs"),
            StatusToastLevel::Info,
        ),
        Ok(false) => {
            let message = if terminal_pause_has_live_owner(app) {
                MessageId::ShellWaitInteractive
            } else if app.is_loading {
                MessageId::ShellWaitUnavailable
            } else {
                MessageId::ShellWaitNoActive
            };
            (app.tr(message).to_string(), StatusToastLevel::Info)
        }
        Err(err) => (err.to_string(), StatusToastLevel::Error),
    };
    app.push_status_toast(notice, level, Some(6_000));
}

fn request_active_shell_wait_detach(app: &App) -> Result<bool> {
    let Some(session_id) = app.current_session_id.as_deref() else {
        return Ok(false);
    };
    let Some(shell_manager) = app.runtime_services.shell_manager.as_ref() else {
        return Ok(false);
    };
    let mut manager = shell_manager.lock().map_err(|_| {
        anyhow::anyhow!("Shell tracking hit an internal error — restart Codewhale to recover.")
    })?;
    Ok(manager.request_shell_wait_detach(session_id))
}

pub(crate) fn prefill_jobs_cancel_all_if_tasks_sidebar(app: &mut App) -> bool {
    if !app.view_stack.is_empty()
        || app.work_surface.panel != crate::tui::work_surface::RailPanel::Tasks
        || app.work_surface.last_area.is_none()
        || !app
            .task_panel
            .iter()
            .any(crate::tui::background_indicator::is_live_shell_entry)
    {
        return false;
    }

    app.input = "/jobs cancel-all".to_string();
    app.cursor_position = app.input.len();
    app.status_message = Some("Press Enter to cancel all running commands".to_string());
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchDirection {
    Forward,
    Backward,
}

pub(crate) fn clamp_event_poll_timeout(timeout: Duration) -> Duration {
    const MIN_EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(1);
    timeout.max(MIN_EVENT_POLL_TIMEOUT)
}

/// Announce the background work that finished since the last notice, when
/// the `[notifications].subagent_completion` mode says it is time (#6565).
///
/// Finite work still live (running agents that are not suspect ghosts, a
/// running workflow, queued or running durable tasks that are not stale)
/// holds a `final-only`
/// batch; a running background shell never does. `parent_idle` forces the
/// parent-turn half of the rule open, for the moment a turn completes.
/// `settings()` still has the final say (method=off / condition=never).
pub(crate) fn flush_background_finished(app: &mut App, config: &Config, parent_idle: bool) {
    use crate::tui::background_finished::{background_finished_payload, ready_to_flush};
    if app.background_finished.is_empty() {
        return;
    }
    let mode = config.notifications_config().subagent_completion;
    let finite_work_live = session_state::live_running_agent_count(app, Instant::now()) > 0
        || frame::workflow_tool_is_running(app)
        || app.task_panel.iter().any(|entry| {
            // A stale entry (a recovered task whose ownership is unverified)
            // is not known to be running and could hold the batch forever,
            // the same reason suspect ghost agents are left out.
            !entry.stale
                && entry.kind != TaskPanelEntryKind::Shell
                && matches!(entry.status.as_str(), "queued" | "running")
        });
    let parent_busy = app.is_loading && !parent_idle;
    if !ready_to_flush(
        mode,
        &app.background_finished,
        finite_work_live,
        parent_busy,
    ) {
        return;
    }
    let batch = std::mem::take(&mut app.background_finished);
    if mode == codewhale_config::notifications::SubagentCompletionNotification::Off {
        return;
    }
    let Some((method, threshold, include_summary)) = notifications::settings(config) else {
        return;
    };
    let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
    let notices: Vec<&[crate::tui::background_finished::FinishedWork]> =
        if mode == codewhale_config::notifications::SubagentCompletionNotification::Always {
            batch.chunks(1).collect()
        } else {
            vec![batch.as_slice()]
        };
    for items in notices {
        let elapsed = items
            .iter()
            .map(|item| item.elapsed)
            .max()
            .unwrap_or_default();
        if let Some(payload) = background_finished_payload(app.ui_locale, items, include_summary) {
            notifications::notify_done(method, in_tmux, &payload, threshold, elapsed);
        }
    }
}

/// Settle the background-finished batch when the parent turn ends (#6565).
///
/// A completed turn sends its own notice, which covers the shells and tasks
/// that finished while it ran, so those are dropped rather than announced a
/// second time. Whatever else was held for the turn is then flushed.
pub(crate) fn settle_background_finished_at_turn_end(
    app: &mut App,
    config: &Config,
    turn_completed: bool,
) {
    if turn_completed {
        crate::tui::background_finished::drop_reported_by_turn(&mut app.background_finished);
    }
    flush_background_finished(app, config, true);
}

// Keyboard-shortcut predicates moved to `tui/key_shortcuts.rs`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupVersionCheckSource {
    Disabled,
    ConfiguredUrl(String),
    ReleaseResolver,
}

/// A newer-stable-release notice, carrying enough context to render both the
/// short transient toast and the durable in-transcript update prompt (#3961).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateNotice {
    current: String,
    latest: String,
}

impl UpdateNotice {
    /// Short line for the transient status toast, naming the command that
    /// actually updates *this* install.
    fn toast_line(&self, install: InstallMethod) -> String {
        format!(
            "v{latest} available - run `{command}` and restart",
            latest = self.latest,
            command = install.update_command()
        )
    }

    /// Compact header chip label shown once the check has landed. Quiet by
    /// design: no action verb, no repetition — the toast and transcript
    /// notice carry the update instructions (#14).
    fn chip_label(&self) -> String {
        format!("↑ v{latest}", latest = self.latest)
    }

    /// Durable, actionable notice pushed into the transcript so it survives the
    /// toast TTL. Includes current/latest versions, release notes, the exact
    /// update command, and restart guidance.
    ///
    /// Package-managed installs get their manager's command instead of
    /// `codewhale update`, plus an explicit warning: self-updating a binary
    /// Homebrew or npm owns leaves the manager's metadata lying about what is
    /// on disk, and the next upgrade silently reverts the user.
    fn notice_block(&self, install: InstallMethod) -> String {
        let action = if install.supports_self_update() {
            "Run `/update install` here (preview it with a bare `/update`), or `codewhale update` in a shell, then restart Codewhale."
                .to_string()
        } else {
            format!(
                "Installed via {label}. Run `{command}`, then restart Codewhale.\n\
                 Do not use `codewhale update` here — it would replace a binary {label} manages.",
                label = install.label(),
                command = install.update_command()
            )
        };
        format!(
            "Update available: v{current} -> v{latest}\n\
             Release notes: https://github.com/codewhale-hq/CodeWhale/releases/tag/v{latest}\n\
             {action}",
            current = self.current,
            latest = self.latest
        )
    }
}

mod activity_detail;

#[cfg(test)]
mod provider_key_validation_tests {
    use super::*;
    use crate::core::engine::mock_engine_handle;
    use ratatui::{buffer::Buffer, layout::Rect};
    use tempfile::TempDir;

    struct ConfigPathEnvGuard {
        _tmp: TempDir,
        // Onboarding completion runs the setup transaction (setup_state.json,
        // settings.toml) against `CODEWHALE_HOME`; without this guard the
        // fixture provider landed in the developer's real ~/.codewhale (#5932).
        _codewhale_home: crate::test_support::EnvVarGuard,
        _codewhale_config_path: crate::test_support::EnvVarGuard,
        _deepseek_config_path: crate::test_support::EnvVarGuard,
        _lock: crate::test_support::TestEnvLock,
    }

    impl ConfigPathEnvGuard {
        fn new() -> Self {
            let lock = crate::test_support::lock_test_env();
            let tmp = TempDir::new().expect("config tempdir");
            let home = tmp.path().join(".codewhale");
            let config_path = home.join("config.toml");
            std::fs::create_dir_all(config_path.parent().expect("config parent"))
                .expect("config dir");
            Self {
                _tmp: tmp,
                _codewhale_home: crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home),
                _codewhale_config_path: crate::test_support::EnvVarGuard::set(
                    "CODEWHALE_CONFIG_PATH",
                    &config_path,
                ),
                _deepseek_config_path: crate::test_support::EnvVarGuard::set(
                    "DEEPSEEK_CONFIG_PATH",
                    &config_path,
                ),
                _lock: lock,
            }
        }

        fn config_path(&self) -> PathBuf {
            std::env::var_os("CODEWHALE_CONFIG_PATH")
                .map(PathBuf::from)
                .expect("config path set")
        }
    }

    fn create_test_app() -> App {
        let options = TuiOptions {
            start_in_agent_mode: true,
            skip_onboarding: false,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        // These suites assert legacy strip geometry (work surface above the
        // transcript). The Bottom default (round 3, 2026-09-01) has its own
        // coverage in work_surface::rail_panels_render_in_all_placements.
        app.work_surface.placement = crate::tui::work_surface::WorkSurfacePlacement::Top;
        app.api_provider = ProviderKind::Deepseek;
        app.model = "deepseek-v4-pro".to_string();
        app.auto_model = false;
        app
    }

    #[test]
    fn api_key_live_mirror_revokes_stale_external_credential_consent() {
        let external_path = if cfg!(windows) {
            PathBuf::from(r"C:\Users\test\grok-auth.json")
        } else {
            PathBuf::from("/tmp/grok-auth.json")
        };
        let mut config = {
            let __cw_config_init_0_0 = Some(ProvidersConfig {
                xai: ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(
                        codewhale_config::ExternalCredentialConsentToml::read_only(
                            codewhale_config::ProviderKind::Xai,
                            codewhale_config::ExternalCredentialSource::GrokCli,
                            external_path,
                        ),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.providers = __cw_config_init_0_0;
            __cw_config_init_0_value
        };

        {
            let captured_fixture_identity = (config).test_identity_for_kind(ProviderKind::Xai);
            mirror_saved_api_key_in_config(
                &mut config,
                &captured_fixture_identity,
                "codewhale-owned-api-key".to_string(),
            )
            .expect("admitted API-key fixture")
        };

        let xai = config
            .provider_config_for(&config.test_identity_for_kind(ProviderKind::Xai))
            .expect("xAI live config");
        assert_eq!(xai.auth_mode.as_deref(), Some("api_key"));
        assert_eq!(xai.api_key.as_deref(), Some("codewhale-owned-api-key"));
        assert!(xai.external_credentials.is_none());
    }

    struct MockProviderKeyVerifier {
        result: Result<(), String>,
        roster: Option<codewhale_config::catalog::ProviderCatalogDelta>,
        calls: std::sync::Mutex<Vec<(ProviderKind, String, String)>>,
    }

    impl MockProviderKeyVerifier {
        fn new(result: Result<(), String>) -> Self {
            Self {
                result,
                roster: None,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn with_roster(mut self, roster: codewhale_config::catalog::ProviderCatalogDelta) -> Self {
            self.roster = Some(roster);
            self
        }

        fn calls(&self) -> Vec<(ProviderKind, String, String)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl ProviderKeyVerifier for MockProviderKeyVerifier {
        fn verify<'a>(
            &'a self,
            provider: ProviderKind,
            api_key: &'a str,
            base_url: &'a str,
        ) -> ProviderKeyVerification<'a> {
            self.calls.lock().expect("calls lock").push((
                provider,
                api_key.to_string(),
                base_url.to_string(),
            ));
            Box::pin(std::future::ready(
                self.result.clone().map(|()| self.roster.clone()),
            ))
        }
    }

    fn openrouter_config(base_url: &str) -> Config {
        {
            let __cw_config_init_1_0 = Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    base_url: Some(base_url.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.providers = __cw_config_init_1_0;
            __cw_config_init_1_value
        }
    }

    fn two_named_custom_routes() -> Config {
        {
            let __cw_config_init_2_0 = Some("custom-a".to_string());
            let __cw_config_init_2_1 = Some(ProvidersConfig {
                custom: std::collections::HashMap::from([
                    (
                        "custom-a".to_string(),
                        ProviderConfig {
                            kind: Some("openai-compatible".to_string()),
                            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
                            model: Some("model-a".to_string()),
                            api_key: Some("key-a".to_string()),
                            ..Default::default()
                        },
                    ),
                    (
                        "custom-b".to_string(),
                        ProviderConfig {
                            kind: Some("openai-compatible".to_string()),
                            base_url: Some("http://127.0.0.1:18182/v1".to_string()),
                            model: Some("model-b".to_string()),
                            ..Default::default()
                        },
                    ),
                ]),
                ..Default::default()
            });
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        }
    }

    #[test]
    fn provider_key_check_classifies_transport_failures_truthfully() {
        assert_eq!(
            provider_verification_error_category("connection refused"),
            crate::error_taxonomy::ErrorCategory::Network
        );
        assert_eq!(
            provider_verification_error_category("request timed out"),
            crate::error_taxonomy::ErrorCategory::Timeout
        );
        assert_eq!(
            provider_verification_error_category("HTTP 429 rate limit"),
            crate::error_taxonomy::ErrorCategory::RateLimit
        );
        assert_eq!(
            provider_verification_error_category("HTTP 401 unauthorized"),
            crate::error_taxonomy::ErrorCategory::Authentication
        );
        assert_eq!(
            provider_verification_error_category("HTTP 403 forbidden"),
            crate::error_taxonomy::ErrorCategory::Authorization
        );
        assert_eq!(
            provider_verification_error_category("HTTP 500 upstream failure"),
            crate::error_taxonomy::ErrorCategory::Network
        );
    }

    /// Release QA: a DeepSeek key probe returned two served ids, yet guided
    /// setup offered catalog rows the endpoint rejects at the first turn. The
    /// roster the probe already downloaded must become the route's model list.
    #[tokio::test]
    async fn provider_key_probe_roster_becomes_the_model_pick_roster() {
        use codewhale_config::catalog::{
            CatalogOffering, CatalogSource, ProviderCatalogDelta, base_url_fingerprint, now_unix,
        };
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::provider_catalog_live::reset_cache_for_test();
                crate::provider_lake::clear_live_snapshot();
            }
        }
        let _config_env = ConfigPathEnvGuard::new();
        let _live = crate::provider_lake::lock_live_snapshot();
        let _reset = Reset;
        crate::provider_catalog_live::reset_cache_for_test();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = Config::default();
        let identity = picker_provider_identity(&config, ProviderKind::Deepseek, None)
            .expect("DeepSeek identity");
        let mut scoped = config.clone();
        scoped
            .scope_to_provider_identity(&identity)
            .expect("scope DeepSeek");
        let base_url = scoped.active_route_base_url();
        let fingerprint = base_url_fingerprint(&base_url);
        let fetched_at = now_unix();
        let served = ["deepseek-flash", "deepseek-v4-pro"];
        // Scoped to the provider kind, exactly as the live probe returns it,
        // plus an id the catalog does not offer as a chat model.
        let roster = ProviderCatalogDelta {
            provider: "deepseek".into(),
            base_url_fingerprint: fingerprint.clone(),
            fetched_at,
            offerings: served
                .iter()
                .chain(&["deepseek-embedding-fixture"])
                .map(|id| CatalogOffering {
                    provider: "deepseek".into(),
                    wire_model_id: (*id).into(),
                    endpoint_key: "chat".into(),
                    source: CatalogSource::Live {
                        base_url_fingerprint: fingerprint.clone(),
                        fetched_at,
                    },
                    ..Default::default()
                })
                .collect(),
        };
        assert_ne!(
            crate::provider_lake::catalog_models_for_route(
                ProviderKind::Deepseek,
                identity.key.as_str(),
                &base_url,
            ),
            served,
            "precondition: the catalog fallback lists more than the endpoint serves"
        );
        let verifier = MockProviderKeyVerifier::new(Ok(())).with_roster(roster);

        apply_provider_picker_api_key_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity.clone(),
            "sk-verified".to_string(),
            None,
            &verifier,
        )
        .await;

        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ProviderPicker));
        assert_eq!(
            crate::provider_lake::catalog_models_for_route(
                ProviderKind::Deepseek,
                identity.key.as_str(),
                &base_url,
            ),
            served
        );
    }

    #[tokio::test]
    async fn provider_key_submit_opens_model_pick_without_persisting_on_validation_success() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = openrouter_config("https://mock.openrouter.test/v1");
        let verifier = MockProviderKeyVerifier::new(Ok(()));
        let identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("OpenRouter identity");

        apply_provider_picker_api_key_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "sk-verified".to_string(),
            None,
            &verifier,
        )
        .await;

        assert_eq!(
            verifier.calls(),
            vec![(
                ProviderKind::Openrouter,
                "sk-verified".to_string(),
                "https://mock.openrouter.test/v1".to_string()
            )]
        );
        // Validation success must not persist or switch yet (#3875 residual):
        // the guided flow continues at model pick first.
        assert_eq!(app.api_provider, ProviderKind::Deepseek);
        assert_eq!(config.provider.as_deref(), None);
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.openrouter.api_key.as_deref()),
            None
        );
        let saved = std::fs::read_to_string(config_env.config_path()).unwrap_or_default();
        assert!(!saved.contains("sk-verified"));
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ProviderPicker));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|status| { status.contains("The provider accepted the key") }),
            "status names connection-probe success: {:?}",
            app.status_message
        );
        // The probe proves only this temporary credential generation, not the
        // original uncredentialed Config or any model's entitlement.
        let mut probed_config = config.clone();
        let probed_identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("captured OpenRouter identity");
        probed_config
            .scope_to_provider_identity(&probed_identity)
            .expect("scope probe");
        probed_config
            .set_provider_api_key_override(&probed_identity, Some("sk-verified".to_string()))
            .expect("temporary probe key");
        let verified_route = crate::provider_readiness::route_identity_for_model(
            &probed_config,
            &probed_identity,
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_MODEL,
        );
        assert_eq!(
            crate::provider_readiness::resolve_with_identity(
                &verified_route,
                crate::provider_readiness::CredentialState::Saved,
                true,
                &app.provider_health,
            ),
            crate::provider_readiness::ResolvedProviderReadiness::ConnectionCheckedModelUnchecked,
            "the live connection probe must not be reported as model ready",
        );

        let picker = app.view_stack.pop().expect("provider picker reopened");
        let area = Rect::new(0, 0, 90, 16);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("The provider accepted the key")
                && rendered.contains("pick the model to use by default"),
            "expected model-pick stage UI, got:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn test_connection_records_models_probe_not_ready() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = openrouter_config("https://mock.openrouter.test/v1");
        config.provider = Some("openrouter".to_string());
        if let Some(providers) = config.providers.as_mut() {
            providers.openrouter.api_key = Some("sk-saved".to_string());
        }
        let verifier = MockProviderKeyVerifier::new(Ok(()));
        let identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("OpenRouter identity");

        apply_provider_picker_test_connection_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            false,
            &verifier,
        )
        .await;

        assert_eq!(
            verifier.calls(),
            vec![(
                ProviderKind::Openrouter,
                "sk-saved".to_string(),
                "https://mock.openrouter.test/v1".to_string()
            )]
        );
        let _ = config_env;
        assert_eq!(config.provider.as_deref(), Some("openrouter"));
        assert!(
            app.status_toasts.iter().any(|toast| {
                toast.text.contains("The provider accepted the key")
                    && !toast.text.contains("pick the model")
            }),
            "test connection names reachability only: {:?}",
            app.status_toasts
        );
        let verified_route = crate::provider_readiness::route_identity_for_model(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_MODEL,
        );
        assert_eq!(
            crate::provider_readiness::resolve_with_identity(
                &verified_route,
                crate::provider_readiness::CredentialState::Saved,
                true,
                &app.provider_health,
            ),
            crate::provider_readiness::ResolvedProviderReadiness::ConnectionCheckedModelUnchecked,
        );
        assert_ne!(
            crate::provider_readiness::resolve_with_identity(
                &verified_route,
                crate::provider_readiness::CredentialState::Saved,
                true,
                &app.provider_health,
            ),
            crate::provider_readiness::ResolvedProviderReadiness::Ready,
        );
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ProviderPicker));
    }

    #[tokio::test]
    async fn test_connection_without_key_does_not_mark_ready() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = openrouter_config("https://mock.openrouter.test/v1");
        let verifier = MockProviderKeyVerifier::new(Ok(()));
        let identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("OpenRouter identity");

        apply_provider_picker_test_connection_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            false,
            &verifier,
        )
        .await;

        assert!(verifier.calls().is_empty());
        let _ = config_env;
        assert!(
            app.status_toasts
                .iter()
                .any(|toast| toast.text.contains("No API key saved")),
            "{:?}",
            app.status_toasts
        );
        let verified_route = crate::provider_readiness::route_identity_for_model(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_MODEL,
        );
        assert_eq!(
            crate::provider_readiness::resolve_with_identity(
                &verified_route,
                crate::provider_readiness::CredentialState::MissingKey,
                true,
                &app.provider_health,
            ),
            crate::provider_readiness::ResolvedProviderReadiness::MissingKey,
        );
    }

    #[tokio::test]
    async fn test_connection_failure_redacts_the_api_key() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = openrouter_config("https://mock.openrouter.test/v1");
        config.provider = Some("openrouter".to_string());
        if let Some(providers) = config.providers.as_mut() {
            providers.openrouter.api_key = Some("sk-saved".to_string());
        }
        let verifier = MockProviderKeyVerifier::new(Err(
            "HTTP 401: upstream echoed sk-saved in a long diagnostic body that must not stay visible"
                .repeat(4),
        ));
        let identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("OpenRouter identity");

        apply_provider_picker_test_connection_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            true,
            &verifier,
        )
        .await;

        let _ = config_env;
        let status = app
            .status_toasts
            .iter()
            .map(|toast| toast.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !status.contains("sk-saved"),
            "probe toast leaked the API key: {status}"
        );
        assert!(status.contains("***"), "{status}");
        assert!(
            app.provider_picker_memory
                .as_ref()
                .is_some_and(|memory| memory.catalog_view),
            "catalog browsing context must survive the probe"
        );
        let verified_route = crate::provider_readiness::route_identity_for_model(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Openrouter),
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_MODEL,
        );
        assert!(matches!(
            crate::provider_readiness::resolve_with_identity(
                &verified_route,
                crate::provider_readiness::CredentialState::Saved,
                true,
                &app.provider_health,
            ),
            crate::provider_readiness::ResolvedProviderReadiness::SavedLastCheckFailed { .. }
        ));
    }

    /// #4526: the wizard's StepFun billing-route choice must be the endpoint
    /// the key is probed against, and it must reach disk only once the user
    /// confirms — never as a side effect of validation.
    #[tokio::test]
    async fn stepfun_plan_route_is_validated_before_the_key_is_persisted() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = Config::default();
        let verifier = MockProviderKeyVerifier::new(Ok(()));
        let identity = picker_provider_identity(&config, ProviderKind::Stepfun, None)
            .expect("StepFun identity");

        apply_provider_picker_api_key_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "step-plan-key".to_string(),
            Some(
                codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_PLAN_BASE_URL.to_string(),
            ),
            &verifier,
        )
        .await;

        assert_eq!(
            verifier.calls(),
            vec![(
                ProviderKind::Stepfun,
                "step-plan-key".to_string(),
                codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_PLAN_BASE_URL.to_string()
            )],
            "the chosen Step Plan endpoint must be the one live-validated"
        );
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.stepfun.base_url.clone()),
            None,
            "validation must not mutate the live config"
        );
        let saved = std::fs::read_to_string(config_env.config_path()).unwrap_or_default();
        assert!(
            !saved.contains("step_plan"),
            "nothing persisted yet: {saved}"
        );
        assert!(!saved.contains("step-plan-key"), "no secret yet: {saved}");
    }

    /// The confirm stage writes the endpoint into `[providers.stepfun]` and
    /// leaves every other provider table alone.
    #[tokio::test]
    async fn stepfun_setup_confirm_writes_only_the_stepfun_base_url() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = Config::default();
        let identity = picker_provider_identity(&config, ProviderKind::Stepfun, None)
            .expect("StepFun identity");

        apply_provider_picker_setup_confirmed(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "step-plan-key".to_string(),
            codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_MODEL.to_string(),
            None,
            Some(
                codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_PLAN_BASE_URL.to_string(),
            ),
        )
        .await;

        let saved = std::fs::read_to_string(config_env.config_path()).expect("config written");
        let document: toml::Table = toml::from_str(&saved).expect("valid TOML");
        let providers = document
            .get("providers")
            .and_then(toml::Value::as_table)
            .expect("providers table");
        assert_eq!(
            providers
                .get("stepfun")
                .and_then(|entry| entry.get("base_url"))
                .and_then(toml::Value::as_str),
            Some(codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_PLAN_BASE_URL)
        );
        assert_eq!(
            providers.keys().collect::<Vec<_>>(),
            vec!["stepfun"],
            "the route choice must not touch other provider tables"
        );
        assert!(
            document.get("base_url").is_none(),
            "the root base_url must stay untouched: {saved}"
        );
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.stepfun.base_url.as_deref()),
            Some(codewhale_config::descriptors::defaults::DEFAULT_STEPFUN_PLAN_BASE_URL),
            "the live config mirrors the persisted endpoint"
        );
    }

    #[tokio::test]
    async fn replacing_legacy_kimi_import_verifies_and_persists_the_kimi_code_api_key_route() {
        let config_env = ConfigPathEnvGuard::new();
        std::fs::write(
            config_env.config_path(),
            r#"# preserve-kimi-comment
[providers.moonshot]
auth_mode = "kimi_oauth"
"#,
        )
        .expect("seed legacy Kimi import config");
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = {
            let __cw_config_init_3_0 = Some(ProvidersConfig {
                moonshot: ProviderConfig {
                    auth_mode: Some("kimi_oauth".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.providers = __cw_config_init_3_0;
            __cw_config_init_3_value
        };
        let identity = picker_provider_identity(&config, ProviderKind::Moonshot, None)
            .expect("Moonshot identity");
        let verifier = MockProviderKeyVerifier::new(Ok(()));

        apply_provider_picker_api_key_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity.clone(),
            "sk-kimi-supported".to_string(),
            None,
            &verifier,
        )
        .await;

        assert_eq!(
            verifier.calls(),
            vec![(
                ProviderKind::Moonshot,
                "sk-kimi-supported".to_string(),
                codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string(),
            )],
            "replacement keys must be verified against Kimi Code, not the ordinary Moonshot API"
        );

        apply_provider_picker_setup_confirmed(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "sk-kimi-supported".to_string(),
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_MODEL.to_string(),
            None,
            None,
        )
        .await;

        let moonshot = config
            .providers
            .as_ref()
            .map(|providers| &providers.moonshot)
            .expect("in-memory Moonshot config");
        assert_eq!(moonshot.auth_mode.as_deref(), Some("api_key"));
        assert_eq!(
            moonshot.base_url.as_deref(),
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL)
        );
        assert_eq!(moonshot.api_key.as_deref(), Some("sk-kimi-supported"));

        let saved = std::fs::read_to_string(config_env.config_path()).expect("saved config");
        assert!(saved.contains("# preserve-kimi-comment"));
        assert!(saved.contains("auth_mode = \"api_key\""));
        assert!(saved.contains(&format!(
            "base_url = \"{}\"",
            codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL
        )));
    }

    #[tokio::test]
    async fn provider_setup_confirm_persists_provider_model_and_preserves_comments() {
        let config_env = ConfigPathEnvGuard::new();
        // Seed a commented config so the confirm path must preserve it.
        std::fs::write(
            config_env.config_path(),
            r#"# keep-me-comment
[providers.openrouter]
# openrouter-table-comment
base_url = "https://mock.openrouter.test/v1"

[providers.anthropic]
api_key = "fixture-other-provider-key"
"#,
        )
        .expect("seed config");

        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = openrouter_config("https://mock.openrouter.test/v1");
        config
            .providers
            .get_or_insert_with(ProvidersConfig::default)
            .anthropic
            .api_key = Some("fixture-other-provider-key".to_string());
        let model = "deepseek/deepseek-v4-pro".to_string();
        let identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("OpenRouter identity");

        apply_provider_picker_setup_confirmed(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "sk-confirmed".to_string(),
            model.clone(),
            None,
            None,
        )
        .await;

        assert_eq!(app.api_provider, ProviderKind::Openrouter);
        assert_eq!(config.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.openrouter.api_key.as_deref()),
            Some("sk-confirmed")
        );
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.openrouter.model.as_deref()),
            Some(model.as_str())
        );
        let saved = std::fs::read_to_string(config_env.config_path()).expect("saved config");
        assert!(
            saved.contains("# keep-me-comment"),
            "root comment lost:\n{saved}"
        );
        assert!(
            saved.contains("# openrouter-table-comment"),
            "table comment lost:\n{saved}"
        );
        assert!(saved.contains("[providers.openrouter]"));
        assert!(saved.contains("api_key = \"sk-confirmed\""));
        assert!(saved.contains(&format!("model = \"{model}\"")));
        assert!(saved.contains("[providers.anthropic]"));
        assert!(saved.contains("api_key = \"fixture-other-provider-key\""));
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.anthropic.api_key.as_deref()),
            Some("fixture-other-provider-key"),
            "saving OpenRouter must not overwrite a different provider slot"
        );
    }

    #[tokio::test]
    async fn provider_key_submit_reopens_picker_without_persisting_on_validation_failure() {
        let config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        let mut engine = mock_engine_handle();
        let mut config = openrouter_config("https://mock.openrouter.test/v1");
        let verifier = MockProviderKeyVerifier::new(Err("HTTP 401: unauthorized".to_string()));
        let identity = picker_provider_identity(&config, ProviderKind::Openrouter, None)
            .expect("OpenRouter identity");

        apply_provider_picker_api_key_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "sk-rejected".to_string(),
            None,
            &verifier,
        )
        .await;

        assert_eq!(app.api_provider, ProviderKind::Deepseek);
        assert_eq!(config.provider.as_deref(), None);
        assert_eq!(
            config
                .providers
                .as_ref()
                .and_then(|providers| providers.openrouter.api_key.as_deref()),
            None
        );
        let saved = std::fs::read_to_string(config_env.config_path()).unwrap_or_default();
        assert!(!saved.contains("sk-rejected"));
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ProviderPicker));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|status| status.contains("The provider did not accept this key")),
            "status names validation failure: {:?}",
            app.status_message
        );

        let picker = app.view_stack.pop().expect("provider picker reopened");
        let area = Rect::new(0, 0, 90, 14);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf);
        let rendered = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // #6566: a plain sentence with the next step, not the raw reply.
        assert!(
            rendered.contains("The provider did not accept this key"),
            "{rendered}"
        );
        assert!(!rendered.contains("HTTP 401"), "{rendered}");
    }

    #[tokio::test]
    async fn named_custom_verification_failure_and_dismiss_keep_committed_a_route() {
        let _config_env = ConfigPathEnvGuard::new();
        let mut app = create_test_app();
        app.set_provider_identity(ProviderKind::Custom, "custom-a");
        app.set_model_selection("model-a".to_string());
        let mut engine = mock_engine_handle();
        let mut config = two_named_custom_routes();
        let identity = picker_provider_identity(&config, ProviderKind::Custom, Some("custom-b"))
            .expect("custom B identity");
        let verifier = MockProviderKeyVerifier::new(Err("HTTP 401: unauthorized".to_string()));

        apply_provider_picker_api_key_with_verifier(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "rejected-b-key".to_string(),
            None,
            &verifier,
        )
        .await;

        assert_eq!(config.provider.as_deref(), Some("custom-a"));
        assert_eq!(app.provider_identity_for_persistence(), "custom-a");
        app.view_stack.pop().expect("failed verifier picker");
        sync_config_provider_from_app(&mut config, &app);
        let route = validated_app_runtime_route(&app, &config).expect("committed A route");
        assert_eq!(route.identity.key.as_str(), "custom-a");
        assert_eq!(route.client.base_url(), "http://127.0.0.1:18181/v1");
    }

    #[tokio::test]
    async fn named_custom_setup_persists_exact_provider_table_and_model() {
        let config_env = ConfigPathEnvGuard::new();
        std::fs::write(
            config_env.config_path(),
            r#"provider = "custom-a"

[providers.custom-a]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18181/v1"
model = "model-a"

[providers.custom-b]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18182/v1"
model = "model-b"
"#,
        )
        .expect("seed named custom config");
        let mut app = create_test_app();
        app.set_provider_identity(ProviderKind::Custom, "custom-a");
        app.set_model_selection("model-a".to_string());
        let mut engine = mock_engine_handle();
        let mut config = two_named_custom_routes();
        let identity = picker_provider_identity(&config, ProviderKind::Custom, Some("custom-b"))
            .expect("custom B identity");

        apply_provider_picker_setup_confirmed(
            &mut app,
            &mut engine.handle,
            &mut config,
            identity,
            "saved-b-key".to_string(),
            "model-b-confirmed".to_string(),
            None,
            None,
        )
        .await;

        assert_eq!(app.provider_identity_for_persistence(), "custom-b");
        assert_eq!(config.provider.as_deref(), Some("custom-b"));
        let saved = std::fs::read_to_string(config_env.config_path()).expect("saved config");
        assert!(saved.contains("[providers.custom-b]"));
        assert!(saved.contains("api_key = \"saved-b-key\""));
        assert!(saved.contains("model = \"model-b-confirmed\""));
        assert!(!saved.contains("[providers.custom]\n"));
    }

    #[test]
    fn legacy_literal_custom_identity_persistence_moves_into_the_custom_table() {
        let config_env = ConfigPathEnvGuard::new();
        std::fs::write(
            config_env.config_path(),
            r#"provider = "custom"
base_url = "http://127.0.0.1:18180/v1"
default_text_model = "legacy-model"
"#,
        )
        .expect("seed legacy root route");
        let config = {
            let __cw_config_init_4_0 = Some("custom".to_string());
            let __cw_config_init_4_1 = Some("legacy-model".to_string());
            let mut __cw_config_init_4_value: Config = Default::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.default_text_model = __cw_config_init_4_1;
            __cw_config_init_4_value
        }
        .with_legacy_root(None, Some("http://127.0.0.1:18180/v1".to_string()));
        let identity = config
            .resolve_provider_identity("custom")
            .expect("legacy identity");

        codewhale_runtime::config::save_api_key_for_identity(
            &identity,
            &config,
            "legacy-saved-key",
        )
        .expect("save legacy key");
        codewhale_runtime::config::save_provider_model_for_identity(
            &identity,
            &config,
            "legacy-model-updated",
        )
        .expect("save legacy model");

        // The literal custom route's top-level fields now live in
        // `[providers.custom]` (#6394); the saves write there and the write
        // moves the old top-level endpoint alongside.
        let saved = std::fs::read_to_string(config_env.config_path()).expect("saved config");
        let table: toml::Table = toml::from_str(&saved).expect("saved config parses");
        assert!(
            !codewhale_config::legacy_root::has_legacy_root_keys(&table),
            "{saved}"
        );
        let custom = table["providers"]["custom"]
            .as_table()
            .expect("custom table");
        assert_eq!(custom["api_key"].as_str(), Some("legacy-saved-key"));
        assert_eq!(custom["model"].as_str(), Some("legacy-model-updated"));
        assert_eq!(
            custom["base_url"].as_str(),
            Some("http://127.0.0.1:18180/v1")
        );
        let reloaded = crate::plugins::providers::load_config(Some(config_env.config_path()), None)
            .expect("reload legacy");
        assert!(reloaded.selects_literal_custom_provider());
        assert_eq!(
            reloaded
                .resolve_provider_identity("custom")
                .expect("repeat legacy identity"),
            identity
        );
        let route = resolve_runtime_route(
            &reloaded,
            ProviderKind::Custom,
            Some("legacy-model-updated"),
        )
        .expect("resolve reloaded legacy")
        .validate()
        .expect("preflight reloaded legacy");
        assert_eq!(route.client.base_url(), "http://127.0.0.1:18180/v1");
    }

    #[test]
    fn legacy_active_route_does_not_redirect_named_custom_persistence_to_root() {
        let config_env = ConfigPathEnvGuard::new();
        std::fs::write(
            config_env.config_path(),
            r#"provider = "custom"
api_key = "legacy-root-key"
base_url = "http://127.0.0.1:18180/v1"
default_text_model = "legacy-model"

[providers.custom-b]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18182/v1"
model = "model-b"
"#,
        )
        .expect("seed coexistence config");
        let config = crate::plugins::providers::load_config(Some(config_env.config_path()), None)
            .expect("load config");
        assert!(config.selects_literal_custom_provider());
        let identity = config
            .resolve_provider_identity("custom-b")
            .expect("named custom identity");

        codewhale_runtime::config::save_api_key_for_identity(&identity, &config, "saved-b-key")
            .expect("save named custom key");
        codewhale_runtime::config::save_provider_model_for_identity(
            &identity,
            &config,
            "model-b-updated",
        )
        .expect("save named custom model");

        let saved = std::fs::read_to_string(config_env.config_path()).expect("saved config");
        let table: toml::Table = toml::from_str(&saved).expect("saved config parses");
        // The literal route's key moved into its own table, untouched by the
        // named route's save (#6394).
        assert_eq!(
            table["providers"]["custom"]["api_key"].as_str(),
            Some("legacy-root-key")
        );
        assert!(saved.contains("default_text_model = \"legacy-model\""));
        assert!(saved.contains("[providers.custom-b]"));
        assert!(saved.contains("api_key = \"saved-b-key\""));
        assert!(saved.contains("model = \"model-b-updated\""));
    }
}

/// Build the foreground receipt only from the immutable route captured when
/// this turn started. The app's selected route may already have changed by the
/// time `TurnComplete` is handled, so it is not accepted as an input here.
fn completed_turn_cost_route_receipt(
    completed_turn: Option<&crate::tui::app::ActiveTurnMetadata>,
    audit: &crate::pricing::TurnCostAudit,
) -> Option<String> {
    let route = completed_turn?.route.as_ref()?;
    Some(route.cost_envelope()?.receipt(audit))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn fleet_role_entry_opens_shared_picker_and_cancel_restores_parked_roster() {
    use crate::tui::views::ModalView;
    let _env = crate::test_support::lock_test_env();
    let workspace = tempfile::tempdir().unwrap();
    let config = Config::default();
    let mut app = App::new(
        crate::tui::test_support::test_tui_options(workspace.path()),
        &config,
    );
    app.view_stack
        .push(crate::tui::views::fleet_roster::FleetRosterView::new(
            &app, &config,
        ));
    open_fleet_setup_target(&mut app, &config, Some("manager"));
    assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ModelPicker));
    let mut picker = app.view_stack.pop().unwrap();
    let action = picker
        .as_any_mut()
        .downcast_mut::<crate::tui::model_picker::ModelPickerView>()
        .unwrap()
        .handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
    let ViewAction::EmitAndClose(ViewEvent::FleetAssignmentPickerDismissed { editor_id }) = action
    else {
        panic!("assignment cancel must identify its editor")
    };
    assert_eq!(app.view_stack.top_kind(), Some(ModalKind::FleetSetup));
    handlers::dismiss_fleet_assignment(&mut app, editor_id);
    assert_eq!(app.view_stack.top_kind(), Some(ModalKind::FleetRoster));
    assert!(
        !workspace
            .path()
            .join(".codewhale/agents/manager.toml")
            .exists()
    );
}
