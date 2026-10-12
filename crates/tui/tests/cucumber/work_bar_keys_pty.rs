//! Press the advertised work-bar chord through a real terminal decoder,
//! including the fresh-session screen where the hint first appears.

use std::time::Duration;

use super::qa_harness::{
    harness::{Harness, make_sealed_workspace},
    keys,
};

#[test]
fn work_bar_opens_from_launch_with_legacy_and_enhanced_keys() {
    for (rows, cols) in [(12, 40), (16, 60), (24, 80), (32, 100), (40, 140)] {
        let workspace = make_sealed_workspace().expect("sealed workspace");
        std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").unwrap();
        let trust = workspace.workspace().join(".deepseek");
        std::fs::create_dir_all(&trust).unwrap();
        std::fs::write(trust.join("trusted"), "").unwrap();
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
                workspace.workspace().to_str().unwrap(),
                "--no-project-config",
                "--mouse-capture",
            ])
            .size(rows, cols)
            .spawn()
            .expect("start TUI");
        tui.wait_for_text("New session", Duration::from_secs(15))
            .unwrap();
        tui.wait_for_idle(Duration::from_millis(250), Duration::from_secs(5))
            .unwrap();

        super::launch_card_pty::capture(&mut tui, "workbar-closed");

        // Legacy Ctrl+] is one byte, decoded by crossterm as Ctrl+5.
        tui.send([0x1d]).unwrap();
        tui.wait_for_text("no agents have run this session", Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("{cols}x{rows}: legacy Ctrl+] failed: {error}"));

        super::launch_card_pty::capture(&mut tui, "workbar-fleet");

        // The enhanced backward chord must work on the same launch screen.
        tui.send(b"\x1b[9;6u").unwrap();
        tui.wait_for_text("no to-dos yet", Duration::from_secs(5))
            .unwrap();
        super::launch_card_pty::capture(&mut tui, "workbar-tasks");
        // The visible close control and keyboard Escape share the close action.
        let (row, col) = tui.frame().find_text("×").expect("visible close control");
        tui.send(keys::mouse::click(row, col)).unwrap();
        tui.wait_for(
            |frame| !frame.contains("no to-dos yet"),
            Duration::from_secs(5),
        )
        .unwrap();

        tui.send(b"\x1b[93;5u").unwrap();
        tui.wait_for_text("no agents have run this session", Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("{cols}x{rows}: enhanced Ctrl+] failed: {error}"));

        // Ordinary bracket/digit text still reaches the composer; the legacy
        // chord also works after typing has dismissed the launch card.
        tui.send("draft ]5").unwrap();
        tui.wait_for_text("draft ]5", Duration::from_secs(5))
            .unwrap();
        tui.wait_for_idle(Duration::from_millis(250), Duration::from_secs(5))
            .unwrap();
        tui.send([0x1d]).unwrap();
        tui.wait_for_text("nothing running in the background", Duration::from_secs(5))
            .unwrap();
        tui.shutdown();
    }
}
