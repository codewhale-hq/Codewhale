//! Real-PTY regression for the active composer's painted `[↵]` submit
//! affordance (#5773, TUI-UX-01 active-work half).
//!
//! The session is driven through the real onboarding flow into deterministic
//! offline-explore mode, so bare submit resolves to Queue: no provider, no
//! credentials, no network. The user types a draft and clicks the painted
//! send cell with SGR mouse down/up. The click must reach the same queued
//! keyboard-submit path as Enter — proven by the offline queue receipt toast
//! and the durable `Queued #1:` pending-input preview, which renders directly
//! from `app.queued_messages` — rather than leaving the draft untouched as a
//! no-op click.

#![cfg(all(unix, feature = "long-running-tests"))]

use std::time::{Duration, Instant};

use super::qa_harness;
use qa_harness::Frame;
use qa_harness::harness::{Harness, make_sealed_workspace};
use qa_harness::keys;
use qa_harness::modes::mode;

/// (rows, cols): the release acceptance matrix for compact through wide terminals.
const SIZES: [(u16, u16); 5] = [(12, 40), (16, 60), (24, 80), (32, 100), (40, 140)];

const STARTUP_WAIT: Duration = Duration::from_secs(15);
const SETTLE_WAIT: Duration = Duration::from_secs(5);

/// Offline-queue receipt toast (MessageId::ToastQueuedOffline, en).
const QUEUE_RECEIPT_TOAST: &str = "Saved for later. Connect a provider to send.";
/// Compact-width prefix of that toast: the footer truncates the tail when the
/// row cannot hold the full sentence next to the posture chips (deterministic
/// layout boundary, observed painted as "Saved for later." at 40 and 60
/// cols).
const QUEUE_RECEIPT_TOAST_COMPACT: &str = "Saved for later.";

/// The receipt needle this width can actually paint: full toast where the
/// footer row fits it, its unwrapped prefix on compact layouts.
fn queue_receipt_needle(cols: u16) -> &'static str {
    if cols >= 100 {
        QUEUE_RECEIPT_TOAST
    } else {
        QUEUE_RECEIPT_TOAST_COMPACT
    }
}

#[test]
fn active_composer_pointer_submit_queues_without_provider() {
    for (rows, cols) in SIZES {
        run_pointer_submit_case(rows, cols);
    }
}

