use std::time::Duration;

use super::qa_harness::harness::{Harness, make_sealed_workspace};

const WAIT: Duration = Duration::from_secs(15);

fn print_frame(label: &str, tui: &mut Harness) {
    tui.pump();
    let text = tui.frame().text();
    println!("--- {label}");
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        println!("{}", line.trim_end());
    }
}

#[test]
fn plugin_create_uses_engine_mode_availability_and_lists_help_in_real_terminal() {
    let workspace = make_sealed_workspace().expect("sealed workspace");
    std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").unwrap();
    let trust = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust).unwrap();
    std::fs::write(trust.join("trusted"), "").unwrap();
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("NO_ANIMATIONS", "1")
        .env("RUST_LOG", "warn")
        .args([
            "--workspace",
            workspace.workspace().to_str().unwrap(),
            "--no-project-config",
            "--skip-onboarding",
            "--fresh",
        ])
        .size(32, 100)
        .spawn()
        .expect("start TUI");
    tui.wait_for_text("New session", WAIT).unwrap();
    tui.wait_for_idle(Duration::from_millis(250), Duration::from_secs(5))
        .unwrap();

    tui.type_line("/plugin help").unwrap();
    tui.wait_for_text("/plugin create [idea]", WAIT).unwrap();
    print_frame("/plugin help in the shipped binary", &mut tui);

    tui.type_line("/plugin create a word counter").unwrap();
    tui.wait_for_text("Not ready yet in this version.", WAIT)
        .unwrap();
    print_frame("/plugin create in the shipped binary", &mut tui);
    let frame = tui.frame().text();
    assert!(
        frame.contains("Creator mode is not available: Not ready yet in this version."),
        "{frame}"
    );
    assert!(
        !frame
            .lines()
            .any(|line| line.contains("Creator mode") && !line.contains("is not available")),
        "{frame}"
    );
    assert!(frame.contains("Type a message"), "{frame}");
    tui.shutdown();
}
