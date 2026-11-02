//! A view of the model's workspace-scoped PTYs, with ordered human input.
//!
//! Shell creation, input and reads run on one dedicated blocking worker;
//! the UI only exchanges bounded messages. Output is a 64 KiB ANSI-styled
//! tail, not a terminal emulator: cursor addressing and alternate screens
//! are not replayed, and process restart does not resurrect a shell.

use codewhale_localization::MessageId;
use crossterm::event::KeyEvent;
#[cfg(all(unix, not(target_env = "ohos")))]
use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, prelude::Widget, style::Style, text::Line, widgets::Paragraph};

#[cfg(all(unix, not(target_env = "ohos")))]
use super::model::RailPanel;
use crate::tui::app::App;
use crate::tui::shell_key_routing::ShellBindingId;
#[cfg(all(unix, not(target_env = "ohos")))]
use crate::tui::shell_key_routing::{binding, display_chord};

#[cfg(all(unix, not(target_env = "ohos")))]
use codewhale_engine::tools::terminal_session::{self, TerminalSessionInfo};
#[cfg(all(unix, not(target_env = "ohos")))]
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[cfg(all(unix, not(target_env = "ohos")))]
#[derive(Debug, Default)]
pub(super) struct TerminalDock {
    #[cfg(all(unix, not(target_env = "ohos")))]
    workspace: PathBuf,
    #[cfg(all(unix, not(target_env = "ohos")))]
    worker: Option<Worker>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    sessions: Vec<TerminalSessionInfo>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    commands: Vec<codewhale_engine::tools::shell::ShellJobSnapshot>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    selected: Option<String>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    output: Vec<u8>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    dropped: u64,
    #[cfg(all(unix, not(target_env = "ohos")))]
    last_poll: Option<Instant>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    refreshing: bool,
    #[cfg(all(unix, not(target_env = "ohos")))]
    size: Option<(u16, u16)>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    lost: bool,
    #[cfg(all(unix, not(target_env = "ohos")))]
    creating: bool,
    #[cfg(all(unix, not(target_env = "ohos")))]
    session_tabs: Vec<(String, Rect)>,
    #[cfg(all(unix, not(target_env = "ohos")))]
    scroll: usize,
    #[cfg(all(unix, not(target_env = "ohos")))]
    error: Option<String>,
}

// A copied presentation must not share an input queue or steal replies.
#[cfg(all(unix, not(target_env = "ohos")))]
impl Clone for TerminalDock {
    fn clone(&self) -> Self {
        Self::default()
    }
}

#[cfg(all(unix, not(target_env = "ohos")))]
#[derive(Debug)]
struct Worker {
    requests: mpsc::SyncSender<Request>,
    replies: mpsc::Receiver<Reply>,
}

#[cfg(all(unix, not(target_env = "ohos")))]
#[derive(Debug)]
enum Request {
    Refresh {
        selected: Option<String>,
        session: String,
    },
    Create(codewhale_sandbox::SandboxPolicy),
    Input {
        name: String,
        id: String,
        bytes: Vec<u8>,
        policy: codewhale_sandbox::SandboxPolicy,
    },
    Resize {
        name: String,
        id: String,
        rows: u16,
        cols: u16,
    },
}

#[cfg(all(unix, not(target_env = "ohos")))]
#[derive(Debug)]
enum Reply {
    Snapshot {
        requested: Option<String>,
        sessions: Vec<TerminalSessionInfo>,
        output: Option<terminal_session::OutputChunk>,
        commands: Vec<codewhale_engine::tools::shell::ShellJobSnapshot>,
    },
    Created(String),
    InputWritten(String),
    Error(String),
}

