//! Real terminal acceptance: draft, schedule/model mouse controls, save,
//! cancel, edit, and process restart. All saved jobs are paused; provider
//! fixtures point to an unused loopback port and no inference is requested.

#![cfg(all(unix, feature = "long-running-tests"))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::qa_harness;
use qa_harness::harness::{Harness, SealedWorkspace, make_sealed_workspace};
use qa_harness::keys::{key, mouse};

const WAIT: Duration = Duration::from_secs(15);
const SIZES: [(u16, u16); 5] = [(12, 40), (16, 60), (24, 80), (32, 100), (40, 140)];

fn wait(tui: &mut Harness, text: &str) {
    if tui.wait_for_text(text, WAIT).is_err() {
        panic!("missing {text:?}\n{}", tui.diagnostics());
    }
}

fn settle(tui: &mut Harness) {
    tui.wait_for_idle(Duration::from_millis(180), Duration::from_secs(5))
        .unwrap();
}

fn send(tui: &mut Harness, bytes: impl AsRef<[u8]>) {
    tui.send(bytes).unwrap();
    settle(tui);
}
fn paste(tui: &mut Harness, text: &str) {
    tui.paste(text).unwrap();
    settle(tui);
}

fn click(tui: &mut Harness, text: &str) {
    wait(tui, text);
    settle(tui);
    tui.pump();
    let (row, col) = tui.frame().find_text(text).unwrap();
    send(tui, mouse::click(row, col));
}

fn open(workspace: &SealedWorkspace, store: &Path, rows: u16, cols: u16) -> Harness {
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("CODEWHALE_AUTOMATIONS_DIR", store.to_str().unwrap())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_TELEMETRY", "0")
        .env("DO_NOT_TRACK", "1")
        .env("NO_ANIMATIONS", "1")
        .env("RUST_LOG", "warn")
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
    send(&mut tui, "/automation");
    send(&mut tui, key::enter());
    wait(&mut tui, "Scheduled automations");
    tui
}