fn run_pointer_submit_case(rows: u16, cols: u16) {
    let size = format!("{cols}x{rows}");
    let workspace = make_sealed_workspace().expect("sealed workspace");
    // Seed only the two first-run markers: prior onboarding completed
    // ($HOME/.codewhale/.onboarded) and this workspace is trusted
    // (<workspace>/.deepseek/trusted). No provider, key, or route is seeded.
    std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").expect("onboarded marker");
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
        .env("RUST_LOG", "warn")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--fresh",
            "--mouse-capture",
        ])
        .size(rows, cols)
        .spawn()
        .expect("start distributed TUI binary");

    // PTY reads can split a redraw: the launch header arrives before the
    // composer, with onboarding rows still on screen (Buildkite #1861/#1867).
    // Wait for the input surface as well as the header before asserting it.
    tui.wait_for(
        |frame| {
            let text = frame.text();
            text.contains("New session") && text.contains('❯') && !text.contains("You're ready.")
        },
        STARTUP_WAIT,
    )
    .unwrap_or_else(|error| panic!("{size}: show the launch card and composer: {error}"));
    tui.pump();
    assert_startup_contract(tui.frame(), rows, cols, &size);
    super::launch_card_pty::explore_offline_from_composer(&mut tui);
    tui.wait_for_idle(Duration::from_millis(300), SETTLE_WAIT)
        .expect("session shell settles");
    // Prove the steady shell before clearing the onboarding seed. The clear
    // action intentionally emits a transient `Ctrl+Z restores` receipt, and
    // live receipts outrank the footer's key hint while present.
    tui.pump();
    assert_live_shell_contract(tui.frame(), cols, &size);

    // Clear the onboarding seed so the unique pointer-test draft starts from
    // an empty composer.
    tui.send(keys::key::ctrl('u')).expect("clear seeded input");
    tui.wait_for_idle(Duration::from_millis(200), SETTLE_WAIT)
        .expect("composer cleared");

    // Mouse capture must be live before the click, or the SGR bytes would
    // test nothing. Readiness comes from the control stream, not the frame.
    wait_for_mouse_capture(&mut tui, &size);

    // Baseline the queue depth while the composer is empty: the pending
    // preview row paints only then, and the later proof needs the exact
    // pre-click depth.
    let queued_before = queued_count(&normalized_text(tui.frame()));

    // Type a unique draft and let the composer settle before recording
    // coordinates, so no late layout shift moves the cells under us.
    let draft = format!("qa pointer draft {size}");
    tui.send(keys::key::text(&draft)).expect("type draft");
    wait_or_panic(
        &mut tui,
        &draft,
        SETTLE_WAIT,
        &format!("{size}: draft echo"),
    );
    tui.wait_for_idle(Duration::from_millis(200), SETTLE_WAIT)
        .expect("composer settles with draft");

    // The paste-burst window treats input arriving within ~150ms of the
    // keystrokes as paste; stay clearly outside it before clicking.
    std::thread::sleep(Duration::from_millis(200));

    tui.pump();
    let (send_row, send_col) = tui.frame().find_text("[↵]").unwrap_or_else(|| {
        panic!(
            "{size}: [↵] submit affordance not painted\n{}",
            tui.diagnostics()
        )
    });
    // Baseline: no queue receipt toast exists before the click, so the
    // receipt below can only be produced by this gesture. (The onboarding
    // seed may already sit in the offline queue; the unique draft cannot.)
    let receipt = queue_receipt_needle(cols);
    tui.pump();
    let before = normalized_text(tui.frame());
    assert!(
        !before.contains(receipt),
        "{size}: queue receipt already visible before any submit\n{}",
        tui.diagnostics()
    );

    // Click the middle cell of the three-cell `[↵]` affordance: SGR down,
    // settle, SGR up — the sequence a real terminal sends for one click.
    tui.send(keys::mouse::down(send_row, send_col + 1))
        .expect("SGR mouse down on [↵]");
    tui.wait_for_idle(Duration::from_millis(150), Duration::from_secs(2))
        .expect("down settles");
    tui.send(keys::mouse::up(send_row, send_col + 1))
        .expect("SGR mouse up on [↵]");

    // Distinguishing assertion: a real submit — the pointer click or keyboard
    // Enter alike — consumes the draft into the deterministic offline queue
    // and paints the queue receipt toast. A no-op click paints no receipt
    // and never grows the queue. The receipt toast is transient, so either
    // signal (toast seen, or the queue count grew) proves the dispatch.
    let expected = queued_before.map(|n| n + 1);
    let deadline = Instant::now() + qa_harness::harness::ci_scaled(SETTLE_WAIT);
    let retry_at = Instant::now() + qa_harness::harness::ci_scaled(SETTLE_WAIT / 2);
    let mut retried = false;
    println!(
        "POINTER DEBUG {size}: queued_before={queued_before:?} expected={expected:?} receipt={receipt:?}"
    );
    loop {
        tui.pump();
        let text = normalized_text(tui.frame());
        let seen = queued_count(&text);
        // The click proves itself by either the receipt toast or the queue
        // preview appearing (baseline absent -> something queued) or growing
        // by exactly this draft (baseline visible -> baseline + 1).
        let grew = match (queued_before, seen) {
            (Some(before), Some(now)) => now == before + 1,
            (None, Some(_)) => true,
            _ => false,
        };
        if text.contains(receipt) || grew {
            println!(
                "POINTER DEBUG {size}: broke with seen={seen:?} expected={expected:?} receipt_seen={}",
                text.contains(receipt)
            );
            break;
        }
        // One bounded retry at the half-way point: re-find the affordance
        // (a redraw may have shifted cells between find and click under
        // runner load) and click it again. Keep polling after it — the app
        // may take a beat to process the second gesture.
        if !retried && Instant::now() >= retry_at {
            retried = true;
            let (retry_row, retry_col) = tui.frame().find_text("[↵]").unwrap_or_else(|| {
                panic!(
                    "{size}: [↵] submit affordance not painted on retry\n{}",
                    tui.diagnostics()
                )
            });
            tui.send(keys::mouse::down(retry_row, retry_col + 1))
                .expect("SGR mouse down on [↵] retry");
            std::thread::sleep(Duration::from_millis(150));
            tui.send(keys::mouse::up(retry_row, retry_col + 1))
                .expect("SGR mouse up on [↵] retry");
        }
        if Instant::now() >= deadline {
            panic!(
                "{size}: click on [↵] at ({send_row},{}) produced no queue receipt \
                 {receipt:?} and no queue growth — pointer submit did not reach the \
                 keyboard-submit dispatch path (seen={seen:?})\n{}",
                send_col + 1,
                tui.diagnostics()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Durable queue proof at every size: the pending-input preview renders
    // straight from `app.queued_messages` — the real queue state, no
    // slash-menu navigation needed. The queue must still report at least
    // the entry this gesture added (the 40-column floor truncates the
    // preview before the draft text, so the entry count, not the text, is
    // the signal at the floor).
    tui.pump();
    let after = normalized_text(tui.frame());
    let grew = match (queued_count(&after), queued_before) {
        (Some(after_n), Some(before_n)) => after_n == before_n + 1,
        _ => after.contains("Queued "),
    };
    assert!(
        grew,
        "{size}: the queue did not grow by exactly the pointer-submitted draft\n{}",
        tui.diagnostics()
    );

    let modes = tui.terminal_modes();
    assert_eq!(
        modes.state(mode::MOUSE_SGR),
        Some(true),
        "{size}: SGR mouse encoding must be enabled for the click to be meaningful\n{}",
        modes.debug_dump()
    );
    assert!(
        modes.was_ever_enabled(mode::MOUSE_BUTTON),
        "{size}: mouse button tracking was never enabled\n{}",
        modes.debug_dump()
    );

    let _ = tui.shutdown();
}

/// Frame text with rows joined on single spaces, so assertions survive a
/// narrow-terminal wrap of the needle across two painted rows.
fn normalized_text(frame: &Frame) -> String {
    frame
        .text()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn assert_startup_contract(frame: &Frame, rows: u16, cols: u16, size: &str) {
    let text = frame.text();
    // The launch card's own truth: the wordmark + version, the prominent
    // new-session entry, and the focused composer. The posture bar and
    // metrics line appear only once a session exists, so `context` is NOT
    // asserted here any more (SHELL-DESIGN-20260901 Round 5).
    for needle in ["codewhale", "❯"] {
        assert!(
            text.contains(needle),
            "{size}: startup misses {needle:?}\n{}",
            frame.debug_dump()
        );
    }
    // Empty workspaces keep the invitation without an empty history section.
    assert!(
        text.contains("New session"),
        "{size}: startup misses invitation\n{}",
        frame.debug_dump()
    );
    assert!(
        !text.contains("No recent sessions"),
        "{size}: empty history adds noise\n{}",
        frame.debug_dump()
    );
    for retired_mark_row in ["▄▄▄▄██▌", "▜████▀▘"] {
        assert!(
            !text.contains(retired_mark_row),
            "{size}: startup still paints the retired approximate mark\n{}",
            frame.debug_dump()
        );
    }
    assert_eq!(frame.rows(), rows, "{size}: PTY row count drift");
    assert_eq!(frame.cols(), cols, "{size}: PTY column count drift");
    assert!(
        frame.max_row_width() <= usize::from(cols),
        "{size}: startup row overflow\n{}",
        frame.debug_dump()
    );
}

fn assert_live_shell_contract(frame: &Frame, cols: u16, size: &str) {
    let text = frame.text();
    // The bottom metrics row owns the model; repository state belongs to
    // the launch header and git view. This sealed offline session carries no
    // key, so the route chip says the model is not connected instead of
    // naming a default route that cannot answer (experience mark 8, U3). That
    // chip must remain visible even at 40 columns.
    let metrics = frame.row(frame.rows().saturating_sub(1));
    assert!(
        metrics.contains("model not connected"),
        "{size}: live shell misses the model in the metrics line\n{}",
        frame.debug_dump()
    );
    // Compact metrics omit the passive help hint; F1 still opens Help.
    assert!(
        !text.contains("RUNS") || cols < 100,
        "{size}: passive duplicate Tideline rail is still visible\n{}",
        frame.debug_dump()
    );
    assert!(
        frame.max_row_width() <= usize::from(cols),
        "{size}: live-shell row overflow\n{}",
        frame.debug_dump()
    );
}

/// The queue depth the frame's pending-input preview reports
/// (`Queued N · next: …`), when one is painted.
fn queued_count(text: &str) -> Option<u32> {
    let idx = text.find("Queued ")?;
    let rest = &text[idx + "Queued ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn wait_or_panic(tui: &mut Harness, needle: &str, timeout: Duration, label: &str) {
    if tui.wait_for_text(needle, timeout).is_err() {
        panic!("{label}: {needle:?} not visible\n{}", tui.diagnostics());
    }
}

fn wait_for_mouse_capture(tui: &mut Harness, size: &str) {
    let deadline = Instant::now() + qa_harness::harness::ci_scaled(STARTUP_WAIT);
    loop {
        tui.pump();
        let modes = tui.terminal_modes();
        if modes.state(mode::MOUSE_SGR) == Some(true)
            && modes.state(mode::MOUSE_BUTTON) == Some(true)
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "{size}: mouse capture never enabled in the control stream\n{}",
                tui.diagnostics()
            );
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}