#[cfg(all(unix, not(target_env = "ohos")))]
impl Worker {
    fn start(
        workspace: PathBuf,
        shell_manager: codewhale_engine::tools::shell::SharedShellManager,
    ) -> Result<Self, String> {
        let (requests, incoming) = mpsc::sync_channel(128);
        let (outgoing, replies) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("terminal-dock".into())
            .spawn(move || {
                while let Ok(request) = incoming.recv() {
                    let reply = match request {
                        Request::Refresh { selected, session } => {
                            let pty = selected
                                .as_deref()
                                .filter(|id| command_job_id(id).is_none());
                            match terminal_session::inspect_workspace(&workspace, pty) {
                                Ok((sessions, output)) => Reply::Snapshot {
                                    requested: selected,
                                    sessions,
                                    output,
                                    commands: shell_manager
                                        .lock()
                                        .map(|mut manager| {
                                            dock_commands(manager.list_jobs_for_session(&session))
                                        })
                                        .unwrap_or_default(),
                                },
                                Err(error) => Reply::Error(error),
                            }
                        }
                        Request::Create(policy) => {
                            let name =
                                format!("user-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                            match terminal_session::get_or_create(
                                &name,
                                &workspace,
                                policy,
                                &shell_manager,
                            )
                            .and_then(|_| terminal_session::inspect_workspace(&workspace, None))
                            {
                                Ok((sessions, _)) => {
                                    match sessions.into_iter().find(|session| session.name == name)
                                    {
                                        Some(session) => Reply::Created(session.id),
                                        None => Reply::Error("created terminal disappeared".into()),
                                    }
                                }
                                Err(error) => Reply::Error(error),
                            }
                        }
                        Request::Input {
                            name,
                            id,
                            bytes,
                            policy,
                        } => {
                            match terminal_session::write_dock_input(
                                &workspace, &name, &id, &bytes, &policy,
                            ) {
                                Ok(()) => Reply::InputWritten(id),
                                Err(error) => Reply::Error(error),
                            }
                        }
                        Request::Resize {
                            name,
                            id,
                            rows,
                            cols,
                        } => {
                            let result = terminal_session::lookup(&name, &workspace)
                                .ok_or_else(|| "selected terminal is gone or was reset".to_string())
                                .and_then(|handle| {
                                    let session = handle.lock().map_err(|_| {
                                        "terminal session lock poisoned".to_string()
                                    })?;
                                    terminal_session::resize_dock_session(&session, &id, rows, cols)
                                });
                            match result {
                                Ok(()) => continue,
                                Err(error) => Reply::Error(error),
                            }
                        }
                    };
                    if outgoing.send(reply).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { requests, replies })
    }
}

#[cfg(all(unix, not(target_env = "ohos")))]
const COMMAND_TAB_PREFIX: &str = "cmd:";

#[cfg(all(unix, not(target_env = "ohos")))]
const FINISHED_COMMAND_TABS: usize = 5;

#[cfg(all(unix, not(target_env = "ohos")))]
fn command_job_id(tab: &str) -> Option<&str> {
    tab.strip_prefix(COMMAND_TAB_PREFIX)
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn command_tab_id(job: &codewhale_engine::tools::shell::ShellJobSnapshot) -> String {
    format!("{COMMAND_TAB_PREFIX}{}", job.id)
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn dock_commands(
    jobs: Vec<codewhale_engine::tools::shell::ShellJobSnapshot>,
) -> Vec<codewhale_engine::tools::shell::ShellJobSnapshot> {
    use codewhale_engine::tools::shell::ShellStatus;
    let (mut running, mut finished): (Vec<_>, Vec<_>) = jobs
        .into_iter()
        .partition(|job| job.status == ShellStatus::Running);
    finished.sort_by_key(|job| std::cmp::Reverse(job.finished_at));
    finished.truncate(FINISHED_COMMAND_TABS);
    running.extend(finished);
    running
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn command_output(job: &codewhale_engine::tools::shell::ShellJobSnapshot) -> Vec<u8> {
    let mut output = job.stdout_tail.clone();
    if !job.stderr_tail.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&job.stderr_tail);
    }
    output.into_bytes()
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn tab_ids(dock: &TerminalDock) -> Vec<String> {
    dock.sessions
        .iter()
        .map(|session| session.id.clone())
        .chain(dock.commands.iter().map(command_tab_id))
        .collect()
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn policy(app: &App) -> codewhale_sandbox::SandboxPolicy {
    codewhale_engine::core::authority::sandbox_policy_for_turn(
        app.mode,
        app.approval_mode,
        app.configured_sandbox_mode.as_deref(),
        &app.workspace,
        codewhale_engine::core::authority::SandboxNetworkAccess::from_config(
            app.configured_sandbox_network,
        ),
    )
}

/// Called by the existing event loop, including when another panel is open.
pub(crate) fn poll(app: &mut App) {
    #[cfg(all(unix, not(target_env = "ohos")))]
    {
        let visible = app.work_surface.panel == RailPanel::Terminal
            && !app.work_surface.dismissed
            && app.work_surface.explicit_view;
        let session_id = app.current_session_id.clone().unwrap_or_default();
        let dock = &mut app.work_surface.terminal;
        if dock.workspace != app.workspace {
            *dock = TerminalDock {
                workspace: app.workspace.clone(),
                ..Default::default()
            };
        }
        tracing::trace!(
            target: "codewhale::terminal_dock",
            visible,
            refreshing = dock.refreshing,
            selected = ?dock.selected,
            last_poll_age_ms = ?dock.last_poll.map(|last| last.elapsed().as_millis()),
            "poll"
        );
        if visible && dock.worker.is_none() && dock.error.is_none() {
            let shell_manager = app
                .runtime_services
                .shell_manager
                .clone()
                .unwrap_or_else(|| {
                    codewhale_engine::tools::shell::new_shared_shell_manager(app.workspace.clone())
                });
            match Worker::start(app.workspace.clone(), shell_manager) {
                Ok(worker) => dock.worker = Some(worker),
                Err(error) => dock.error = Some(error),
            }
        }
        if let Some(worker) = &dock.worker {
            while let Ok(reply) = worker.replies.try_recv() {
                match reply {
                    Reply::Snapshot {
                        requested,
                        sessions,
                        output,
                        commands,
                    } => {
                        tracing::trace!(
                            target: "codewhale::terminal_dock",
                            requested = ?requested,
                            selected = ?dock.selected,
                            accepted = dock.selected == requested,
                            session_states = ?sessions.iter().map(|session| (&session.id, session.state)).collect::<Vec<_>>(),
                            output_bytes = output.as_ref().map_or(0, |chunk| chunk.bytes.len()),
                            "snapshot"
                        );
                        dock.refreshing = false;
                        if dock.selected != requested {
                            continue;
                        }
                        if dock.selected.is_none() {
                            dock.selected = sessions
                                .first()
                                .map(|session| session.id.clone())
                                .or_else(|| commands.first().map(command_tab_id));
                        }
                        let selected_command = dock
                            .selected
                            .as_deref()
                            .and_then(command_job_id)
                            .and_then(|id| commands.iter().find(|job| job.id == id));
                        dock.lost = dock.selected.is_some()
                            && selected_command.is_none()
                            && !sessions
                                .iter()
                                .any(|session| Some(&session.id) == dock.selected.as_ref());
                        let (bytes, dropped) = match selected_command {
                            Some(job) => (command_output(job), 0),
                            None => output
                                .map(|chunk| (chunk.bytes, chunk.offset))
                                .unwrap_or_default(),
                        };
                        if dock.sessions != sessions
                            || dock.commands != commands
                            || dock.output != bytes
                            || dock.dropped != dropped
                        {
                            app.needs_redraw = true;
                        }
                        dock.sessions = sessions;
                        dock.commands = commands;
                        dock.output = bytes;
                        dock.dropped = dropped;
                    }
                    Reply::Created(id) => {
                        dock.creating = false;
                        dock.selected = Some(id);
                        dock.output.clear();
                        dock.size = None;
                        dock.error = None;
                        dock.lost = false;
                        dock.last_poll = None;
                        app.needs_redraw = true;
                    }
                    Reply::InputWritten(id) => {
                        if dock.selected.as_ref() == Some(&id) && dock.error.take().is_some() {
                            app.needs_redraw = true;
                        }
                    }
                    Reply::Error(error) => {
                        dock.creating = false;
                        dock.refreshing = false;
                        dock.error = Some(error);
                        app.needs_redraw = true;
                    }
                }
            }
            if visible
                && !dock.refreshing
                && dock
                    .last_poll
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(100))
            {
                match worker.requests.try_send(Request::Refresh {
                    selected: dock.selected.clone(),
                    session: session_id.clone(),
                }) {
                    Ok(()) => {
                        dock.refreshing = true;
                        dock.last_poll = Some(Instant::now());
                    }
                    Err(error) => {
                        dock.error = Some(error.to_string());
                        app.needs_redraw = true;
                    }
                }
            }
        }
    }
    #[cfg(any(not(unix), target_env = "ohos"))]
    let _ = app;
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn send(app: &mut App, request: Request) -> bool {
    let dock = &mut app.work_surface.terminal;
    let result = dock
        .worker
        .as_ref()
        .ok_or_else(|| "terminal is not ready".to_string())
        .and_then(|worker| {
            worker
                .requests
                .try_send(request)
                .map_err(|error| error.to_string())
        });
    let sent = result.is_ok();
    if let Err(error) = result {
        dock.error = Some(error);
    }
    dock.last_poll = None;
    app.needs_redraw = true;
    sent
}

pub(super) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    use crate::tui::shell_key_routing::route;
    let action = route(crate::tui::shell_key_routing::Focus::TerminalPanel, &key);
    if action == Some(ShellBindingId::TerminalDetach) {
        super::interaction::release_focus(app);
        return true;
    }
    // Shared shell shortcuts keep their meaning; ordinary Tab remains PTY input.
    if action.is_some_and(|id| {
        !matches!(
            id,
            ShellBindingId::TerminalNew
                | ShellBindingId::TerminalNext
                | ShellBindingId::TerminalPrevious
        )
    }) {
        return false;
    }
    #[cfg(all(unix, not(target_env = "ohos")))]
    {
        if action == Some(ShellBindingId::TerminalNew) {
            if app.work_surface.terminal.worker.is_none() {
                app.work_surface.terminal.error = None;
                poll(app);
                if app.work_surface.terminal.worker.is_none() {
                    return true;
                }
            }
            let dock = &mut app.work_surface.terminal;
            dock.selected = Some(uuid::Uuid::new_v4().to_string());
            dock.output.clear();
            dock.creating = true;
            dock.lost = false;
            dock.scroll = 0;
            dock.size = None;
            dock.error = None;
            let sent = send(app, Request::Create(policy(app)));
            app.work_surface.terminal.creating = sent;
            return true;
        }
        if matches!(
            action,
            Some(ShellBindingId::TerminalNext | ShellBindingId::TerminalPrevious)
        ) {
            let dock = &mut app.work_surface.terminal;
            let tabs = tab_ids(dock);
            if !tabs.is_empty() {
                let current = tabs
                    .iter()
                    .position(|id| Some(id) == dock.selected.as_ref());
                let index = match (current, action) {
                    (Some(index), Some(ShellBindingId::TerminalPrevious)) => {
                        (index + tabs.len() - 1) % tabs.len()
                    }
                    (Some(index), _) => (index + 1) % tabs.len(),
                    (None, _) => 0,
                };
                dock.selected = Some(tabs[index].clone());
                dock.output.clear();
                dock.error = None;
                dock.size = None;
                dock.last_poll = None;
                dock.lost = false;
                dock.scroll = 0;
                app.needs_redraw = true;
            }
            return true;
        }
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let dock = &mut app.work_surface.terminal;
            dock.scroll = if key.code == KeyCode::PageUp {
                dock.scroll.saturating_add(8)
            } else {
                dock.scroll.saturating_sub(8)
            };
            app.needs_redraw = true;
            return true;
        }
        if let Some(bytes) = key_bytes(key) {
            input(app, bytes);
        }
    }
    true
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn input(app: &mut App, bytes: Vec<u8>) {
    let dock = &app.work_surface.terminal;
    if dock.selected.as_deref().and_then(command_job_id).is_some() {
        return;
    }
    if let Some(session) = dock
        .sessions
        .iter()
        .find(|session| Some(&session.id) == dock.selected.as_ref())
    {
        send(
            app,
            Request::Input {
                name: session.name.clone(),
                id: session.id.clone(),
                bytes,
                policy: policy(app),
            },
        );
    } else if !dock.sessions.is_empty() {
        app.work_surface.terminal.error = Some(app.tr(MessageId::TerminalDockLost).into_owned());
        app.needs_redraw = true;
    }
}

pub(crate) fn handle_paste(app: &mut App, text: &str) -> bool {
    if app.focus() != crate::tui::shell_key_routing::Focus::TerminalPanel
        || app.work_surface.last_area.is_none()
    {
        return false;
    }
    #[cfg(all(unix, not(target_env = "ohos")))]
    {
        // Paste is deliberate terminal input. Limit it before queuing, and
        // never pass terminal control sequences supplied by the clipboard.
        if text.len() > 64 * 1024
            || text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
        {
            app.work_surface.terminal.error =
                Some(app.tr(MessageId::TerminalDockPasteRefused).into_owned());
        } else {
            input(
                app,
                text.replace("\r\n", "\n").replace('\r', "\n").into_bytes(),
            );
        }
        app.needs_redraw = true;
    }
    #[cfg(any(not(unix), target_env = "ohos"))]
    let _ = text;
    true
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let control =
        key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);
    let text = match key.code {
        KeyCode::Char(c) if control && c.is_ascii() => vec![(c.to_ascii_uppercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) && !key.modifiers.contains(KeyModifiers::CONTROL) {
        Some([vec![0x1b], text].concat())
    } else {
        Some(text)
    }
}

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let style = Style::default()
        .fg(app.ui_theme.text_body)
        .bg(app.ui_theme.panel_bg);
    let muted = Style::default()
        .fg(app.ui_theme.text_muted)
        .bg(app.ui_theme.panel_bg);
    let mut lines = Vec::new();
    #[cfg(all(unix, not(target_env = "ohos")))]
    {
        use ansi_to_tui::IntoText;
        let hints = app
            .tr(MessageId::TerminalDockControls)
            .replace(
                "{new}",
                &display_chord(binding(ShellBindingId::TerminalNew).footer_chord),
            )
            .replace(
                "{next}",
                &display_chord(binding(ShellBindingId::TerminalNext).footer_chord),
            )
            .replace(
                "{detach}",
                &display_chord(binding(ShellBindingId::TerminalDetach).footer_chord),
            );
        let dock = &app.work_surface.terminal;
        let mut session_tabs = Vec::new();
        // On compact hosts, remove help chrome before the shell's output.
        if area.height >= 4 {
            lines.push(Line::styled(hints, muted));
        }
        if dock.creating {
            lines.push(Line::styled(app.tr(MessageId::TerminalDockStarting), muted));
        } else if dock.lost {
            lines.push(Line::styled(app.tr(MessageId::TerminalDockLost), muted));
        } else if dock.sessions.is_empty() && dock.commands.is_empty() {
            lines.push(Line::styled(
                app.tr(MessageId::TerminalDockEmpty)
                    .replace("{new}", binding(ShellBindingId::TerminalNew).footer_chord),
                muted,
            ));
        } else if area.height >= 2 {
            let ordered = dock
                .sessions
                .iter()
                .filter(|session| Some(&session.id) == dock.selected.as_ref())
                .chain(
                    dock.sessions
                        .iter()
                        .filter(|session| Some(&session.id) != dock.selected.as_ref()),
                );
            let mut spans = Vec::new();
            let mut column = area.x;
            for session in ordered {
                let selected = Some(&session.id) == dock.selected.as_ref();
                let status = if let Some(code) = session.exit_code {
                    app.tr(MessageId::TerminalDockExited)
                        .replace("{code}", &code.to_string())
                } else {
                    match session.state {
                        terminal_session::DurableTerminalState::Running => {
                            app.tr(MessageId::SubagentsStatusRunning)
                        }
                        terminal_session::DurableTerminalState::Canceled => {
                            app.tr(MessageId::AutomationRunStatusCanceled)
                        }
                        terminal_session::DurableTerminalState::Failed => {
                            app.tr(MessageId::AutomationRunStatusFailed)
                        }
                        _ => app.tr(MessageId::TerminalDockLive),
                    }
                    .into_owned()
                };
                let activity = session
                    .last_activity
                    .map(|last| format!(" · {}s", last.elapsed().as_secs()))
                    .unwrap_or_default();
                let text = format!(
                    "{}{} ({status}{activity})  ",
                    if selected { "› " } else { "" },
                    session.name
                );
                let width = unicode_width::UnicodeWidthStr::width(text.as_str())
                    .min(usize::from(area.right().saturating_sub(column)))
                    as u16;
                if width == 0 {
                    break;
                }
                session_tabs.push((
                    session.id.clone(),
                    Rect::new(column, area.y.saturating_add(lines.len() as u16), width, 1),
                ));
                column = column.saturating_add(width);
                spans.push(ratatui::text::Span::styled(
                    text,
                    if selected && app.work_surface.focused {
                        style.bg(app.ui_theme.selection_bg)
                    } else {
                        style
                    },
                ));
            }
            for job in &dock.commands {
                let id = command_tab_id(job);
                let selected = Some(&id) == dock.selected.as_ref();
                let status = match job.exit_code {
                    Some(code)
                        if job.status != codewhale_engine::tools::shell::ShellStatus::Running =>
                    {
                        app.tr(MessageId::TerminalDockExited)
                            .replace("{code}", &code.to_string())
                    }
                    _ => app.tr(MessageId::SubagentsStatusRunning).into_owned(),
                };
                let command: String = job.command.chars().take(32).collect();
                let ellipsis = if job.command.chars().count() > 32 {
                    "…"
                } else {
                    ""
                };
                let text = format!(
                    "{}$ {command}{ellipsis} ({status} · {}s)  ",
                    if selected { "› " } else { "" },
                    job.elapsed_ms / 1000
                );
                let width = unicode_width::UnicodeWidthStr::width(text.as_str())
                    .min(usize::from(area.right().saturating_sub(column)))
                    as u16;
                if width == 0 {
                    break;
                }
                session_tabs.push((
                    id,
                    Rect::new(column, area.y.saturating_add(lines.len() as u16), width, 1),
                ));
                column = column.saturating_add(width);
                spans.push(ratatui::text::Span::styled(
                    text,
                    if selected && app.work_surface.focused {
                        style.bg(app.ui_theme.selection_bg)
                    } else {
                        style
                    },
                ));
            }
            lines.push(Line::from(spans));
        }
        if let Some(error) = &dock.error {
            lines.push(Line::styled(
                app.tr(MessageId::TerminalDockError)
                    .replace("{error}", error),
                muted,
            ));
        }
        if dock.dropped > 0 {
            lines.push(Line::styled(
                app.tr(MessageId::TerminalDockDropped)
                    .replace("{count}", &dock.dropped.to_string()),
                muted,
            ));
        }
        let mut safe = String::new();
        codewhale_tui_kit::osc8::strip_ansi_keep_sgr_into(
            &String::from_utf8_lossy(&dock.output),
            &mut safe,
        );
        let output = safe.into_text().unwrap_or_default();
        let available = usize::from(area.height).saturating_sub(lines.len());
        let scroll = dock
            .scroll
            .min(output.lines.len().saturating_sub(available));
        let skip = output
            .lines
            .len()
            .saturating_sub(available)
            .saturating_sub(scroll);
        lines.extend(output.lines.into_iter().skip(skip).take(available));
        if let Some(session) = dock
            .sessions
            .iter()
            .find(|session| Some(&session.id) == dock.selected.as_ref())
        {
            let size = (available.max(1) as u16, area.width.max(1));
            if dock.size != Some(size) {
                let request = Request::Resize {
                    name: session.name.clone(),
                    id: session.id.clone(),
                    rows: size.0,
                    cols: size.1,
                };
                if send(app, request) {
                    app.work_surface.terminal.size = Some(size);
                }
            }
        }
        app.work_surface.terminal.session_tabs = session_tabs;
        app.work_surface.terminal.scroll = scroll;
    }
    #[cfg(any(not(unix), target_env = "ohos"))]
    lines.push(Line::styled(
        app.tr(MessageId::TerminalDockUnsupported),
        muted,
    ));
    Paragraph::new(lines)
        .style(style)
        .render(area, frame.buffer_mut());
    app.work_surface.visible_rows = usize::from(area.height);
    app.work_surface.total_rows = usize::from(area.height);
    app.work_surface.hitboxes.clear();
    app.work_surface.latest_rows.clear();
}

pub(super) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if app.work_surface.panel != super::model::RailPanel::Terminal {
        return false;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            super::interaction::claim_focus(app);
            #[cfg(all(unix, not(target_env = "ohos")))]
            if let Some(id) = app
                .work_surface
                .terminal
                .session_tabs
                .iter()
                .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                .map(|(id, _)| id.clone())
            {
                let dock = &mut app.work_surface.terminal;
                dock.selected = Some(id);
                dock.output.clear();
                dock.error = None;
                dock.size = None;
                dock.last_poll = None;
                dock.lost = false;
                dock.scroll = 0;
            }
            app.needs_redraw = true;
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            #[cfg(all(unix, not(target_env = "ohos")))]
            {
                let dock = &mut app.work_surface.terminal;
                dock.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    dock.scroll.saturating_add(3)
                } else {
                    dock.scroll.saturating_sub(3)
                };
                app.needs_redraw = true;
            }
        }
        _ => {}
    }
    true
}
