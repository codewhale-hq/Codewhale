//! Launch actions must work through the real input loop, including Enter
//! after a mouse click. All state is sealed; provider fixtures use loopback.

use std::time::Duration;

use super::qa_harness;
use qa_harness::harness::{Harness, SealedWorkspace, make_sealed_workspace};
use qa_harness::keys;

const WAIT: Duration = Duration::from_secs(15);
const SIZES: [(u16, u16); 5] = [(12, 40), (16, 60), (24, 80), (32, 100), (40, 140)];
const TITLE: &str = "Recent proof";
const SAVED_TEXT: &str = "Restored conversation proof";

fn start(rows: u16, cols: u16, with_mcp: bool) -> (SealedWorkspace, Harness) {
    start_titled(rows, cols, with_mcp, TITLE)
}

fn start_titled(rows: u16, cols: u16, with_mcp: bool, title: &str) -> (SealedWorkspace, Harness) {
    start_with_titles(rows, cols, with_mcp, &[title])
}

fn start_with_titles(
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &[&str],
) -> (SealedWorkspace, Harness) {
    start_with_theme(rows, cols, with_mcp, titles, None)
}

fn start_with_theme(
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &[&str],
    theme: Option<&str>,
) -> (SealedWorkspace, Harness) {
    start_with_options(rows, cols, with_mcp, titles, theme, false, false)
}

fn start_with_options(
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &[&str],
    theme: Option<&str>,
    animated: bool,
    no_color: bool,
) -> (SealedWorkspace, Harness) {
    start_in(
        make_sealed_workspace().unwrap(),
        Launch {
            rows,
            cols,
            with_mcp,
            titles,
            theme,
            animated,
            no_color,
            keep_composer: false,
        },
    )
}

#[derive(Default)]
struct Launch<'a> {
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &'a [&'a str],
    theme: Option<&'a str>,
    animated: bool,
    no_color: bool,
    /// Skip the post-launch Ctrl+U that empties the composer.
    keep_composer: bool,
}

/// Launch into `workspace`; the composer-first launch card is home.
fn start_in(workspace: SealedWorkspace, launch: Launch<'_>) -> (SealedWorkspace, Harness) {
    let Launch {
        rows,
        cols,
        with_mcp,
        titles,
        theme,
        animated,
        no_color,
        keep_composer,
    } = launch;
    let trust = workspace.workspace().join(".deepseek");
    let sessions = workspace.home().join(".codewhale/sessions");
    for directory in [&trust, &sessions] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let mut fixtures = vec![
        (workspace.home().join(".codewhale/.onboarded"), Vec::new()),
        (trust.join("trusted"), Vec::new()),
    ];
    if let Some(theme) = theme {
        fixtures.push((
            workspace.home().join(".codewhale/settings.toml"),
            format!("theme = {theme:?}\n").into_bytes(),
        ));
    }
    for (index, title) in titles.iter().enumerate() {
        let id = format!(
            "11111111-2222-4333-8444-{:012}",
            555555555555u64 + index as u64
        );
        let session = serde_json::json!({
            "schema_version": 1,
            "metadata": {
                "id": id,
                "title": title,
                "created_at": "2026-09-19T00:00:00Z",
                "updated_at": format!("2026-09-19T00:00:{:02}Z", 59usize.saturating_sub(index)),
                "message_count": 1,
                "total_tokens": 0,
                "model": "deepseek-flash",
                "model_provider": "deepseek",
                "workspace": workspace.workspace()
            },
            "messages": [{"role": "user", "content": [{"type": "text", "text": SAVED_TEXT}]}],
            "system_prompt": null
        });
        fixtures.push((
            sessions.join(format!("{id}.json")),
            serde_json::to_vec(&session).unwrap(),
        ));
    }
    if with_mcp {
        // A local failing server gives the summary a real row without any network.
        let mcp = serde_json::json!({"mcpServers": {"launch-proof": {
            "command": "/usr/bin/false", "required": true
        }}});
        fixtures.push((
            workspace.home().join(".codewhale/mcp.json"),
            serde_json::to_vec(&mcp).unwrap(),
        ));
    }
    for (path, contents) in fixtures {
        std::fs::write(path, contents).unwrap();
    }

    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("NO_ANIMATIONS", if animated { "0" } else { "1" })
        .env("COLORTERM", "truecolor")
        .env("NO_COLOR", if no_color { "1" } else { "" })
        .args([
            "--workspace",
            workspace.workspace().to_str().unwrap(),
            "--no-project-config",
            "--fresh",
            "--mouse-capture",
        ])
        .size(rows, cols)
        .spawn()
        .unwrap();
    wait(&mut tui, "New session");
    if animated || keep_composer {
        return (workspace, tui);
    }
    tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
    tui.send(keys::key::ctrl('u')).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    (workspace, tui)
}

