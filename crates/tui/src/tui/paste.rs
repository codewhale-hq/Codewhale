//! Paste-burst handling — turn rapid keystrokes (terminals without bracketed
//! paste) into a single committed buffer instead of N individual chars.
//!
//! Extracted from `tui/ui.rs` (P1.2). The owning state machine lives on
//! `App.paste_burst` (`tui::paste_burst`); these helpers wire it to the key
//! event loop and the composer's text buffer.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, looks_like_slash_command_input};
use super::clipboard::{ClipboardContent, ClipboardRead};
use super::shell_key_routing::Focus;
use super::views::ModalKind;
use codewhale_localization::MessageId;
use codewhale_tui_kit::paste_burst::CharDecision;

/// Terminals known to deliver `Event::Paste` for a real paste.
///
/// `bracketed_paste_seen` alone waits for proof: it only flips after the
/// person has pasted once, so every session ran the rapid-keystroke
/// heuristic until then. That heuristic re-arms a ~120 ms Enter-suppression
/// window on each character, and the composer's send cue honestly reports it
/// — so the `[↵]` control strobed dim-to-bold once per keystroke while
/// someone typed at an ordinary pace. Reported from a live session.
///
/// Naming the terminals we have verified is deliberately narrower than
/// trusting `EnableBracketedPaste` to have succeeded: `execute!` returning
/// `Ok` only means the escape was written, not that the terminal honors it,
/// and a terminal that ignores it would submit on a pasted newline. That is
/// data loss, so the fallback stays for everything not on this list.
/// Windows input keeps the fallback until an actual `Event::Paste`: Windows
/// Terminal can advertise bracketed paste while the input path still delivers
/// individual key events (#6427). Its session marker is not delivery proof.
///
/// Known limitation: a non-Windows terminal that sets one of the remaining
/// identifiers while not honoring bracketed paste would still lose the guard.
pub(crate) fn terminal_delivers_bracketed_paste() -> bool {
    use std::io::IsTerminal;

    // Only a real interactive terminal gets the trust. Under `cargo test`
    // stdout is captured, so every unit test keeps the heuristic and stays
    // hermetic regardless of the `TERM_PROGRAM` the developer happens to be
    // running in — the first version of this read the ambient environment
    // and broke five paste tests on a Mac running Terminal.app.
    terminal_identity_allows_paste_bypass(
        std::io::stdout().is_terminal(),
        cfg!(windows) || std::env::var_os("WT_SESSION").is_some(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
    )
}

fn terminal_identity_allows_paste_bypass(
    interactive: bool,
    windows_input: bool,
    program: Option<&str>,
) -> bool {
    interactive
        && !windows_input
        && program.is_some_and(|program| {
            matches!(
                program.trim(),
                "ghostty" | "Ghostty" | "iTerm.app" | "WezTerm" | "Apple_Terminal"
            )
        })
}

/// Process a key in the context of paste-burst detection. Returns `true`
/// when the key was fully handled by the paste machinery (caller skips
/// further input handling); `false` when the key still needs the normal
/// composer path.
pub fn handle_paste_burst_key(app: &mut App, key: &KeyEvent, now: Instant) -> bool {
    if !app.use_paste_burst_detection {
        return false;
    }
    // Once we've observed a real `Event::Paste` in this session, bracketed
    // paste is verified working and the rapid-keystroke heuristic is
    // unnecessary. Skipping it eliminates false positives on fast typing /
    // IME commits / autocomplete on terminals with reliable bracketed
    // paste (the dominant case on iTerm2 / Ghostty / WezTerm / Windows
    // Terminal).
    if app.bracketed_paste_seen || (app.bracketed_paste_trusted && app.use_bracketed_paste) {
        return false;
    }

    let has_ctrl_alt_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::ALT)
        || key.modifiers.contains(KeyModifiers::SUPER);

    match key.code {
        KeyCode::Enter => {
            if app.paste_burst.is_active()
                && in_command_context(app)
                && let Some(pending) = app.paste_burst.flush_before_modified_input()
            {
                app.insert_str(&pending);
            }
            if app.paste_burst.append_newline_if_active(now) {
                if let Some(chunk) = app.paste_burst.take_chunk_if_full() {
                    app.insert_str(&chunk);
                }
                return true;
            }
            if app.paste_burst.newline_should_insert_instead_of_submit(now) {
                app.insert_char('\n');
                // Deliberately no `extend_window` here. This Enter arrived
                // with no burst being assembled, so it is only *maybe* a
                // pasted newline. Re-arming on that guess let each absorbed
                // Enter buy another 120ms, so a user pressing Enter to send
                // never submitted — every press just added a newline. The
                // window now always expires 120ms after the last real
                // keystroke; newlines genuinely inside a paste are absorbed
                // by `append_newline_if_active` above, which does re-arm.
                return true;
            }
        }
        KeyCode::Char(c) if !has_ctrl_alt_or_super => {
            if !c.is_ascii() {
                // IME-committed characters (Chinese, Japanese, Korean)
                // arrive as individual KeyCode::Char events, typically with
                // tens-of-milliseconds gaps between each committed character,
                // so a lone commit still inserts straight into the composer.
                // A run at paste speed is a raw paste: it buffers like an
                // ASCII burst and keeps the full Enter-suppression window
                // armed (#1302) while a lone commit earns the short window
                // (`PasteBurst::arm_window_for_direct_char`).
                let rapid_chars = app.paste_burst.note_plain_char(now);
                if app.paste_burst.is_buffering() {
                    app.paste_burst.append_char_to_buffer(c, now);
                    if let Some(chunk) = app.paste_burst.take_chunk_if_full() {
                        app.insert_str(&chunk);
                    }
                } else {
                    if let Some(pending) = app.paste_burst.flush_before_modified_input() {
                        app.insert_str(&pending);
                    }
                    if rapid_chars >= 2 {
                        app.paste_burst.begin_buffer_from_now(c, now);
                        return true;
                    }
                    app.paste_burst.arm_window_for_direct_char(now, rapid_chars);
                    return false;
                }
                return true;
            }

            let decision = app.paste_burst.on_plain_char(c, now);
            return handle_paste_burst_decision(app, decision, c, now);
        }
        _ => {}
    }

    false
}

