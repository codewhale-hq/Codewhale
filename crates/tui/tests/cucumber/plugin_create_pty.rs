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
    std::fs::write(
        workspace.home().join(".codewhale/config.toml"),
        "[notifications]\nmethod = \"off\"\ncompletion_sound = \"off\"\n\n[extension_host]\nruntime = \"node\"\nnode = \"/nonexistent/codewhale-no-node\"\n",
    )
    .unwrap();
    workspace.trust_workspace().unwrap();
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
    tui.wait_for_text("Creator mode needs a plugin runtime", WAIT)
        .unwrap();
    print_frame("/plugin create in the shipped binary", &mut tui);
    let frame = tui.frame().text();
    let normalized = frame.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains(
            "Creator mode is not available: Creator mode needs a plugin runtime (Node.js or Bun)"
        ),
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
