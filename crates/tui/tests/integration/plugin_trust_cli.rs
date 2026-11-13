use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    workspace: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = TempDir::new().expect("home");
        std::fs::create_dir_all(home.path().join(".codewhale")).expect("config dir");
        Self {
            home,
            workspace: TempDir::new().expect("workspace"),
        }
    }

    fn marked(&self) -> [(&'static str, &str); 1] {
        [(
            "CODEWHALE_AGENT_PROJECT",
            self.workspace.path().to_str().expect("utf-8 temp path"),
        )]
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run_in(self.workspace.path(), args, env)
    }

    #[cfg(unix)]
    fn status_in(&self, workspace: &Path) -> Value {
        let output = self.run_in(workspace, &["plugins", "status", "demo", "--json"], &[]);
        assert!(output.status.success(), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).expect("status json");
        value["plugins"][0].clone()
    }

    fn run_in(&self, workspace: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(crate::binary::codewhale());
        command.env_clear();
        for key in [
            "PATH",
            "TMPDIR",
            "TEMP",
            "TMP",
            "TERM",
            "LANG",
            "LC_ALL",
            "SystemRoot",
            "WINDIR",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .current_dir(workspace)
            .arg("--workspace")
            .arg(workspace)
            .arg("--no-project-config")
            .args(args)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .envs(env.iter().copied())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env(
                "CODEWHALE_CONFIG_PATH",
                self.home.path().join(".codewhale").join("config.toml"),
            )
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .output()
            .expect("run codewhale")
    }

    fn ok(&self, args: &[&str]) -> Output {
        self.ok_with(args, &[])
    }

    fn ok_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let output = self.run_with(args, env);
        assert!(
            output.status.success(),
            "{args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn refused(&self, args: &[&str], needle: &str) {
        self.refused_with(args, &[], needle);
    }

    fn refused_with(&self, args: &[&str], env: &[(&str, &str)], needle: &str) -> String {
        let output = self.run_with(args, env);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{args:?} must be refused\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            stderr.contains(needle),
            "{args:?}: expected `{needle}` in:\n{stderr}"
        );
        stderr.into_owned()
    }

    fn status(&self) -> Value {
        let output = self.ok(&["plugins", "status", "demo", "--json"]);
        let value: Value = serde_json::from_slice(&output.stdout).expect("status json");
        value["plugins"][0].clone()
    }
}

fn write_bundle(parent: &Path) -> std::path::PathBuf {
    let source = parent.join("demo");
    std::fs::create_dir_all(source.join("skills").join("hello")).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    )
    .unwrap();
    std::fs::write(
        source.join("skills").join("hello").join("SKILL.md"),
        "---\nname: hello\ndescription: hi\n---\nbody\n",
    )
    .unwrap();
    source
}

fn review(fixture: &Fixture) -> String {
    let output = fixture.ok(&["plugins", "trust", "demo", "--json"]);
    let value: Value = serde_json::from_slice(&output.stdout).expect("review json");
    assert!(
        value["review"]
            .as_str()
            .is_some_and(|review| review.contains("Activation boundary:")),
        "{value}"
    );
    value["bundle_review_token"].as_str().unwrap().to_string()
}

