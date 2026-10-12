//! Real-PTY proof for the screen-mode switch (`/fullscreen` · `/inline`).
//!
//! Two claims are checked against the *control stream*, not a screenshot,
//! because "did the TUI take the alternate screen" is a terminal-mode fact:
//!
//! 1. `tui.alternate_screen = "never"` starts the session inline — DEC private
//!    mode 1049 is never enabled, so the shell's scrollback stays intact — and
//!    the shell still paints.
//! 2. `/fullscreen` moves the live terminal onto the alternate screen and
//!    `/inline` moves it back, in-process, with the transcript still painting
//!    afterwards. A probe that fails would roll back and leave 1049 where it
//!    was; this asserts the successful path actually flips it.

#![cfg(all(unix, feature = "long-running-tests"))]

use std::time::Duration;

use super::qa_harness;
use qa_harness::harness::{Harness, make_sealed_workspace};
use qa_harness::keys;
use qa_harness::modes::mode;

const ROWS: u16 = 24;
const COLS: u16 = 80;
const STARTUP_WAIT: Duration = Duration::from_secs(15);
const SETTLE_WAIT: Duration = Duration::from_secs(5);
/// Stable proof the live shell repainted after a screen change: the composer
/// placeholder, which every live-shell frame paints in both screen modes.
/// The old `ctx` label no longer qualifies — it stays silent with no model
/// connected, and the workspace caption only paints in the inline stage.
const LIVE_SHELL_SENTINEL: &str = "Type a message";

#[test]
fn offline_queue_late_unbracketed_submit_keeps_composer_and_commands_responsive() {
    // #5999 requires the burst heuristic to stay armed: type_line() uses
    // bracketed paste and would hide the original queue/session-id wedge.
    for (rows, cols) in [(24, 80), (32, 100)] {
        for delay_ms in [150, 250, 400] {
            let workspace = make_sealed_workspace().expect("sealed workspace");
            std::fs::write(workspace.home().join(".codewhale/.onboarded"), "")
                .expect("onboarded marker");
            let trust_dir = workspace.workspace().join(".deepseek");
            std::fs::create_dir_all(&trust_dir).expect("workspace trust dir");
            std::fs::write(trust_dir.join("trusted"), "").expect("workspace trust marker");
            let mut tui = Harness::builder(Harness::codewhale_binary())
                .cwd(workspace.workspace())
                .clear_env()
                .seal_home(workspace.home())
                .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
                .env("CODEWHALE_NO_UPDATE_CHECK", "1")
                .env("NO_ANIMATIONS", "1")
                .args([
                    "--workspace",
                    workspace.workspace().to_str().expect("workspace UTF-8"),
                    "--no-project-config",
                    "--fresh",
                ])
                .size(rows, cols)
                .spawn()
                .expect("start offline TUI");

            wait_or_panic(&mut tui, "New session", STARTUP_WAIT, "launch card");
            super::launch_card_pty::explore_offline_from_composer(&mut tui);

            tui.send(keys::key::text("late queue draft"))
                .expect("raw prompt bytes");
            std::thread::sleep(Duration::from_millis(delay_ms));
            tui.send(keys::key::enter()).expect("late submit");
            wait_or_panic(
                &mut tui,
                "Queued #1",
                STARTUP_WAIT,
                &format!("offline queue receipt ({cols}x{rows}, {delay_ms}ms submit)"),
            );

            tui.send(keys::key::ctrl('u')).expect("clear queued draft");
            tui.send(keys::key::text("input is still live"))
                .expect("type after queued submit");
            wait_or_panic(
                &mut tui,
                "input is still live",
                SETTLE_WAIT,
                "composer liveness",
            );
            tui.send(keys::key::ctrl('u'))
                .expect("clear liveness probe");
            tui.wait_for(|frame| !frame.contains("input is still live"), SETTLE_WAIT)
                .expect("Ctrl+U still clears the composer");
            tui.send(keys::key::text("/queue drop 1"))
                .expect("type queue command");
            // Keep the heuristic armed, but let this raw command's burst
            // settle before Enter so it is not a pasted newline.
            tui.wait_for_idle(Duration::from_millis(300), SETTLE_WAIT)
                .expect("queue command settles");
            tui.send(keys::key::enter()).expect("execute queue command");
            wait_or_panic(
                &mut tui,
                "Dropped queued message",
                SETTLE_WAIT,
                "command liveness",
            );
            assert!(
                !tui.frame().contains("engine session id diverged"),
                "{cols}x{rows}, {delay_ms}ms submit: {}",
                tui.diagnostics()
            );
            tui.shutdown();
        }
    }
}