pub(crate) fn explore_offline_from_composer(tui: &mut Harness) {
    wait(tui, "Type a message");
    tui.type_line("seed").expect("send a first message");
    wait(tui, "Choose your model provider");
    tui.send(keys::key::ctrl('o')).unwrap();
    wait(tui, "You're ready.");
    tui.send(keys::key::enter()).unwrap();
    tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
    tui.send(keys::key::ctrl('u')).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
}

fn wait(tui: &mut Harness, text: &str) {
    if let Err(error) = tui.wait_for(|frame| frame.contains(text), WAIT) {
        let transcript = tui.transcript();
        let tail = &transcript[transcript.len().saturating_sub(4096)..];
        panic!(
            "waiting for {text:?}: {error}\n{}\nPTY tail: {:?}",
            tui.diagnostics(),
            String::from_utf8_lossy(tail)
        );
    }
}

fn wait_for_plugins_screen(tui: &mut Harness) {
    if let Err(error) = tui.wait_for(
        |frame| frame.contains("Plugins") && frame.contains("Connectors"),
        WAIT,
    ) {
        panic!(
            "waiting for the Plugins screen: {error}\n{}",
            tui.diagnostics()
        );
    }
}

fn click_text(tui: &mut Harness, text: &str) {
    tui.pump();
    let (row, col) = tui
        .frame()
        .find_text(text)
        .unwrap_or_else(|| panic!("missing click target {text:?}\n{}", tui.diagnostics()));
    tui.send(keys::mouse::click(row, col)).unwrap();
}

#[test]
fn local_slash_navigation_does_not_create_rewindable_user_turns() {
    let (_workspace, mut tui) = start_with_titles(24, 80, false, &[]);
    // The first command leaves home; the others use the active-session path.
    for (command, title) in [("/settings", "Settings"), ("/skills", ""), ("/mcp", "")] {
        tui.type_line(command).unwrap();
        if title.is_empty() {
            wait_for_plugins_screen(&mut tui);
        } else {
            wait(&mut tui, title);
        }
        tui.send(keys::key::esc()).unwrap();
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        assert!(
            !tui.frame()
                .text()
                .lines()
                .take(18)
                .any(|line| line.contains(command)),
            "navigation leaked into transcript above the composer: {}",
            tui.diagnostics()
        );
    }
    tui.send(keys::key::esc()).unwrap();
    tui.send(keys::key::esc()).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    assert!(
        !tui.frame().contains("Backtrack preview"),
        "view navigation became a rewindable turn: {}",
        tui.diagnostics()
    );
    tui.shutdown();
}

#[test]
fn raw_slash_input_keeps_a_steady_submit_cue_and_runs_on_enter_without_another_key() {
    // #6397: the `[↵]` chip follows the draft, not the paste-burst window, so
    // it is already lit while a raw (non-bracketed) burst's Enter-suppression
    // window is still open. It is therefore not a signal that Enter will
    // submit; wait out the window (120ms) with a quiet PTY before pressing
    // Enter, which must then run the command with no other key.
    let (_workspace, mut tui) = start_with_titles(24, 80, false, &[]);
    tui.send("/mcp").unwrap();
    wait(&mut tui, "enter:run");
    wait(&mut tui, "[↵]");
    tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
    assert!(
        tui.frame().contains("[↵]") && !tui.frame().contains("[·]"),
        "submit cue did not stay steady: {}",
        tui.diagnostics()
    );
    tui.send(keys::key::enter()).unwrap();
    wait_for_plugins_screen(&mut tui);
    tui.shutdown();
}