#[test]
fn headless_refusals_need_no_terminal_and_change_nothing() {
    let fixture = Fixture::new();
    write_bundle(&fixture.home.path().join(".codewhale").join("plugins"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let state_dir = fixture.home.path().join(".codewhale").join("plugins");
        let mode = |mode| {
            std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        mode(0o755);
        let unreadable = fixture.refused_with(&["plugins", "enable", "demo"], &[], "is unknown");
        assert!(!unreadable.contains("plugins trust"), "{unreadable}");
        mode(0o700);
    }
    assert_ne!(fixture.status()["trust"], "trusted");
    fixture.refused(
        &["plugins", "enable", "demo"],
        if cfg!(unix) { "is not trusted" } else { "demo" },
    );

    let text = fixture.ok(&["plugins", "trust", "demo"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Activation boundary:"), "{text}");
    assert!(text.contains("does not record that anyone read"), "{text}");
    let token = review(&fixture);
    assert!(
        text.contains(&format!("Bundle review token: {token}")),
        "{text}"
    );
    assert!(
        text.contains("trusted from the command line with --yes"),
        "{text}"
    );
    let (content, capability) = token.split_once('.').expect("two digests");

    fixture.refused(&["plugins", "trust", "demo", "--yes"], "--review-token");
    fixture.refused(
        &["plugins", "trust", "demo", "--review-token", &token],
        "Trust needs confirmation",
    );
    fixture.refused(
        &[
            "plugins",
            "trust",
            "demo",
            "--review-token",
            &format!("{content}.{}", "0".repeat(64)),
            "--yes",
        ],
        "Bundle review token does not match",
    );
    fixture.refused(
        &[
            "plugins",
            "trust",
            "demo",
            "--review-token",
            &format!("{}.{capability}", "0".repeat(64)),
            "--yes",
        ],
        "content changed",
    );
    fixture.refused_with(
        &["plugins", "enable", "demo"],
        &fixture.marked(),
        if cfg!(unix) { "is not trusted" } else { "demo" },
    );
    let marked_review = fixture.ok_with(&["plugins", "trust", "demo"], &fixture.marked());
    let marked_review = String::from_utf8_lossy(&marked_review.stdout);
    assert!(
        marked_review.contains("approved by the agent from a shell command"),
        "{marked_review}"
    );

    let plugin = fixture.status();
    assert_ne!(plugin["trust"], "trusted");
    assert_eq!(plugin["enabled"], false);
}

#[cfg(unix)]
#[test]
fn headless_install_review_trust_enable_disable_revoke_journey() {
    let fixture = Fixture::new();
    let source = write_bundle(&fixture.workspace.path().join("source"));
    fixture.ok(&["install", source.to_str().unwrap()]);

    let plugin = fixture.status();
    assert_eq!(plugin["enabled"], false);
    assert_ne!(plugin["trust"], "trusted");
    fixture.refused(&["plugins", "enable", "demo"], "codewhale plugins trust");

    let token = review(&fixture);
    assert_ne!(
        fixture.status()["trust"],
        "trusted",
        "review alone must not trust"
    );
    let trust = [
        "plugins",
        "trust",
        "demo",
        "--review-token",
        &token,
        "--yes",
    ];
    fixture.ok(&trust);
    let plugin = fixture.status();
    assert_eq!(plugin["trust"], "trusted");
    assert_eq!(plugin["enabled"], false);
    assert_eq!(plugin["approved_by"]["kind"], "command_line");

    fixture.refused_with(
        &["plugins", "enable", "demo"],
        &fixture.marked(),
        "approved for every project",
    );
    assert_eq!(fixture.status()["enabled"], false);
    fixture.ok(&["plugins", "enable", "demo"]);
    let plugin = fixture.status();
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["active"], true);
    assert_eq!(plugin["enabled_by"]["kind"], "command_line");

    fixture.ok(&["plugins", "disable", "demo"]);
    assert_eq!(fixture.status()["enabled"], false);

    let skill = fixture
        .home
        .path()
        .join(".codewhale/plugins/demo/skills/hello/SKILL.md");
    std::fs::write(&skill, "---\nname: hello\ndescription: hi\n---\nchanged\n").unwrap();
    fixture.refused(&trust, "content changed");
    assert_ne!(fixture.status()["trust"], "trusted");

    let token = review(&fixture);
    fixture.ok_with(
        &[
            "plugins",
            "trust",
            "demo",
            "--review-token",
            &token,
            "--yes",
        ],
        &fixture.marked(),
    );
    let plugin = fixture.status();
    assert_eq!(plugin["approved_by"]["kind"], "agent");
    assert_eq!(plugin["approved_by"]["posture"], "command_line");
    assert_eq!(plugin["outside_reach"], false);
    fixture.ok_with(&["plugins", "enable", "demo"], &fixture.marked());
    let plugin = fixture.status();
    assert_eq!(plugin["active"], true);
    assert_eq!(plugin["enabled_by"]["kind"], "agent");
    fixture.ok(&["plugins", "revoke", "demo"]);
    let plugin = fixture.status();
    assert_ne!(plugin["trust"], "trusted");
    assert_eq!(plugin["active"], false);
}

#[cfg(unix)]
fn trust_args(token: &str) -> [&str; 6] {
    ["plugins", "trust", "demo", "--review-token", token, "--yes"]
}

#[cfg(unix)]
#[test]
fn an_agents_trust_and_enable_reach_only_its_own_project() {
    let fixture = Fixture::new();
    let source = write_bundle(&fixture.workspace.path().join("source"));
    fixture.ok(&["install", source.to_str().unwrap()]);
    let other = TempDir::new().expect("other project");

    let token = review(&fixture);
    fixture.ok_with(&trust_args(&token), &fixture.marked());
    fixture.ok_with(&["plugins", "enable", "demo"], &fixture.marked());
    let here = fixture.status();
    assert_eq!(here["approved_by"]["kind"], "agent", "{here}");
    assert_eq!(here["enabled_by"]["kind"], "agent", "{here}");
    assert_eq!(here["active"], true, "{here}");
    let there = fixture.status_in(other.path());
    assert_eq!(there["outside_reach"], true, "{there}");
    assert_eq!(there["active"], false, "{there}");
}

#[cfg(unix)]
#[test]
fn the_agent_trusts_only_a_plugin_it_installed_from_its_project() {
    let fixture = Fixture::new();
    let source = write_bundle(&fixture.home.path().join("downloads"));
    fixture.ok(&["install", source.to_str().unwrap()]);
    let token = review(&fixture);

    fixture.refused_with(&trust_args(&token), &fixture.marked(), "ask the person");
    assert_ne!(fixture.status()["trust"], "trusted");

    fixture.ok(&trust_args(&token));
    assert_eq!(fixture.status()["approved_by"]["kind"], "command_line");
}

#[cfg(target_os = "linux")]
#[test]
fn a_prompt_answered_in_a_pseudo_terminal_is_the_agents_when_marked() {
    use std::io::Write;

    let fixture = Fixture::new();
    let source = write_bundle(&fixture.workspace.path().join("source"));
    fixture.ok(&["install", source.to_str().unwrap()]);
    let quoted = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let answer_in_pty = |env: &[(&str, &str)]| {
        let line = [
            quoted(crate::binary::codewhale().to_str().unwrap()),
            "--workspace".to_string(),
            quoted(fixture.workspace.path().to_str().unwrap()),
            "--no-project-config plugins trust demo".to_string(),
        ]
        .join(" ");
        let mut command = Command::new("script");
        command.env_clear();
        for key in ["PATH", "TMPDIR", "LANG", "LC_ALL"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command
            .current_dir(fixture.workspace.path())
            .args(["-qec", line.as_str(), "/dev/null"])
            .env("TERM", "xterm")
            .env("HOME", fixture.home.path())
            .env("XDG_CONFIG_HOME", fixture.home.path().join(".config"))
            .env(
                "CODEWHALE_CONFIG_PATH",
                fixture.home.path().join(".codewhale").join("config.toml"),
            )
            .env("RUST_LOG", "warn")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("script(1) runs");
        child.stdin.take().unwrap().write_all(b"y\n").unwrap();
        let output = child.wait_with_output().expect("script(1) finishes");
        assert!(output.status.success(), "{output:?}");
    };

    answer_in_pty(&[]);
    let person = fixture.status();
    assert_eq!(person["approved_by"]["kind"], "person", "{person}");

    fixture.ok(&["plugins", "revoke", "demo"]);
    answer_in_pty(&fixture.marked());
    let agent = fixture.status();
    assert_eq!(agent["approved_by"]["kind"], "agent", "{agent}");
    assert_eq!(agent["approved_by"]["posture"], "command_line", "{agent}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_prompt_answered_through_the_agents_terminal_tools_is_the_agents() {
    use codewhale_engine::tools::spec::{ToolContext, ToolSpec};
    use codewhale_engine::tools::terminal_session::{
        TerminalResetTool, TerminalRunTool, TerminalSendTool, TerminalWaitTool,
    };

    let fixture = Fixture::new();
    let source = write_bundle(&fixture.workspace.path().join("source"));
    fixture.ok(&["install", source.to_str().unwrap()]);
    let quoted = |value: &Path| format!("'{}'", value.display().to_string().replace('\'', "'\\''"));
    let home = fixture.home.path();
    let command = format!(
        "HOME={} USERPROFILE={} XDG_CONFIG_HOME={} CODEWHALE_CONFIG_PATH={} RUST_LOG=warn {} --workspace {} --no-project-config plugins trust demo",
        quoted(home),
        quoted(home),
        quoted(&home.join(".config")),
        quoted(&home.join(".codewhale").join("config.toml")),
        quoted(&crate::binary::codewhale()),
        quoted(fixture.workspace.path()),
    );
    let mut context = ToolContext::new(fixture.workspace.path())
        .with_elevated_sandbox_policy(codewhale_sandbox::SandboxPolicy::DangerFullAccess);
    context.auto_approve = true;
    let session = "plugin-trust-prompt";

    let asked = TerminalRunTool
        .execute(
            serde_json::json!({ "command": command, "session": session, "timeout_secs": 5 }),
            &context,
        )
        .await
        .expect("terminal/run");
    assert!(asked.content.contains("[y/N]"), "{}", asked.content);
    TerminalSendTool
        .execute(
            serde_json::json!({ "session": session, "text": "y\n", "wait_ms": 500 }),
            &context,
        )
        .await
        .expect("terminal/send");
    let done = TerminalWaitTool
        .execute(
            serde_json::json!({ "session": session, "timeout_secs": 60 }),
            &context,
        )
        .await
        .expect("terminal/wait");
    let _ = TerminalResetTool
        .execute(serde_json::json!({ "session": session }), &context)
        .await;

    let plugin = fixture.status();
    assert_eq!(
        plugin["approved_by"]["kind"], "agent",
        "{plugin}\n{}",
        done.content
    );
    assert_eq!(plugin["approved_by"]["posture"], "command_line", "{plugin}");
}