fn record(store: &Path) -> (PathBuf, serde_json::Value) {
    let files: Vec<_> = std::fs::read_dir(store.join("automations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(
        files.len(),
        1,
        "only an explicit Save may create a definition"
    );
    (
        files[0].clone(),
        serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap(),
    )
}

#[test]
fn automations_editor_create_edit_cancel_and_reopen_across_terminal_sizes() {
    for (rows, cols) in SIZES {
        let workspace = make_sealed_workspace().unwrap();
        let store = workspace.home().join("qa-automations");
        let second_workspace = workspace.workspace().join("second-workspace");
        std::fs::create_dir_all(&second_workspace).unwrap();
        std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").unwrap();
        std::fs::write(
            workspace.home().join(".codewhale/config.toml"),
            r#"
provider = "first"
[providers.first]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/first/v1"
api_key = "fixture-first"
model = "private-first"
[providers.second]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/second/v1"
api_key = "fixture-second"
model = "private-second"
[notifications]
method = "off"
completion_sound = "off"
"#,
        )
        .unwrap();
        workspace.trust_workspace().unwrap();
        let mut tui = open(&workspace, &store, rows, cols);
        click(&mut tui, "n New");
        wait(&mut tui, "Time zone: local");
        let name = format!("qa-auto-{cols}");
        paste(&mut tui, &name);
        send(&mut tui, key::tab());
        paste(&mut tui, "first line\nsecond line 🐳");
        send(&mut tui, key::tab());
        send(&mut tui, key::right()); // Weekly
        send(&mut tui, key::tab());
        send(&mut tui, key::ctrl('a'));
        paste(&mut tui, "17:45");
        send(&mut tui, key::up());
        click(&mut tui, "[+]");
        wait(&mut tui, "18:15");
        send(&mut tui, key::tab());
        send(&mut tui, key::right());
        send(&mut tui, " ");
        click(&mut tui, "Wed");
        send(&mut tui, key::tab());
        send(&mut tui, key::enter());
        paste(&mut tui, "private-second");
        wait(&mut tui, "second / private-second");
        send(&mut tui, key::enter());
        send(&mut tui, key::tab());
        send(&mut tui, key::ctrl('a'));
        paste(&mut tui, second_workspace.to_str().unwrap());
        send(&mut tui, key::tab());
        send(&mut tui, " ");
        wait(&mut tui, "Paused");
        send(&mut tui, key::ctrl_s());
        wait(&mut tui, &format!("Saved {name}"));
        let (path, saved) = record(&store);
        assert_eq!(saved["name"], name);
        assert_eq!(saved["prompt"], "first line\nsecond line 🐳");
        assert_eq!(
            saved["rrule"],
            "FREQ=WEEKLY;BYDAY=MO,TU,WE;BYHOUR=18;BYMINUTE=15"
        );
        assert_eq!(saved["status"], "paused");
        assert!(saved["next_run_at"].is_null());
        assert_eq!(
            saved["cwds"][0],
            second_workspace.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(saved["model"], "private-second");
        assert_eq!(saved["model_provider"], "custom");
        assert_eq!(saved["model_provider_id"], "second");
        assert!(
            saved["allow_shell"].is_null()
                && saved["trust_mode"].is_null()
                && saved["auto_approve"].is_null()
        );

        let before_cancel = std::fs::read(&path).unwrap();
        send(&mut tui, "e");
        wait(&mut tui, "Time zone: local");
        send(&mut tui, key::ctrl('a'));
        paste(&mut tui, "discard this edit");
        send(&mut tui, key::esc());
        assert_eq!(std::fs::read(&path).unwrap(), before_cancel);

        send(&mut tui, "e");
        wait(&mut tui, "Time zone: local");
        send(&mut tui, key::ctrl('a'));
        paste(&mut tui, &format!("{name}-edited"));
        send(&mut tui, key::tab());
        send(&mut tui, key::ctrl('a'));
        paste(&mut tui, "updated prompt\nkeep this newline");
        send(&mut tui, key::tab());
        send(&mut tui, key::left()); // Daily
        send(&mut tui, key::tab());
        send(&mut tui, key::ctrl('a'));
        paste(&mut tui, "08:30");
        click(&mut tui, "[save");
        wait(&mut tui, &format!("Saved {name}-edited"));
        let (_, edited) = record(&store);
        assert_eq!(edited["name"], format!("{name}-edited"));
        assert_eq!(edited["prompt"], "updated prompt\nkeep this newline");
        assert_eq!(
            edited["rrule"],
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=8;BYMINUTE=30"
        );
        assert_eq!(edited["model_provider_id"], "second");
        assert_eq!(edited["model"], "private-second");
        assert_eq!(edited["status"], "paused");
        drop(tui);

        let mut reopened = open(&workspace, &store, rows, cols);
        send(&mut reopened, "e");
        wait(&mut reopened, &format!("{name}-edited"));
        send(&mut reopened, key::tab());
        wait(&mut reopened, "updated prompt");
        for _ in 0..3 {
            send(&mut reopened, key::tab());
        }
        wait(&mut reopened, "second / private-second");
        std::fs::write(
            format!("/tmp/cw-automation-editor-pty-{cols}x{rows}.txt"),
            reopened.frame().text(),
        )
        .unwrap();
        send(&mut reopened, key::esc());
        assert_eq!(record(&store).1, edited);
        send(&mut reopened, "d");
        wait(&mut reopened, "Confirm");
        assert_eq!(record(&store).1, edited, "opening review cannot delete");
        send(&mut reopened, "y");
        wait(&mut reopened, "y/Enter");
        send(&mut reopened, key::esc()); // disarm
        send(&mut reopened, key::esc()); // leave review
        assert_eq!(
            record(&store).1,
            edited,
            "cancel keeps the exact definition"
        );
        send(&mut reopened, "d");
        wait(&mut reopened, "Confirm");
        std::fs::write(
            format!("/tmp/cw-automation-delete-review-{cols}x{rows}.txt"),
            reopened.frame().text(),
        )
        .unwrap();
        click(&mut reopened, "Confirm");
        wait(&mut reopened, "y/Enter");
        click(&mut reopened, "Confirm");
        // The review already says "Nothing was deleted"; wait for the durable
        // mutation and its receipt after the armed review has closed.
        reopened
            .wait_for(
                |frame| !path.exists() && frame.contains("deleted") && !frame.contains("y/Enter"),
                WAIT,
            )
            .unwrap_or_else(|error| panic!("deletion did not complete at {cols}x{rows}: {error}"));
        assert!(
            !path.exists(),
            "confirmed mouse control deletes the reviewed definition"
        );
        println!(
            "PASS {cols}x{rows}: create, multiline paste, time/day/model mouse+keyboard, explicit save, cancel unchanged, edit, paused persistence, restart, deletion cancel and mouse confirmation"
        );
    }
}

#[test]
fn plugin_review_control_keeps_trust_pinned_and_separate_from_enable() {
    for (rows, cols) in [(12, 40), (24, 80), (40, 140)] {
        let workspace = make_sealed_workspace().unwrap();
        let state_root = workspace.home().join(".codewhale");
        std::fs::write(state_root.join(".onboarded"), "").unwrap();
        std::fs::write(
            state_root.join("config.toml"),
            r#"
provider = "first"
[providers.first]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/v1"
api_key = "fixture-only"
model = "private-first"
[notifications]
method = "off"
completion_sound = "off"
"#,
        )
        .unwrap();
        workspace.trust_workspace().unwrap();
        let bundle = workspace.workspace().join(".codewhale/plugins/fixture");
        let skill = bundle.join("skills/example/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(bundle.join("plugin.toml"), "schema_version = 1\n[plugin]\nname = \"fixture\"\nversion = \"1.0.0\"\ndescription = \"Local review fixture\"\n[skills]\npath = \"skills\"\n").unwrap();
        std::fs::write(
            &skill,
            "---\nname: example\ndescription: local fixture\n---\nKeep receipts.\n",
        )
        .unwrap();
        let state_path = state_root.join("plugins/state.json");
        let trusted_entries = || -> Vec<serde_json::Value> {
            let Ok(bytes) = std::fs::read(&state_path) else {
                return Vec::new();
            };
            let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            state["plugins"]
                .as_object()
                .unwrap()
                .values()
                .filter(|entry| entry["trust"].is_object())
                .cloned()
                .collect()
        };
        let mut tui = open(
            &workspace,
            &workspace.home().join("qa-automations"),
            rows,
            cols,
        );
        send(&mut tui, key::esc());
        send(&mut tui, "/plugin trust fixture");
        send(&mut tui, key::enter());
        wait(&mut tui, "Confirm");
        assert!(trusted_entries().is_empty());
        send(&mut tui, "y");
        send(&mut tui, key::esc());
        send(&mut tui, key::esc());
        assert!(
            trusted_entries().is_empty(),
            "canceled review cannot grant trust"
        );

        send(&mut tui, "/plugin trust fixture");
        send(&mut tui, key::enter());
        wait(&mut tui, "Confirm");
        std::fs::write(
            &skill,
            "---\nname: example\ndescription: changed fixture\n---\nNew bytes need review.\n",
        )
        .unwrap();
        send(&mut tui, "y");
        send(&mut tui, key::enter());
        assert!(
            trusted_entries().is_empty(),
            "changed content cannot consume an old review"
        );

        send(&mut tui, "/plugin reload");
        send(&mut tui, key::enter());
        send(&mut tui, "/plugin trust fixture");
        send(&mut tui, key::enter());
        wait(&mut tui, "Confirm");
        std::fs::write(
            format!("/tmp/cw-plugin-review-{cols}x{rows}.txt"),
            tui.frame().text(),
        )
        .unwrap();
        click(&mut tui, "Confirm");
        wait(&mut tui, "y/Enter");
        click(&mut tui, "Confirm");
        let deadline = std::time::Instant::now() + WAIT;
        while trusted_entries().is_empty() && std::time::Instant::now() < deadline {
            tui.pump();
            std::thread::sleep(Duration::from_millis(20));
        }
        let entries = trusted_entries();
        assert_eq!(entries.len(), 1, "{}", tui.diagnostics());
        assert_eq!(
            entries[0]["enabled"], false,
            "trust never enables the plugin"
        );
        println!(
            "PASS {cols}x{rows}: plugin review cancel, changed-content refusal, exact mouse confirmation, trust without enable"
        );
    }
}