#[test]
fn launch_recent_click_then_enter_resumes_without_another_mouse_event() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        assert!(!tui.frame().contains(TITLE));
        capture(&mut tui, "home");
        click_text(&mut tui, "Recent");
        wait(&mut tui, TITLE);
        capture(&mut tui, "recent-expanded");
        click_text(&mut tui, TITLE);
        wait(&mut tui, "Resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        capture(&mut tui, "confirm");
        tui.send(keys::key::enter()).unwrap();
        // No pointer motion follows Enter: the accepted action must run now.
        wait(&mut tui, SAVED_TEXT);
        if cols >= 80 {
            wait(&mut tui, "Resumed:");
        }
        assert!(
            !tui.frame().contains("Session loaded from"),
            "resume should not add a technical path receipt to the conversation"
        );
        capture(&mut tui, "conversation");
        tui.shutdown();
    }
}

#[test]
fn launch_mcp_summary_opens_manager_by_click_and_keyboard() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, true);
        wait(&mut tui, "Connectors");
        assert!(!tui.frame().contains("launch-proof"));
        capture(&mut tui, "home-mcp");
        click_text(&mut tui, "Connectors");
        wait_for_plugins_screen(&mut tui);
        wait(&mut tui, "launch-proof");
        capture(&mut tui, "mcp");
        tui.send(keys::key::esc()).unwrap();
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains("launch-proof"));
        // New session, recent sessions, then MCP.
        for _ in 0..3 {
            tui.send(keys::key::down()).unwrap();
        }
        tui.send(keys::key::enter()).unwrap();
        wait_for_plugins_screen(&mut tui);
        wait(&mut tui, "launch-proof");
        tui.shutdown();
    }
}

#[test]
fn launch_resume_buttons_support_mouse_cancel_and_keyboard_choice() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        tui.send(keys::key::up()).unwrap();
        wait(&mut tui, TITLE);
        click_text(&mut tui, TITLE);
        wait(&mut tui, "cancel");
        click_text(&mut tui, "cancel");
        tui.wait_for(
            |frame| frame.find_text(TITLE).is_some() && !frame.contains("resume  Enter"),
            WAIT,
        )
        .unwrap();
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains(SAVED_TEXT));
        click_text(&mut tui, TITLE);
        wait(&mut tui, "cancel");
        tui.send(keys::key::tab()).unwrap();
        tui.send(keys::key::enter()).unwrap();
        tui.wait_for(
            |frame| frame.find_text(TITLE).is_some() && !frame.contains("resume  Enter"),
            WAIT,
        )
        .unwrap();
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains(SAVED_TEXT));
        click_text(&mut tui, TITLE);
        wait(&mut tui, "resume  Enter");
        click_text(&mut tui, "resume  Enter");
        wait(&mut tui, SAVED_TEXT);
        tui.shutdown();
    }
}

/// Optional review evidence from the real PTY, keeping cell colors rather
/// than relying on symbol-only goldens. The viewer supplies terminal fonts.
pub(super) fn capture(tui: &mut Harness, name: &str) {
    let Some(directory) = std::env::var_os("QA_LAUNCH_CAPTURE_DIR") else {
        return;
    };
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    let frame = tui.frame();
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}-{}x{}.json", frame.cols(), frame.rows()));
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&frame.capture_cells()).unwrap(),
    )
    .unwrap();
}

#[test]
fn plugin_command_opens_plugins_screen_with_kind_filters() {
    for (rows, cols) in [(24, 80), (32, 120)] {
        let (_workspace, mut tui) = start(rows, cols, true);
        tui.paste("/plugin").unwrap();
        tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait_for_plugins_screen(&mut tui);
        tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
        let frame = tui.frame();
        println!("--- /plugin at {cols}x{rows} ---\n{}", frame.text());
        for label in ["Hooks", "Installed", "Skills", "Connectors", "Command"] {
            assert!(
                frame.contains(label),
                "missing filter {label:?} at {cols}x{rows}\n{}",
                frame.text()
            );
        }
        assert!(
            !frame.contains("Extensions"),
            "the screen still says Extensions at {cols}x{rows}\n{}",
            frame.text()
        );
        tui.shutdown();
    }
}