/// Apply a paste-burst decision to the composer buffer. The burst never
/// rewrites text the user already has on the composer line: chars before
/// the burst stay exactly as typed, and buffering starts from the current
/// char (Y-7 — the old retro-grab deleted and reinserted typed text on a
/// timing guess and scrambled fast input).
pub fn handle_paste_burst_decision(
    app: &mut App,
    decision: CharDecision,
    c: char,
    now: Instant,
) -> bool {
    match decision {
        CharDecision::RetainFirstChar => true,
        CharDecision::BeginBufferFromPending | CharDecision::BufferAppend => {
            app.paste_burst.append_char_to_buffer(c, now);
            if let Some(chunk) = app.paste_burst.take_chunk_if_full() {
                app.insert_str(&chunk);
            }
            true
        }
        CharDecision::BeginBuffer => {
            app.paste_burst.begin_buffer_from_now(c, now);
            true
        }
    }
}

fn in_command_context(app: &App) -> bool {
    let mut composite = app.input.clone();
    composite.push_str(&app.paste_burst.held_text());
    looks_like_slash_command_input(&composite)
}

/// Where a paste came from. Every paste, terminal-delivered or read from the
/// clipboard, reaches [`handle_paste`]: the one place that decides who owns
/// it and whether the clipboard has to be asked at all.
pub(crate) enum PasteSource<'a> {
    /// `Event::Paste`: the terminal already delivered the text. Windows
    /// Terminal sends an empty one for an image-only clipboard ("send an empty
    /// bracketed paste even if the clipboard was empty"), which is the only
    /// signal a TUI gets that the person pasted.
    Terminal(&'a str),
    /// Cmd/Ctrl+V reached Codewhale, or the mouse menu's Paste: read the
    /// clipboard.
    Shortcut,
    /// Only the release of Ctrl+V arrived: the terminal took the chord and
    /// delivers any text itself, so only an image is Codewhale's to attach.
    ReleaseOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasteTarget {
    TerminalPanel,
    HistorySearch,
    ProviderPicker,
    Modal,
    Composer,
}

/// A clipboard read still running on the reader thread. One at a time: a
/// paste while it runs is the other edge of the same key press, or a repeat
/// the person will see land once the read answers.
pub(crate) struct PendingPaste {
    target: PasteTarget,
    image_only: bool,
    started: Instant,
    rx: std::sync::mpsc::Receiver<Option<ClipboardContent>>,
}

/// The slowest read finishes inside this: two `wl-paste` waits of 2 s, or
/// PowerShell's 10 s image fallback plus the backend open. A reader stuck
/// past it is abandoned so the next paste can start a fresh one.
const PENDING_PASTE_LIMIT: Duration = Duration::from_secs(15);
const PENDING_PASTE_POLL: Duration = Duration::from_millis(16);

fn paste_target(app: &App) -> PasteTarget {
    if app.focus() == Focus::TerminalPanel && app.work_surface.last_area.is_some() {
        PasteTarget::TerminalPanel
    } else if app.view_stack.top_kind() == Some(ModalKind::ProviderPicker) {
        PasteTarget::ProviderPicker
    } else if !app.view_stack.is_empty() {
        PasteTarget::Modal
    } else if app.is_history_search_active() {
        PasteTarget::HistorySearch
    } else {
        PasteTarget::Composer
    }
}

/// Route one paste. Returns whether the paste was taken; a clipboard shortcut
/// the focused surface does not paste into (a terminal panel, a non-credential
/// modal) is left for that surface's own key handling.
pub(crate) fn handle_paste(app: &mut App, source: PasteSource<'_>) -> bool {
    let target = paste_target(app);
    match source {
        PasteSource::Terminal(text) => {
            tracing::debug!(
                paste_bytes = text.len(),
                paste_chars = text.chars().count(),
                "Received bracketed paste event"
            );
            app.bracketed_paste_seen = true;
            if text.is_empty() && target == PasteTarget::Composer {
                request_clipboard(app, target, false, true);
            } else {
                deliver_text(app, target, text);
            }
            true
        }
        PasteSource::Shortcut => match target {
            PasteTarget::TerminalPanel | PasteTarget::Modal => false,
            _ => {
                request_clipboard(app, target, false, false);
                true
            }
        },
        PasteSource::ReleaseOnly => {
            if target == PasteTarget::Composer {
                request_clipboard(app, target, true, true);
            }
            true
        }
    }
}

fn request_clipboard(app: &mut App, target: PasteTarget, image_only: bool, quiet: bool) {
    // A terminal may forward both edges of Ctrl+V. The window runs from the
    // end of the previous read, so a slow read cannot push the second edge
    // outside it, and a read still running absorbs that edge outright.
    if app.pending_paste.is_some() || app.clipboard_paste_within_dedupe_window() {
        return;
    }
    // In SSH sessions without a forwarded display the terminal client owns
    // paste: a direct shortcut gets the local terminal-paste instruction and
    // `Event::Paste` stays the data path.
    if app.clipboard.requires_terminal_paste() {
        if !quiet {
            app.status_message = Some(app.tr(MessageId::ClipboardSshPasteHint).into_owned());
            app.note_clipboard_paste();
        }
        return;
    }
    let prefer_markdown = target == PasteTarget::Composer && !image_only;
    match app.clipboard.read(app.workspace.as_path(), prefer_markdown) {
        ClipboardRead::Ready(content) => finish_paste(app, target, image_only, content),
        ClipboardRead::Pending(rx) => {
            app.pending_paste = Some(PendingPaste {
                target,
                image_only,
                started: Instant::now(),
                rx,
            });
        }
    }
}

/// Apply a finished clipboard read. Called from the event loop every tick.
pub(crate) fn poll_pending_paste(app: &mut App) {
    let Some(pending) = app.pending_paste.as_ref() else {
        return;
    };
    let content = match pending.rx.try_recv() {
        Ok(content) => content,
        Err(std::sync::mpsc::TryRecvError::Empty)
            if pending.started.elapsed() < PENDING_PASTE_LIMIT =>
        {
            return;
        }
        Err(_) => {
            tracing::warn!(
                target: "codewhale_tui::clipboard",
                "clipboard read did not answer; abandoned"
            );
            app.pending_paste = None;
            return;
        }
    };
    let Some(pending) = app.pending_paste.take() else {
        return;
    };
    if paste_target(app) == pending.target {
        finish_paste(app, pending.target, pending.image_only, content);
    } else {
        // Focus moved while the clipboard answered: the paste belonged to a
        // surface that is gone, and must not land in whatever replaced it (a
        // credential must never reach the composer under a closed picker).
        app.note_clipboard_paste();
    }
}

/// Shortens the event loop's wait while a clipboard read is running.
pub(crate) fn pending_paste_poll_delay(app: &App) -> Option<Duration> {
    app.pending_paste.as_ref().map(|_| PENDING_PASTE_POLL)
}

fn finish_paste(
    app: &mut App,
    target: PasteTarget,
    image_only: bool,
    content: Option<ClipboardContent>,
) {
    app.note_clipboard_paste();
    app.needs_redraw = true;
    match content {
        // Images go only to the composer; an open credential modal consumes
        // and ignores one rather than leaking it to the draft beneath.
        Some(ClipboardContent::Image(pasted)) if target == PasteTarget::Composer => {
            crate::tui::work_surface::release_focus(app);
            let description = format!("{} ({})", pasted.short_label(), pasted.size_label());
            app.insert_media_attachment("image", &pasted.path, Some(&description));
            app.status_message = Some(format!("Attached image: {description}"));
        }
        Some(ClipboardContent::Text(text)) if !image_only => deliver_text(app, target, &text),
        // The read cannot tell an empty clipboard from an unreadable one;
        // either way the person asked to paste and nothing happened.
        None if !image_only && target == PasteTarget::Composer => {
            app.status_message = Some(app.tr(MessageId::ClipboardNothingToPaste).into_owned());
        }
        _ => {}
    }
}

fn deliver_text(app: &mut App, target: PasteTarget, text: &str) {
    match target {
        PasteTarget::TerminalPanel => {
            crate::tui::work_surface::handle_terminal_paste(app, text);
        }
        PasteTarget::HistorySearch => app.history_search_insert_str(text),
        PasteTarget::ProviderPicker | PasteTarget::Modal => {
            let _ = app.view_stack.handle_paste(text);
        }
        PasteTarget::Composer => {
            crate::tui::work_surface::release_focus(app);
            app.insert_paste_text(text);
        }
    }
}

#[cfg(test)]
mod bracketed_paste_trust_tests {
    use super::{terminal_delivers_bracketed_paste, terminal_identity_allows_paste_bypass};
    use crate::test_support::{EnvVarGuard, lock_test_env};

    /// Under `cargo test` stdout is captured, so the probe refuses to trust
    /// anything — the heuristic stays on and every existing paste test keeps
    /// the behaviour it was written against, whatever terminal the developer
    /// is in. This is the hermeticity the first version of this check broke.
    #[test]
    fn a_captured_stdout_is_never_trusted() {
        let _env = lock_test_env();
        for program in ["ghostty", "iTerm.app", "WezTerm", "some-new-terminal"] {
            let _guard = EnvVarGuard::set("TERM_PROGRAM", program);
            assert!(
                !terminal_delivers_bracketed_paste(),
                "{program} must not be trusted without a real terminal"
            );
        }
    }

    #[test]
    fn windows_input_requires_observed_paste_even_with_a_known_terminal_identity() {
        for program in [
            None,
            Some("Windows Terminal"),
            Some("iTerm.app"),
            Some("ghostty"),
        ] {
            assert!(
                !terminal_identity_allows_paste_bypass(true, true, program),
                "Windows input must retain the fallback before Event::Paste: {program:?}"
            );
        }
        assert!(terminal_identity_allows_paste_bypass(
            true,
            false,
            Some("iTerm.app")
        ));
        assert!(!terminal_identity_allows_paste_bypass(
            false,
            false,
            Some("iTerm.app")
        ));
        assert!(!terminal_identity_allows_paste_bypass(
            true,
            false,
            Some("unknown")
        ));
    }

    #[test]
    fn windows_terminal_is_trusted_through_its_session_marker() {
        let _env = lock_test_env();
        let _program = EnvVarGuard::remove("TERM_PROGRAM");
        let _wt = EnvVarGuard::set("WT_SESSION", "abc-123");
        assert!(
            !terminal_delivers_bracketed_paste(),
            "still gated on a real terminal, which a test never has"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    fn plain(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    #[test]
    fn a_clipboard_read_lands_when_it_answers_and_absorbs_the_second_edge() {
        let mut app = test_app();
        let (clipboard, tx) = crate::tui::clipboard::ClipboardHandler::with_pending_read_for_test();
        app.clipboard = clipboard;
        assert!(handle_paste(&mut app, PasteSource::Shortcut));
        assert!(pending_paste_poll_delay(&app).is_some());

        // The other edge of the same chord arrives while the read runs.
        assert!(handle_paste(&mut app, PasteSource::Shortcut));
        poll_pending_paste(&mut app);
        assert!(app.input.is_empty());

        tx.send(Some(ClipboardContent::Text("pasted".to_string())))
            .unwrap();
        poll_pending_paste(&mut app);
        assert_eq!(app.input, "pasted");
        assert!(app.pending_paste.is_none());
        assert!(handle_paste(&mut app, PasteSource::Shortcut));
        assert_eq!(app.input, "pasted");
    }

    #[test]
    fn a_clipboard_read_for_a_surface_that_closed_is_dropped() {
        let mut app = test_app();
        let (clipboard, tx) = crate::tui::clipboard::ClipboardHandler::with_pending_read_for_test();
        app.clipboard = clipboard;
        assert!(handle_paste(&mut app, PasteSource::Shortcut));
        // Focus moves to history search before the clipboard answers.
        app.start_history_search();

        tx.send(Some(ClipboardContent::Text("sk-secret".to_string())))
            .unwrap();
        poll_pending_paste(&mut app);

        assert!(app.input.is_empty(), "{}", app.input);
        assert!(app.pending_paste.is_none());
        assert!(
            !app.history_search_query()
                .unwrap_or_default()
                .contains("sk-secret")
        );
    }

    #[test]
    fn requested_bracketed_paste_keeps_raw_multiline_fallback_until_verified() {
        for pasted in [
            "first line\nsecond line\nthird line",
            "多行粘贴测试\n行1\n行2\n行3",
        ] {
            let mut app = test_app();
            assert!(app.use_bracketed_paste);
            assert!(!app.bracketed_paste_seen);
            let t0 = Instant::now();
            for (i, ch) in pasted.chars().enumerate() {
                let key = if ch == '\n' {
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
                } else {
                    plain(ch)
                };
                assert!(
                    handle_paste_burst_key(&mut app, &key, t0 + Duration::from_millis(i as u64)),
                    "raw paste {ch:?} must not reach the submit path"
                );
            }
            app.flush_paste_burst_if_due(t0 + Duration::from_secs(1));
            assert_eq!(app.input, pasted);
            assert!(
                !handle_paste_burst_key(
                    &mut app,
                    &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    t0 + Duration::from_secs(2),
                ),
                "a deliberate Enter after the paste must reach the submit path"
            );
        }
    }

    #[test]
    fn mixed_ascii_cjk_24_line_paste_keeps_newlines() {
        let expected: String = (1..=24)
            .map(|n| format!("cw-clipboard-{n:02} 漢字 🐳 line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut app = test_app();
        let start = Instant::now();
        for (index, ch) in expected.chars().enumerate() {
            let now = start + Duration::from_millis(index as u64);
            app.flush_paste_burst_if_due(now);
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(ch)
            };
            assert!(
                handle_paste_burst_key(&mut app, &key, now),
                "char {index} {ch:?} not handled"
            );
        }
        app.flush_paste_burst_if_due(start + Duration::from_secs(1));
        assert_eq!(app.input, expected);
    }

    #[test]
    #[cfg(windows)]
    fn slow_paced_mixed_ascii_cjk_paste_keeps_newlines() {
        let expected: String = (1..=6)
            .map(|n| format!("cw-clipboard-{n:02} 漢字 🐳 line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut app = test_app();
        let start = Instant::now();
        for (index, ch) in expected.chars().enumerate() {
            let now = start + Duration::from_millis(30 * index as u64);
            app.flush_paste_burst_if_due(now);
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(ch)
            };
            assert!(
                handle_paste_burst_key(&mut app, &key, now),
                "char {index} {ch:?} not handled"
            );
        }
        app.flush_paste_burst_if_due(start + Duration::from_secs(60));
        assert_eq!(app.input, expected);
    }

    #[cfg(windows)]
    #[test]
    fn fifty_ms_windows_typing_and_ime_commits_leave_send_available() {
        for text in ["abc", "你好啊", "hello world", "你好世界你好世界"] {
            let mut app = test_app();
            let start = Instant::now();
            let mut last = start;
            for (i, ch) in text.chars().enumerate() {
                last = start + Duration::from_millis(50 * i as u64);
                app.flush_paste_burst_if_due(last);
                let handled = handle_paste_burst_key(&mut app, &plain(ch), last);
                if ch.is_ascii() {
                    assert!(handled);
                } else {
                    assert!(!handled);
                    app.insert_char(ch);
                }
            }
            let enter = last + Duration::from_millis(30);
            app.flush_paste_burst_if_due(enter);
            assert!(app.composer_enter_would_submit(true, enter), "{text}");
            assert!(!handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                enter,
            ));
            assert_eq!(app.input, text);
        }
    }

    #[test]
    fn composer_enter_uses_event_observed_time_not_processing_time() {
        let mut app = test_app();
        let t0 = Instant::now() - Duration::from_millis(880);
        for (i, ch) in "cw-clipboard-01".chars().enumerate() {
            let now = t0 + Duration::from_millis(5 * i as u64);
            app.flush_paste_burst_if_due(now);
            assert!(handle_paste_burst_key(&mut app, &plain(ch), now));
        }
        let enter_observed_at = t0 + Duration::from_millis(5 * 15 + 40);
        app.flush_paste_burst_if_due(enter_observed_at);
        assert!(
            app.handle_composer_enter(true, enter_observed_at).is_none(),
            "a mid-stream Enter must insert a newline, not submit"
        );
        assert!(
            !app.composer_enter_would_submit(true, enter_observed_at),
            "the would-submit probe must agree on the same clock"
        );
        app.flush_paste_burst_if_due(enter_observed_at + Duration::from_secs(1));
        assert_eq!(app.input, "cw-clipboard-01\n");
    }

    #[test]
    fn windows_terminal_raw_five_line_paste_never_reaches_submit() {
        // #6427: test the interactive startup decision, not captured stdout,
        // then send the raw keys that the Windows input backend can produce.
        for pasted in [
            "first line\nsecond line\nthird line\nfourth line\nfifth line",
            "第一行\n第二行\n第三行\n第四行\n第五行",
        ] {
            let mut app = test_app();
            app.bracketed_paste_trusted =
                terminal_identity_allows_paste_bypass(true, true, Some("iTerm.app"));
            let start = Instant::now();
            for (index, ch) in pasted.chars().enumerate() {
                let key = if ch == '\n' {
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
                } else {
                    plain(ch)
                };
                let handled = handle_paste_burst_key(
                    &mut app,
                    &key,
                    start + Duration::from_millis(index as u64),
                );
                if index == 0 && !ch.is_ascii() {
                    assert!(!handled);
                    app.insert_char(ch);
                } else {
                    assert!(handled, "raw pasted {ch:?} must not reach submit");
                }
            }
            app.flush_paste_burst_if_due(start + Duration::from_secs(1));
            assert_eq!(app.input, pasted);
            assert!(!handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                start + Duration::from_secs(2),
            ));
            app.bracketed_paste_seen = true;
            assert!(!handle_paste_burst_key(
                &mut app,
                &plain('x'),
                start + Duration::from_secs(3),
            ));
        }
    }

    #[test]
    fn raw_paste_enter_bypasses_slash_completion_and_deliberate_enter_submits() {
        let mut app = test_app();
        let start = Instant::now();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        for (index, ch) in "/mo\nsecond line".chars().enumerate() {
            let now = start + Duration::from_millis(2 * index as u64);
            app.flush_paste_burst_if_due(now);
            let key = if ch == '\n' { enter } else { plain(ch) };
            assert!(handle_paste_burst_key(&mut app, &key, now));
        }
        app.flush_paste_burst_if_due(start + Duration::from_secs(1));
        assert_eq!(app.input, "/mo\nsecond line");

        let mut app = test_app();
        for (index, ch) in "/new".chars().enumerate() {
            let now = start + Duration::from_millis(50 * index as u64);
            app.flush_paste_burst_if_due(now);
            assert!(handle_paste_burst_key(&mut app, &plain(ch), now));
        }
        let now = start + Duration::from_millis(175);
        app.flush_paste_burst_if_due(now);
        assert!(!handle_paste_burst_key(&mut app, &enter, now));
        assert_eq!(
            app.handle_composer_enter(true, now).as_deref(),
            Some("/new")
        );
    }

    /// The same burst stream ending in a normal prompt must still absorb
    /// its Enter on terminals without bracketed paste — that absorption is
    /// the heuristic's whole job for real multi-line pastes (#1073).
    #[test]
    fn enter_on_buffered_plain_text_is_still_absorbed() {
        let mut app = test_app();
        let t0 = Instant::now();

        for (i, ch) in "hello world".chars().enumerate() {
            assert!(handle_paste_burst_key(
                &mut app,
                &plain(ch),
                t0 + Duration::from_millis(2 * i as u64)
            ));
        }
        assert!(handle_paste_burst_key(
            &mut app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            t0 + Duration::from_millis(60),
        ));
    }

    #[test]
    fn raw_short_cjk_multiline_paste_buffers_enter_instead_of_submitting() {
        // #1302: pasting short CJK content like "请联网搜索：\nSTM32 …" used
        // to silently submit the first line because the heuristic decided
        // it wasn't paste-like (no whitespace + under 16 chars). The
        // non-ASCII bypass now classifies it as a paste so the Enter is
        // absorbed into the burst buffer.
        let mut app = test_app();
        let t0 = Instant::now();

        let pasted = "请联网搜索：\nSTM32 商业应用案例";
        for (i, ch) in pasted.chars().enumerate() {
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(ch)
            };
            let handled =
                handle_paste_burst_key(&mut app, &key, t0 + Duration::from_millis(i as u64));
            assert!(
                handled,
                "raw paste character {ch:?} must be handled by paste-burst detection"
            );
        }

        app.flush_paste_burst_if_due(t0 + Duration::from_secs(1));
        assert_eq!(app.input, pasted);
    }

    #[test]
    fn raw_multiline_paste_buffers_enter_instead_of_submitting() {
        let mut app = test_app();
        let t0 = Instant::now();

        assert!(handle_paste_burst_key(&mut app, &plain('a'), t0));
        assert!(handle_paste_burst_key(
            &mut app,
            &plain('b'),
            t0 + Duration::from_millis(1)
        ));
        assert!(handle_paste_burst_key(
            &mut app,
            &plain('c'),
            t0 + Duration::from_millis(2)
        ));
        assert!(handle_paste_burst_key(
            &mut app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            t0 + Duration::from_millis(3)
        ));

        assert!(app.input.is_empty(), "paste remains buffered until idle");
        assert!(app.flush_paste_burst_if_due(
            t0 + Duration::from_millis(3)
                + codewhale_tui_kit::paste_burst::PasteBurst::recommended_active_flush_delay()
        ));
        assert_eq!(app.input, "abc\n");
    }

    /// A raw CJK paste can open with a one-character line
    /// ("好\n…"). That single character never forms a paste-speed *run*, so
    /// the short window is all that protects the embedded newline — the
    /// newline arrives within the burst interval, so it must still be
    /// absorbed rather than submitting "好" on its own (#1302).
    #[test]
    fn raw_paste_with_single_char_first_line_still_absorbs_its_newline() {
        let mut app = test_app();
        let t0 = Instant::now();

        assert!(!handle_paste_burst_key(&mut app, &plain('好'), t0));
        app.insert_char('好');
        assert!(
            handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                t0 + Duration::from_millis(1),
            ),
            "the newline of a raw paste lands within the burst interval and \
             must be absorbed, not submitted"
        );
        assert_eq!(app.input, "好\n");
    }

    /// The IME half of the same ambiguity: one committed character followed
    /// by a human-speed Enter is a send gesture. `handle_paste_burst_key`
    /// must decline the Enter so it reaches the normal submit path.
    #[test]
    fn ime_commit_then_human_enter_falls_through_to_submit() {
        let mut app = test_app();
        let t0 = Instant::now();

        assert!(!handle_paste_burst_key(&mut app, &plain('好'), t0));
        app.insert_char('好');
        assert_eq!(app.input, "好");

        assert!(
            !handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                t0 + Duration::from_millis(30),
            ),
            "Enter 30ms after an IME candidate commit is a send, not a \
             pasted newline"
        );
        assert_eq!(app.input, "好", "no stray newline may be inserted");
    }

    /// A whole IME-typed CJK sentence, one commit at a time at human speed,
    /// followed by Enter: every character lands verbatim and the Enter still
    /// reaches the submit path. A 50ms cadence alone must not establish a
    /// paste stream (#7398).
    #[test]
    fn ime_typed_sentence_then_enter_falls_through_to_submit() {
        let mut app = test_app();
        let t0 = Instant::now();

        for (i, ch) in "你好世界".chars().enumerate() {
            let now = t0 + Duration::from_millis(200 * i as u64);
            assert!(!handle_paste_burst_key(&mut app, &plain(ch), now));
            app.insert_char(ch);
        }
        assert_eq!(app.input, "你好世界");

        assert!(
            !handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                t0 + Duration::from_millis(700),
            ),
            "Enter after an IME-typed CJK message must submit"
        );
        assert_eq!(app.input, "你好世界");
    }

    /// Absorbing an Enter outside an active burst must not re-arm the
    /// suppression window. It used to, so each swallowed Enter bought
    /// another 120ms and a user pressing Enter to send only ever added
    /// newlines. The first Enter is still absorbed (#1073 trailing-newline
    /// protection); the next one submits.
    #[test]
    fn absorbed_enter_does_not_extend_the_suppression_window() {
        let mut app = test_app();
        let t0 = Instant::now();

        // Unbracketed paste of "abc" with no trailing newline.
        for (i, ch) in "abc".chars().enumerate() {
            let now = t0 + Duration::from_millis(i as u64);
            assert!(handle_paste_burst_key(&mut app, &plain(ch), now));
        }
        let last_char = t0 + Duration::from_millis(2);
        assert!(app.flush_paste_burst_if_due(
            last_char
                + codewhale_tui_kit::paste_burst::PasteBurst::recommended_active_flush_delay()
        ));
        assert_eq!(app.input, "abc");

        // Still inside the window: this could be the paste's trailing
        // newline, so it is absorbed.
        assert!(handle_paste_burst_key(
            &mut app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            last_char + Duration::from_millis(60),
        ));
        assert_eq!(app.input, "abc\n");

        // Past the window measured from the last *keystroke* — the absorbed
        // Enter bought no extra time, so this one submits.
        assert!(
            !handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                last_char + Duration::from_millis(121),
            ),
            "the second Enter must reach the submit path"
        );
        assert_eq!(app.input, "abc\n", "no second newline may be inserted");
    }

    #[test]
    fn paste_buffered_question_mark_does_not_fall_through_to_help_shortcut() {
        let mut app = test_app();
        let t0 = Instant::now();

        assert!(handle_paste_burst_key(&mut app, &plain('?'), t0));

        assert!(app.input.is_empty(), "shortcut char stays buffered first");
        assert!(app.view_stack.is_empty(), "help modal must not open");
        assert!(app.flush_paste_burst_if_due(
            t0 + codewhale_tui_kit::paste_burst::PasteBurst::recommended_flush_delay()
        ));
        assert_eq!(app.input, "?");
    }

    /// Pin the IME-input contract: macOS/Windows input methods commit
    /// each Chinese character as a single `KeyCode::Char(c)` event
    /// after the candidate popup closes. Each codepoint fits in a
    /// `char` (no surrogate pair concerns for BMP chars), so a
    /// straightforward sequence of plain-char events must land in
    /// `app.input` verbatim — no ASCII filter, no byte-vs-char index
    /// drift, no paste-burst false-positive that buffers the chars
    /// indefinitely.
    #[test]
    fn ime_chinese_chars_route_through_to_composer() {
        let mut app = test_app();
        let t0 = Instant::now();

        // Type the four Chinese codepoints "你好世界" one event at a
        // time, with realistic ~50ms gaps so the paste-burst heuristic
        // doesn't classify them as a paste burst.
        for (i, ch) in "你好世界".chars().enumerate() {
            let now = t0 + Duration::from_millis(50 * i as u64);
            assert!(!handle_paste_burst_key(&mut app, &plain(ch), now));
            app.insert_char(ch);
        }

        // Past the active-flush delay so any buffered burst commits.
        let after = t0
            + Duration::from_millis(50 * 4)
            + codewhale_tui_kit::paste_burst::PasteBurst::recommended_active_flush_delay();
        let _ = app.flush_paste_burst_if_due(after);

        assert_eq!(
            app.input, "你好世界",
            "IME-typed Chinese characters must land in composer verbatim"
        );
        assert_eq!(
            app.cursor_position, 4,
            "cursor advances by one per codepoint, not per UTF-8 byte"
        );
    }

    /// Pin the bracketed-paste contract for CJK content: pasted
    /// Chinese text (e.g. when a user copies a question from a
    /// Chinese website and pastes into the composer) must preserve
    /// every codepoint and not double-count multi-byte chars in the
    /// cursor position.
    #[test]
    fn bracketed_paste_preserves_chinese_and_mixed_text() {
        let mut app = test_app();
        app.insert_paste_text("你好世界 hello 世界 café");
        assert_eq!(app.input, "你好世界 hello 世界 café");
        // 4 + 1 + 5 + 1 + 2 + 1 + 4 = 18 codepoints (counting é as one).
        assert_eq!(app.cursor_position, 18);
    }

    #[test]
    fn paste_burst_detection_can_be_disabled_without_disabling_bracketed_paste() {
        let mut app = test_app();
        app.use_paste_burst_detection = false;

        assert!(!handle_paste_burst_key(
            &mut app,
            &plain('a'),
            Instant::now()
        ));
        assert!(app.input.is_empty());

        app.insert_paste_text("line 1\r\nline 2");
        assert_eq!(app.input, "line 1\nline 2");
        assert!(app.use_bracketed_paste);
    }

    /// Once the session has observed a real `Event::Paste`, the
    /// rapid-keystroke heuristic must short-circuit. This pins the new
    /// "auto-disable paste-burst on verified bracketed paste" behavior so
    /// fast typing / IME commits / autocomplete on capable terminals can't
    /// be mis-classified as a paste burst.
    #[test]
    fn paste_burst_short_circuits_after_bracketed_paste_observed() {
        let mut app = test_app();
        app.use_paste_burst_detection = true;
        app.bracketed_paste_seen = true;

        let t0 = Instant::now();
        for (i, ch) in "abcdefgh".chars().enumerate() {
            // Type fast enough that paste-burst would normally fire.
            let now = t0 + Duration::from_millis(i as u64);
            assert!(
                !handle_paste_burst_key(&mut app, &plain(ch), now),
                "paste-burst must NOT consume keys once bracketed paste verified"
            );
        }
        // No buffering — every char fell through to the normal composer
        // path (the test harness doesn't insert chars when the burst
        // handler returns false; we only assert the short-circuit
        // contract here).
        assert!(app.input.is_empty());
    }

    #[test]
    fn oversized_raw_and_bracketed_pastes_preserve_every_char_and_newline() {
        use codewhale_tui_kit::paste_burst::{FlushResult, PasteBurst};

        // The 31-byte line is deliberate. Chunk boundaries drift against the
        // 64 KiB cap instead of landing on it, so the "does this character
        // still fit" branch is actually exercised. A line whose cycles divide
        // the cap exactly — `cw-7403-漢字-🐳-line\n` is 25 bytes, and
        // `25 × 2621` hits 65 525 with 11 bytes to spare — drains at the exact
        // boundary every time and never trips that branch, so #7403's original
        // drop rule could come back without failing this test (verified by
        // injecting it: 0 bytes lost on the 25-byte line, 10 bytes lost on
        // this one).
        let payload = "cw-7403-漢字-🐳-paste-line\n".repeat(11_000);
        let character_count = payload.chars().count();
        assert!(payload.len() > 256 * 1024);

        let mut raw = test_app();
        raw.use_paste_burst_detection = true;
        raw.bracketed_paste_seen = false;
        raw.bracketed_paste_trusted = false;

        let mut now = Instant::now();
        for (index, ch) in payload.chars().enumerate() {
            now += Duration::from_micros(100);
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(ch)
            };
            let handled = handle_paste_burst_key(&mut raw, &key, now);
            if index == 0 && !ch.is_ascii() {
                assert!(!handled);
                raw.insert_char(ch);
            } else {
                assert!(handled);
            }
        }
        let flush_at = now + PasteBurst::recommended_active_flush_delay();
        let FlushResult::TextContinuation(tail) = raw.take_paste_burst_flush_if_enabled(flush_at)
        else {
            panic!("oversized raw paste must flush its final continuation");
        };
        raw.insert_str(&tail);
        assert_eq!(raw.input, payload);
        assert_eq!(raw.cursor_position, character_count);

        let mut bracketed = test_app();
        bracketed.insert_paste_text(&payload);
        assert_eq!(bracketed.input, raw.input);
        assert_eq!(bracketed.cursor_position, character_count);
    }

    /// Drive a raw (non-bracketed) paste one key event per character.
    ///
    /// Intermediate chunks are committed by `handle_paste_burst_key` itself
    /// (`insert_str` right after each `take_chunk_if_full`); this helper only
    /// commits the tail, through `App::take_paste_burst_flush_if_enabled` — the
    /// same non-test entry `event_loop.rs` calls, so the fixture cannot pass
    /// against a test-only stub.
    ///
    /// `input == payload` alone cannot prove chunking: a buffer that never
    /// flushes still ends up correct once the tail lands, so deleting a
    /// `take_chunk_if_full()` check would pass unnoticed. The probe at 60%
    /// fails in exactly that case — the composer is still nearly empty
    /// because everything is sitting in the burst buffer.
    ///
    /// The probe names no cap, but it is not cap-independent: it needs
    /// `floor(0.6·P / cap) · cap ≥ P/3`, i.e. roughly `cap < 4P/15 ≈ 0.27·P`.
    /// For these fixtures that is ~72 KiB against the 64 KiB cap, so there is
    /// margin — but raising `PASTE_BURST_MAX_BYTES` past that fraction of a
    /// fixture would fail this test on a healthy build. Widen the payload
    /// along with the cap.
    ///
    /// # What this cannot see
    ///
    /// The third `take_chunk_if_full()` call, the one after
    /// `append_newline_if_active` in the `KeyCode::Enter` arm, is out of reach
    /// here: this loop sends no newline, and the 60% probe would fail on a run
    /// that deliberately parks the buffer just under the cap. That site has its
    /// own fixture — `pasted_newline_crossing_the_chunk_cap_flushes_immediately`
    /// — which fills to `CAP - 1` bytes and then sends one newline, so the
    /// composer length directly reports whether the Enter arm flushed.
    ///
    /// An earlier revision of this comment argued the site needed no test at
    /// all because deleting it "leaves the text complete". That was half right
    /// and dangerous: the *text* survives, but the flush moves from the Enter
    /// to the next plain character, and an assertion placed between the two
    /// sees it. What was untestable was content equality, not the behaviour.
    fn feed_raw_paste(app: &mut App, payload: &str) {
        use codewhale_tui_kit::paste_burst::{FlushResult, PasteBurst};

        let probe_at = payload.chars().count() * 3 / 5;
        let mut now = Instant::now();
        for (index, ch) in payload.chars().enumerate() {
            now += Duration::from_micros(100);
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(ch)
            };
            let handled = handle_paste_burst_key(app, &key, now);
            if index == 0 && !ch.is_ascii() {
                assert!(!handled);
                app.insert_char(ch);
            } else {
                assert!(
                    handled,
                    "raw paste character {ch:?} must be handled by paste-burst detection"
                );
            }
            if index + 1 == probe_at {
                assert!(
                    app.input.len() * 3 >= payload.len(),
                    "60% into a multi-chunk paste the composer holds {} of {} bytes: the \
                     burst buffer is accumulating the paste instead of flushing chunks",
                    app.input.len(),
                    payload.len()
                );
            }
        }
        let flush_at = now + PasteBurst::recommended_active_flush_delay();
        let FlushResult::TextContinuation(tail) = app.take_paste_burst_flush_if_enabled(flush_at)
        else {
            panic!("an oversized raw paste must flush its final continuation, not a first `Paste`");
        };
        app.insert_str(&tail);
    }

    /// #7403, ASCII append path only: every character here takes
    /// `KeyCode::Char` → `on_plain_char` → `handle_paste_burst_decision`, and
    /// nothing else.
    ///
    /// The mixed-script fixture above cannot stand in for this. Each CJK or
    /// emoji character re-enters the separate `!c.is_ascii()` branch in
    /// `handle_paste_burst_key`, whose own `take_chunk_if_full()` call flushes
    /// the buffer and hides a missing check on the ASCII side — delete only
    /// the ASCII-path check and that test stays green. This payload has no
    /// non-ASCII byte and, deliberately, no newline either: an Enter reaches
    /// `append_newline_if_active`, whose chunk check would stand in again.
    ///
    /// # What this one is for
    ///
    /// One failure mode, precisely: the ASCII-path chunk check going away.
    /// Delete it and the buffer accumulates the whole paste, so the 60% probe
    /// finds the composer empty (0 of 273 000 bytes) and fails.
    ///
    /// It is the *wrong* fixture for #7403's original drop rule, and that is
    /// arithmetic rather than an oversight: every byte here is one UTF-8 byte
    /// wide, so the buffer fills to exactly the cap every cycle and a
    /// `len + ch.len_utf8() <= cap` guard always admits the character. Injecting
    /// that rule leaves this test green (0 bytes lost); the CJK fixture below,
    /// whose characters are 3–4 bytes wide, loses its tail and fails.
    #[test]
    fn ascii_only_raw_paste_over_the_chunk_cap_lands_complete() {
        let mut app = test_app();
        app.use_paste_burst_detection = true;
        app.bracketed_paste_seen = false;
        app.bracketed_paste_trusted = false;

        let payload = "cw-7403-ascii-paste-chunking".repeat(9_750);
        assert!(
            payload.len() > 256 * 1024,
            "fixture must exceed four 64 KiB chunk caps, got {} bytes",
            payload.len()
        );

        feed_raw_paste(&mut app, &payload);

        assert_eq!(app.input, payload);
        assert_eq!(app.cursor_position, payload.chars().count());
    }

    /// #7403, non-ASCII append path only: a CJK-only burst stays in the
    /// `!c.is_ascii()` branch for its whole length, so this is the only
    /// fixture that exercises that branch's `take_chunk_if_full()` call in
    /// isolation. A mixed payload keeps ASCII nearby and that check would
    /// flush on the next ASCII character, masking the gap. Again no newline,
    /// for the same reason as the ASCII case.
    ///
    /// # What this one is for
    ///
    /// Two failure modes: the non-ASCII-path chunk check going away (the 60%
    /// probe then finds 3 of 342 000 bytes), and #7403's original drop rule
    /// coming back. The second works here and not in the ASCII fixture because
    /// these characters are 3–4 bytes wide: the buffer no longer fills to
    /// exactly the cap, the guard rejects the character that would cross it,
    /// and the paste loses its tail.
    #[test]
    fn cjk_only_raw_paste_over_the_chunk_cap_lands_complete() {
        let mut app = test_app();
        app.use_paste_burst_detection = true;
        app.bracketed_paste_seen = false;
        app.bracketed_paste_trusted = false;

        let payload = "漢字テスト🐳".repeat(18_000);
        assert!(
            payload.len() > 256 * 1024,
            "fixture must exceed four 64 KiB chunk caps, got {} bytes",
            payload.len()
        );

        feed_raw_paste(&mut app, &payload);

        assert_eq!(app.input, payload);
        assert_eq!(app.cursor_position, payload.chars().count());
    }

    /// #7403, Enter append path: the third `take_chunk_if_full`, the one right
    /// after `append_newline_if_active`.
    ///
    /// The other fixtures cannot reach it. They carry no newline, and their
    /// 60% probe would fail on a run that deliberately keeps the composer
    /// empty — so this test fills the burst buffer to just under the cap and
    /// then sends exactly one newline.
    ///
    /// That newline is the only thing that can flush the buffer at this point:
    /// no plain character follows, so neither the ASCII nor the non-ASCII
    /// check gets a turn. The composer length is therefore a direct readout of
    /// whether the Enter arm flushed. Delete that `take_chunk_if_full` and the
    /// buffer sits at the cap while the composer stays empty, and the
    /// assertion below fails.
    ///
    /// This is why the tail flush in `feed_raw_paste` is not the only place
    /// chunking can be observed — an earlier revision of these docs claimed
    /// no content assertion could see this site, which was wrong.
    #[test]
    fn pasted_newline_crossing_the_chunk_cap_flushes_immediately() {
        // Mirrors `PASTE_BURST_MAX_BYTES`, which is private to `codewhale-tui-kit`.
        const CAP: usize = 64 * 1024;

        let mut app = test_app();
        app.use_paste_burst_detection = true;
        app.bracketed_paste_seen = false;
        app.bracketed_paste_trusted = false;

        // Fill to CAP - 1 bytes. The plain-character checks fire at `>= CAP`,
        // so nothing flushes yet and the composer must stay empty.
        let mut now = Instant::now();
        while app.paste_burst.held_text().len() < CAP - 1 {
            now += Duration::from_micros(100);
            assert!(handle_paste_burst_key(&mut app, &plain('x'), now));
            assert!(
                app.input.is_empty(),
                "the buffer must not cross the cap while filling; it holds {} bytes",
                app.paste_burst.held_text().len()
            );
        }
        let buffered = app.paste_burst.held_text().len();
        assert_eq!(
            buffered,
            CAP - 1,
            "fill loop must stop one byte under the cap"
        );

        now += Duration::from_micros(100);
        assert!(handle_paste_burst_key(
            &mut app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            now
        ));

        assert_eq!(
            app.input.len(),
            CAP,
            "a pasted newline that crosses the cap must flush the buffer at once instead \
             of waiting for the next plain character"
        );
    }

    #[test]
    fn launch_composer_preserves_newlines_during_active_paste_burst() {
        let mut app = test_app();
        app.use_paste_burst_detection = true;
        app.bracketed_paste_seen = false;
        app.bracketed_paste_trusted = false;
        app.launch.visible = true;
        app.launch.composer_focus = true;

        let payload = "cw-clipboard-01 漢字 🐳 line 1\ncw-clipboard-02 漢字 🐳 line 2\ncw-clipboard-03 漢字 🐳 line 3";
        let mut now = Instant::now();
        for ch in payload.chars() {
            now += Duration::from_millis(1);
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(ch)
            };
            let launch_verdict = crate::tui::underwater::handle_launch_composer_key(&mut app, key);
            assert!(
                matches!(
                    launch_verdict,
                    crate::tui::underwater::LaunchComposerKey::ComposerAuthority
                ),
                "raw paste key {ch:?} on launch screen must route to ComposerAuthority"
            );
            assert!(handle_paste_burst_key(&mut app, &key, now));
        }
        let flush_at =
            now + codewhale_tui_kit::paste_burst::PasteBurst::recommended_active_flush_delay();
        if let codewhale_tui_kit::paste_burst::FlushResult::Paste(rest) =
            app.paste_burst.flush_if_due(flush_at)
        {
            app.insert_str(&rest);
        }
        assert_eq!(app.input, payload);
    }
}
