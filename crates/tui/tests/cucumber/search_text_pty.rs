//! Search owns printable keys from the first character, through the real
//! terminal decoder and modal host. No provider or saved user config is used.

use std::time::Duration;

use super::qa_harness::{
    harness::{Harness, make_sealed_workspace},
    keys,
};

#[test]
fn search_text_stays_in_modal_and_out_of_composer() {
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_TELEMETRY", "0")
        .env("NO_ANIMATIONS", "1")
        .args([
            "--workspace",
            workspace.workspace().to_str().unwrap(),
            "--no-project-config",
            "--fresh",
        ])
        .size(24, 80)
        .spawn()
        .expect("start TUI");
    let timeout = Duration::from_secs(10);
    tui.wait_for_composer(timeout).unwrap();
    // Enter the live shell with a local command, without making a model call.
    tui.paste("/help").unwrap();
    tui.wait_for_text("❯ /help", timeout).unwrap();
    tui.send(keys::key::enter()).unwrap();
    tui.wait_for_text("Help —", timeout).unwrap();
    tui.send(keys::key::esc()).unwrap();
    tui.wait_for_text("Type a message", timeout).unwrap();
    let draft = "draft_no_leak";
    tui.paste(draft).unwrap();
    tui.wait_for_text(draft, timeout).unwrap();

    for (open, title, prefix, query, escapes) in [
        (keys::key::f1(), "Help —", "Filter: ", "queue", 1),
        (keys::key::f1(), "Help —", "Filter: ", "Queue", 1),
        (keys::key::f2(), "Settings", "Search: ", "quiet", 2),
        (keys::key::f2(), "Settings", "Search: ", "effort", 2),
        (keys::key::f2(), "Settings", "Search: ", "json", 2),
        (keys::key::f2(), "Settings", "Search: ", "key", 2),
        (keys::key::f2(), "Settings", "Search: ", " 队列é", 2),
        (keys::key::ctrl('k'), "Command —", "Filter: ", "json", 1),
        (keys::key::ctrl('k'), "Command —", "Filter: ", "key", 1),
    ] {
        tui.send(open).unwrap();
        tui.wait_for_text(title, timeout).unwrap();
        let mut typed = String::new();
        for ch in query.chars() {
            tui.send(ch.to_string()).unwrap();
            typed.push(ch);
            let shown = if title == "Settings" {
                typed.trim_start()
            } else {
                typed.as_str()
            };
            tui.wait_for_text(&format!("{prefix}{shown}"), timeout)
                .unwrap_or_else(|error| panic!("{title} query {typed:?}: {error}"));
        }
        for _ in 0..escapes {
            tui.send(keys::key::esc()).unwrap();
            tui.wait_for_idle(Duration::from_millis(150), timeout)
                .unwrap();
        }
        tui.wait_for_text(draft, timeout).unwrap();
        let frame = tui.frame();
        let composer = (0..frame.rows())
            .map(|row| frame.row(row))
            .find(|row| row.contains('❯'))
            .expect("composer is visible after closing search");
        let content = composer
            .split_once('❯')
            .unwrap()
            .1
            .split("[↵]")
            .next()
            .unwrap()
            .trim_end();
        let content = content.strip_suffix('│').unwrap_or(content).trim();
        assert_eq!(content, draft, "{title} leaked query {query:?}");
    }
    tui.shutdown();
}