#[test]
#[ignore = "opt-in visual evidence; writes only with QA_LAUNCH_CAPTURE_DIR"]
fn workbench_settings_visual_evidence() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    for (command, title, name) in [
        ("/model", "route ·", "models"),
        ("/provider", "Provider", "providers"),
        ("/fleet", "Coordinator", "fleet"),
        ("/plugin", "Installed", "plugins"),
        ("/config", "Settings", "settings"),
        ("/statusline", "Status", "statusline"),
    ] {
        for (rows, cols) in SIZES {
            let (_workspace, mut tui) = start(rows, cols, true);
            tui.paste(command).unwrap();
            tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
            tui.send(keys::key::enter()).unwrap();
            wait(&mut tui, title);
            capture(&mut tui, name);
            if name == "providers" {
                tui.send(keys::key::alt('v')).unwrap();
                wait(&mut tui, "DeepSeek · Open details");
                capture(&mut tui, "provider-details");
                tui.send(keys::key::esc()).unwrap();
                wait(&mut tui, "Provider");
            }
            tui.shutdown();
        }
    }
}

#[test]
#[ignore = "opt-in populated launch evidence; fixture sessions, no provider calls"]
fn workbench_populated_home_visual_evidence() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start_with_titles(
            rows,
            cols,
            true,
            &[
                "Polish the release notes",
                "Investigate a provider timeout",
                "Review the plugin setup flow",
            ],
        );
        capture(&mut tui, "home-populated");
        tui.send(keys::key::down()).unwrap();
        tui.send(keys::key::down()).unwrap();
        capture(&mut tui, "home-populated-selected");
        tui.shutdown();
    }
}

#[test]
fn launch_long_resume_title_preserves_warning_and_truthful_enter_hint() {
    let title = "Investigate provider timeouts and connection failures across multiple accounts, preserve the original credentials, and verify every saved session can still be restored after the upgrade";
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start_titled(rows, cols, false, title);
        tui.send(keys::key::up()).unwrap();
        wait(&mut tui, "Investigate provider");
        click_text(&mut tui, "Investigate provider");
        wait(&mut tui, "resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        capture(&mut tui, "confirm-long");
        let text = tui
            .frame()
            .text()
            .chars()
            .map(|ch| {
                if ('\u{2500}'..='\u{257f}').contains(&ch) {
                    ' '
                } else {
                    ch
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("This replaces the current context with that session's history."),
            "{text}"
        );
        tui.send(keys::key::tab()).unwrap();
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        let text = tui.frame().text();
        assert!(text.contains("cancel  Enter"), "{text}");
        assert!(!text.contains("resume  Enter"), "{text}");
        capture(&mut tui, "confirm-cancel");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains(SAVED_TEXT));
        tui.shutdown();
    }
}

#[test]
#[ignore = "opt-in all-theme evidence; fixture sessions, no provider calls"]
fn workbench_every_theme_visual_evidence() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    for theme in codewhale_palette::SELECTABLE_THEMES {
        let (_workspace, mut tui) = start_with_theme(24, 80, true, &[TITLE], Some(theme.name()));
        capture(&mut tui, &format!("theme-{}-home", theme.name()));
        tui.paste("/statusline").unwrap();
        tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Status");
        capture(&mut tui, &format!("theme-{}-statusline", theme.name()));
        tui.shutdown();
    }
}