#[test]
fn inline_start_never_takes_the_alternate_screen_and_screen_commands_switch_it() {
    let workspace = make_sealed_workspace().expect("sealed workspace");
    std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").expect("onboarded marker");
    let trust_dir = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust_dir).expect("workspace trust dir");
    std::fs::write(trust_dir.join("trusted"), "").expect("workspace trust marker");

    // The existing knob is the startup switch: `never` now means inline.
    for relative in [".codewhale/config.toml", ".deepseek/config.toml"] {
        let path = workspace.home().join(relative);
        let mut config = std::fs::read_to_string(&path).unwrap_or_default();
        config.push_str("\n[tui]\nalternate_screen = \"never\"\n");
        std::fs::write(&path, config).expect("seed inline screen mode");
    }

    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("NO_ANIMATIONS", "1")
        .env("RUST_LOG", "warn")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--fresh",
        ])
        .size(ROWS, COLS)
        .spawn()
        .expect("start distributed TUI binary");

    enter_live_shell(&mut tui);

    // Claim 1: the session came up without ever taking the alternate screen.
    tui.pump();
    assert_ne!(
        tui.terminal_modes().state(mode::ALT_SCREEN),
        Some(true),
        "inline startup must not enable DEC 1049\n{}",
        tui.diagnostics()
    );
    assert!(
        tui.frame().contains(LIVE_SHELL_SENTINEL),
        "inline shell painted no info line\n{}",
        tui.diagnostics()
    );

    // Claim 2: `/fullscreen` takes the alternate screen in-process. In
    // Explore Offline the first prompt is parked by the offline queue
    // ("Queued #1 … Enter send now"), and while a queued draft is held the
    // composer answers to the queue — a follow-up command's Enter would
    // send the draft instead of executing the command. Drop the queue
    // first, exactly the way the footer tells a human to.
    if tui.frame().contains("Queued #1") {
        tui.send(keys::key::text("/queue drop 1"))
            .expect("type /queue drop 1");
        tui.send(keys::key::enter()).expect("submit /queue drop 1");
        wait_or_panic(
            &mut tui,
            "Dropped queued message",
            Duration::from_secs(20),
            "queue drop receipt",
        );
    }
    tui.send(keys::key::ctrl('u')).expect("clear seeded input");
    tui.send(keys::key::text("/fullscreen"))
        .expect("type /fullscreen");
    tui.send(keys::key::enter()).expect("submit /fullscreen");
    wait_for_alt_screen(&mut tui, true, "/fullscreen");
    wait_or_panic(
        &mut tui,
        LIVE_SHELL_SENTINEL,
        SETTLE_WAIT,
        "fullscreen repaint",
    );

    // …and `/inline` gives the terminal back.
    tui.send(keys::key::text("/inline")).expect("type /inline");
    tui.send(keys::key::enter()).expect("submit /inline");
    wait_for_alt_screen(&mut tui, false, "/inline");
    wait_or_panic(&mut tui, LIVE_SHELL_SENTINEL, SETTLE_WAIT, "inline repaint");

    // Claim 3: the inline viewport follows the terminal size. Stock ratatui
    // keeps an inline viewport at the rows it was built with, so without the
    // refit a taller window would leave the new bottom rows blank.
    tui.resize(ROWS + 8, COLS).expect("grow the terminal");
    wait_for_bottom_rows_painted(&mut tui, "grow to 32 rows");
    tui.resize(ROWS, COLS).expect("shrink the terminal back");
    wait_for_bottom_rows_painted(&mut tui, "shrink back to 24 rows");
    assert_ne!(
        tui.terminal_modes().state(mode::ALT_SCREEN),
        Some(true),
        "resizing inline must not take the alternate screen\n{}",
        tui.diagnostics()
    );

    tui.shutdown();
}

/// The live shell paints its composer at the bottom of the viewport, so a
/// viewport that fits the terminal has text within its last rows.
fn wait_for_bottom_rows_painted(tui: &mut Harness, label: &str) {
    let painted = tui.wait_for(
        |frame| {
            let rows = frame.rows();
            (rows.saturating_sub(4)..rows).any(|y| !frame.row(y).trim().is_empty())
        },
        SETTLE_WAIT,
    );
    if painted.is_err() {
        panic!(
            "{label}: nothing painted in the bottom rows after resize\n{}",
            tui.diagnostics()
        );
    }
}

/// Land on the composer-first launch card with no
/// provider, no credentials and no network.
fn enter_live_shell(tui: &mut Harness) {
    wait_or_panic(tui, "New session", STARTUP_WAIT, "launch card");
    super::launch_card_pty::explore_offline_from_composer(tui);
    // Typing goes straight to the composer; Enter sends the first message
    // and the session begins (the card dissolved on the first keystroke).
    // type_line, not send+enter: a zero-gap PTY write is paste-classified
    // and the immediate Enter would be absorbed as a pasted newline.
    tui.type_line("start the session")
        .expect("type and send the first prompt");
    if tui
        .wait_for(|frame| !frame.text().contains('\u{2442}'), STARTUP_WAIT)
        .is_err()
    {
        panic!(
            "the first prompt did not enter the live shell\n{}",
            tui.diagnostics()
        );
    }
    tui.wait_for_idle(Duration::from_millis(300), SETTLE_WAIT)
        .expect("session shell settles");
}

fn wait_for_alt_screen(tui: &mut Harness, expected: bool, label: &str) {
    let deadline = std::time::Instant::now() + qa_harness::harness::ci_scaled(STARTUP_WAIT);
    loop {
        tui.pump();
        if tui.terminal_modes().state(mode::ALT_SCREEN) == Some(expected) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "{label}: alternate screen never became {expected}\n{}",
                tui.diagnostics()
            );
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn wait_or_panic(tui: &mut Harness, needle: &str, timeout: Duration, label: &str) {
    if tui.wait_for_text(needle, timeout).is_err() {
        panic!("{label}: {needle:?} not visible\n{}", tui.diagnostics());
    }
}
