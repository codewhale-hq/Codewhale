use std::time::Duration;

use super::qa_harness::{
    harness::{Harness, make_sealed_workspace},
    keys,
};

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
fn shipped_binary_lists_creator_disabled_and_refuses_new_creator_with_the_reason() {
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

    tui.type_line("/new creator").unwrap();
    tui.wait_for_text("Not ready yet in this version.", WAIT)
        .unwrap();
    print_frame("/new creator in the shipped binary", &mut tui);
    assert!(
        !tui.frame().contains("Creator mode"),
        "the conversation stays Standard: {}",
        tui.diagnostics()
    );

    tui.type_line("/mode").unwrap();
    tui.wait_for_text("Conversation mode", WAIT).unwrap();
    tui.wait_for_text("Permissions", WAIT).unwrap();
    print_frame("/mode picker in the shipped binary", &mut tui);
    let frame = tui.frame().text();
    let creator_row = frame
        .lines()
        .find(|line| line.contains("Creator mode"))
        .expect("Creator is listed");
    assert!(
        creator_row.contains("Not ready yet in this version."),
        "{creator_row}"
    );
    let first_group = frame.find("Conversation mode").unwrap();
    let second_group = frame.find("Permissions").unwrap();
    assert!(first_group < frame.find("Standard mode").unwrap());
    assert!(frame.find("Creator mode").unwrap() < second_group);
    tui.send(keys::key::esc()).unwrap();
    tui.shutdown();
}