#[test]
#[ignore = "opt-in real launch animation capture; fixture state, no provider calls"]
fn workbench_whale_reveal_visual_evidence() {
    let directory = std::path::PathBuf::from(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    let (_workspace, mut tui) =
        start_with_options(32, 100, false, &[TITLE], Some("shoreline"), true, false);
    let start = std::time::Instant::now();
    for index in 0..16 {
        let frame = tui.frame();
        assert!(
            frame.text().contains("New session"),
            "controls must remain usable during reveal"
        );
        std::fs::write(
            directory.join(format!("reveal-{index:02}-100x32.json")),
            serde_json::to_vec_pretty(&frame.capture_cells()).unwrap(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("Captured launch reveal over {:?}", start.elapsed());
    tui.shutdown();
}

/// Record temporal evidence from the real terminal, including its idle settle.
#[test]
#[ignore = "opt-in Underwater motion capture; isolated fixture, no provider calls"]
fn underwater_motion_visual_evidence() {
    use std::io::Write;
    let directory = std::path::PathBuf::from(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    for (rows, cols) in [(24, 80), (36, 120)] {
        let (_workspace, mut tui) =
            start_with_options(rows, cols, false, &[TITLE], Some("underwater"), true, false);
        // Home intentionally gives its brief whale reveal the stage. Sea life
        // lives in the conversation field, so enter a fresh offline session.
        tui.send(keys::key::ctrl('u')).unwrap();
        click_text(&mut tui, "New session");
        wait(&mut tui, "What do you want to accomplish?");
        let file =
            std::fs::File::create(directory.join(format!("ocean-{cols}x{rows}.jsonl.gz"))).unwrap();
        let mut output = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let start = std::time::Instant::now();
        for index in 0..360u64 {
            let frame = tui.frame();
            assert!(
                frame.contains("Type a message")
                    && frame.contains("What do you want to accomplish?"),
                "motion cannot displace the conversation or composer"
            );
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({
                    "elapsed_ms": start.elapsed().as_millis(),
                    "frame": frame.capture_cells()
                }),
            )
            .unwrap();
            output.write_all(b"\n").unwrap();
            let target = Duration::from_millis((index + 1) * 33);
            if let Some(remaining) = target.checked_sub(start.elapsed()) {
                std::thread::sleep(remaining);
            }
        }
        output.finish().unwrap();
        tui.shutdown();
    }
}

/// Exercise the visible catalog controls and provider search through the
/// input decoder. This only browses fixture state; it never applies a route.
#[test]
fn settings_catalog_controls_and_provider_search_work_with_mouse_and_keyboard() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        tui.paste("/provider").unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Provider");
        click_text(&mut tui, "browse all");
        wait(&mut tui, "configured");
        tui.send("/Anthropic").unwrap();
        wait(&mut tui, "search: Anthropic");
        capture(&mut tui, "providers-search");
        tui.send(keys::key::esc()).unwrap();
        wait(&mut tui, "Provider");
        capture(&mut tui, "providers-catalog");
        tui.send(keys::key::esc()).unwrap();
        // Let the standalone Escape decode before starting a bracketed paste.
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.paste("/model").unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "route ·");
        click_text(&mut tui, "browse catalog");
        wait(&mut tui, "catalog");
        capture(&mut tui, "models-catalog");
        tui.send(keys::key::esc()).unwrap();
        tui.shutdown();
    }
}

#[test]
fn fleet_roles_open_the_shared_model_picker_and_escape_returns_to_the_same_role() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        tui.paste("/fleet").unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Coordinator");
        capture(&mut tui, "fleet-assignments");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Model · Coordinator");
        wait(&mut tui, "Current session");
        capture(&mut tui, "fleet-coordinator-model");
        // The roster footer ("saved teams") can stay visible behind the
        // picker at wide sizes, so it does not prove Esc landed. Wait for the
        // picker itself to close and the screen to settle before the next
        // key: a key sent inside the Esc disambiguation window is read as
        // Alt+key and the role never changes.
        close_picker(&mut tui, "Model · Coordinator");
        tui.send(keys::key::down()).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Model · manager");
        capture(&mut tui, "fleet-role-model");
        tui.send("search-proof").unwrap();
        wait(&mut tui, "search-proof");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.send(keys::key::esc()).unwrap();
        tui.wait_for(|frame| !frame.contains("search-proof"), WAIT)
            .unwrap();
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        close_picker(&mut tui, "Model · manager");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Model · manager");
        // Following Coordinator is a selectable local choice even without credentials.
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Personal");
        capture(&mut tui, "fleet-role-destination");
        tui.shutdown();
    }
}

