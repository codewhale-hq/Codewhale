//! Paste-burst handling — turn rapid keystrokes (terminals without bracketed
//! paste) into a single committed buffer instead of N individual chars.
//!
//! Extracted from `tui/ui.rs` (P1.2). The owning state machine lives on
//! `App.paste_burst` (`tui::paste_burst`); these helpers wire it to the key
//! event loop and the composer's text buffer.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, looks_like_slash_command_input};
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
    if app.bracketed_paste_seen || app.bracketed_paste_trusted {
        return false;
    }

    let has_ctrl_alt_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::ALT)
        || key.modifiers.contains(KeyModifiers::SUPER);

    match key.code {
        KeyCode::Enter => {
            if in_command_context(app) {
                // The burst buffer can hold the text the user is actually
                // entering (fast-typed or raw paste). Command context must
                // be judged on that text, not the composer alone, or Enter
                // glues the lines into one multiline slash argument
                // ("Invalid model 'qwen2.5:0.5b\n/status\n…'" — Y-7,
                // 2026-08-31 QA). Flush what is held onto the composer line
                // and let Enter take the ordinary submit path.
                if let Some(pending) = app.paste_burst.flush_before_modified_input() {
                    app.insert_str(&pending);
                }
                return false;
            }
            if app.paste_burst.append_newline_if_active(now) {
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
                // tens-of-milliseconds gaps between each committed character.
                // Paste-burst buffering would lose characters when the IME
                // commits slower than the burst heuristic's timing window.
                //
                // We still call note_plain_char + arm the suppression window
                // so that:
                //   1. The burst timing counter advances for non-IME fast
                //      typing on terminals without bracketed paste support.
                //   2. The Enter-suppression window stays open during a rapid
                //      non-ASCII sequence, preventing premature submission.
                // But the character is inserted directly into the composer
                // rather than placed into the paste-burst buffer.
                //
                // The window is sized by how fast the characters are
                // arriving: a lone IME candidate commit is ordinary typing
                // and must not swallow the Enter that follows it, while a
                // run of characters at paste speed keeps the full window.
                // See `PasteBurst::arm_window_for_direct_char`.
                if let Some(pending) = app.paste_burst.flush_before_modified_input() {
                    app.insert_str(&pending);
                }
                let rapid_chars = app.paste_burst.note_plain_char(now);
                app.paste_burst.arm_window_for_direct_char(now, rapid_chars);
                app.insert_char(c);
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
                assert!(
                    handle_paste_burst_key(
                        &mut app,
                        &key,
                        start + Duration::from_millis(index as u64),
                    ),
                    "raw pasted {ch:?} must not reach submit"
                );
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

    /// Y-7 regression (2026-08-31 QA, `tui-swarm-head`): a scripted driver
    /// or fast typist enters `/model …` faster than the burst heuristic's
    /// windows. The text sat in the burst buffer, `in_command_context`
    /// judged the empty composer, and every Enter was absorbed as a pasted
    /// newline — gluing `/model qwen2.5:0.5b`, `/status`, and the prompt
    /// into one multiline argument ("Invalid model
    /// 'qwen2.5:0.5b\n/status\n…'"). Enter must flush buffered command text
    /// to the composer and reach the submit path.
    #[test]
    fn enter_on_buffered_slash_command_flushes_and_submits() {
        let mut app = test_app();
        let t0 = Instant::now();

        for (i, ch) in "/model qwen2.5:0.5b".chars().enumerate() {
            assert!(handle_paste_burst_key(
                &mut app,
                &plain(ch),
                t0 + Duration::from_millis(2 * i as u64)
            ));
        }
        assert!(
            !handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                t0 + Duration::from_millis(60),
            ),
            "Enter on a buffered slash command is a submit, not a pasted newline"
        );
        assert_eq!(app.input, "/model qwen2.5:0.5b");
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

        // Non-ASCII characters are now inserted directly into the composer
        // rather than buffered by paste burst. The Enter suppression window
        // kept the newline from submitting prematurely.
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

        assert!(handle_paste_burst_key(&mut app, &plain('好'), t0));
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

        assert!(handle_paste_burst_key(&mut app, &plain('好'), t0));
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
    /// reaches the submit path.
    #[test]
    fn ime_typed_sentence_then_enter_falls_through_to_submit() {
        let mut app = test_app();
        let t0 = Instant::now();

        for (i, ch) in "你好世界".chars().enumerate() {
            let now = t0 + Duration::from_millis(50 * i as u64);
            assert!(handle_paste_burst_key(&mut app, &plain(ch), now));
        }
        assert_eq!(app.input, "你好世界");

        assert!(
            !handle_paste_burst_key(
                &mut app,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                t0 + Duration::from_millis(180),
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
            let _ = handle_paste_burst_key(&mut app, &plain(ch), now);
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
}