/// Esc out of a Fleet model picker and wait until the roster is back and
/// quiet, so the next key is never folded into the Esc sequence.
fn close_picker(tui: &mut Harness, title: &str) {
    tui.send(keys::key::esc()).unwrap();
    if let Err(error) = tui.wait_for(|frame| !frame.contains(title), WAIT) {
        panic!(
            "waiting for {title:?} to close: {error}\n{}",
            tui.diagnostics()
        );
    }
    wait(tui, "saved teams");
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
}

#[test]
fn no_color_keeps_home_navigation_and_submit_cues_without_color() {
    let sgr = regex::Regex::new(r"\x1b\[([0-9;:]*)m").unwrap();
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) =
            start_with_options(rows, cols, false, &[TITLE], Some("shoreline"), false, true);
        wait(&mut tui, "[·]");
        tui.send(keys::key::up()).unwrap();
        wait(&mut tui, TITLE);
        tui.send(keys::key::up()).unwrap();
        capture(&mut tui, "no-color-selected");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, SAVED_TEXT);
        tui.send("monochrome draft").unwrap();
        wait(&mut tui, "[↵]");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        capture(&mut tui, "no-color-draft");

        for row in 0..rows {
            for col in 0..cols {
                assert_eq!(
                    tui.frame().colors_at(row, col),
                    Some((
                        qa_harness::frame::Color::Default,
                        qa_harness::frame::Color::Default
                    )),
                    "{cols}x{rows} cell ({row}, {col}) added a color"
                );
            }
        }
        // Inspect the whole emitted stream, not just its last rendered frame.
        let transcript = tui.transcript();
        let output = String::from_utf8_lossy(&transcript);
        for codes in sgr.captures_iter(&output) {
            for code in codes[1]
                .split([';', ':'])
                .filter_map(|code| code.parse::<u16>().ok())
            {
                assert!(
                    !matches!(code, 30..=38 | 40..=48 | 58 | 90..=97 | 100..=107),
                    "{cols}x{rows} emitted color SGR {:?}",
                    &codes[0]
                );
            }
        }
        tui.shutdown();
    }
}

#[test]
fn home_returns_to_the_same_conversation_by_escape_click_and_typing() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        tui.send(keys::key::up()).unwrap();
        wait(&mut tui, TITLE);
        click_text(&mut tui, TITLE);
        wait(&mut tui, "Resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, SAVED_TEXT);
        for return_path in ["escape", "click", "type"] {
            tui.paste("/home").unwrap();
            tui.send(keys::key::enter()).unwrap();
            wait(&mut tui, "Back to conversation");
            capture(&mut tui, "home-return");
            match return_path {
                "escape" => tui.send(keys::key::esc()).unwrap(),
                "click" => click_text(&mut tui, "Back to conversation"),
                _ => tui.send("draft stays here").unwrap(),
            }
            tui.wait_for(|frame| !frame.contains("Back to conversation"), WAIT)
                .unwrap();
            tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
            if return_path == "type" {
                wait(&mut tui, "draft stays here");
                capture(&mut tui, "home-return-draft");
                tui.send(keys::key::ctrl('u')).unwrap();
            }
            // Slash commands add transcript rows. At 40x12 the original
            // message is now above the viewport, so inspect scrollback.
            tui.send(keys::key::page_up()).unwrap();
            wait(&mut tui, SAVED_TEXT);
            tui.send(keys::key::alt('G')).unwrap();
            tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        }
        tui.paste("/overview").unwrap();
        tui.send(keys::key::enter()).unwrap();
        tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
        // The dashboard is longer than a short transcript viewport.
        for _ in 0..20 {
            if tui.frame().contains("Quick Actions") {
                break;
            }
            tui.send(keys::key::page_up()).unwrap();
            tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        }
        wait(&mut tui, "Quick Actions");
        tui.shutdown();
    }
}
