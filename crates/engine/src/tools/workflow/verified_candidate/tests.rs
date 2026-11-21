use super::*;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
    worktree: PathBuf,
    manifest: PathBuf,
    #[cfg(target_os = "linux")]
    public_cases: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("candidate");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(
            repo.join("candidate.py"),
            "import json,sys\nprint(json.dumps(json.load(sys.stdin)))\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Evaluator Test",
                "-c",
                "user.email=evaluator@example.invalid",
                "commit",
                "-qm",
                "baseline",
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let public_cases = temp.path().join("public.json");
        let hidden_cases = temp.path().join("hidden.json");
        fs::write(&public_cases, r#"[{"input":1,"expected":2}]"#).unwrap();
        fs::write(&hidden_cases, r#"[{"input":2,"expected":3}]"#).unwrap();
        let manifest = temp.path().join("evaluator.json");
        let spec = json!({
            "version": 1,
            "objective": "increment an integer",
            "baseline_sha": git(&repo, &["rev-parse", "HEAD"]),
            "allowed_write_roots": ["candidate.py", "helper.py"],
            "protected_paths": ["tests"],
            "isolation_backend": "linux-bwrap",
            "max_wall_secs": 60,
            "hard_gates": [
                {"candidate_argv":["/usr/bin/python3","candidate.py"],
                 "cases_path": public_cases, "timeout_secs": 5, "min_tests": 1,
                 "holdout": false},
                {"candidate_argv":["/usr/bin/python3","candidate.py"],
                 "cases_path": hidden_cases, "timeout_secs": 5, "min_tests": 1,
                 "holdout": true}
            ]
        });
        fs::write(&manifest, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();
        Self {
            temp,
            repo,
            worktree,
            manifest,
            #[cfg(target_os = "linux")]
            public_cases,
        }
    }

    fn edit_spec(&self, edit: impl FnOnce(&mut Value)) {
        let mut spec: Value = serde_json::from_slice(&fs::read(&self.manifest).unwrap()).unwrap();
        edit(&mut spec);
        fs::write(&self.manifest, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn correct_candidate(&self) {
        fs::write(
            self.worktree.join("candidate.py"),
            "import json,sys\nprint(json.dumps(json.load(sys.stdin)+1))\n",
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    fn freeze(&self, id: &str) -> VerifiedCandidateEvaluator {
        VerifiedCandidateEvaluator::freeze(&self.manifest, &self.repo, id, "increment an integer")
            .unwrap()
    }
}

#[cfg(target_os = "linux")]
fn bwrap_available() -> bool {
    Path::new("/usr/bin/bwrap").exists()
        && Command::new("/usr/bin/bwrap")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
}

#[test]
fn checked_in_workflow_identity_is_exact() {
    let source = include_str!("../../../../../../workflows/operate_best_of_n.workflow.js");
    assert_eq!(hash(source.as_bytes()), AUTHORIZED_WORKFLOW_SHA256);
    assert!(authorized_workflow_source(source));
    assert!(!authorized_workflow_source(&format!("{source}\n")));
}

#[test]
fn declared_baseline_and_case_identity_fail_closed() {
    let fixture = Fixture::new();
    fixture.edit_spec(|spec| spec["baseline_sha"] = json!("0".repeat(40)));
    assert!(
        VerifiedCandidateEvaluator::freeze(
            &fixture.manifest,
            &fixture.repo,
            "baseline_mismatch",
            "increment an integer"
        )
        .is_err()
    );
    fixture
        .edit_spec(|spec| spec["baseline_sha"] = json!(git(&fixture.repo, &["rev-parse", "HEAD"])));
    fixture.edit_spec(|spec| spec["hard_gates"][0]["min_tests"] = json!(0));
    fixture.edit_spec(|spec| spec["hard_gates"][1]["min_tests"] = json!(0));
    assert!(
        VerifiedCandidateEvaluator::freeze(
            &fixture.manifest,
            &fixture.repo,
            "zero_tests",
            "increment an integer"
        )
        .is_err()
    );
    fixture.edit_spec(|spec| spec["isolation_backend"] = json!("unconfined"));
    assert!(
        VerifiedCandidateEvaluator::freeze(
            &fixture.manifest,
            &fixture.repo,
            "unsupported",
            "increment an integer"
        )
        .is_err()
    );
}

#[test]
fn sealed_artifact_survives_deleted_worktree_and_rejects_tampering() {
    let fixture = Fixture::new();
    let root = fixture.temp.path().join("store");
    let store = ArtifactStore::freeze(&fixture.repo, &root).unwrap();
    fs::write(
        fixture.worktree.join("candidate.py"),
        "import helper\nprint(helper.answer())\n",
    )
    .unwrap();
    fs::write(
        fixture.worktree.join("helper.py"),
        "def answer():\n    return 2\n",
    )
    .unwrap();
    let artifact = store
        .capture(&fixture.worktree, "sealed_1", Vec::new(), &[])
        .unwrap();
    let tree = fixture.temp.path().join("materialized");
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().unwrap(),
        ],
    );
    store.materialize(&artifact, &tree).unwrap();
    assert_eq!(
        fs::read(tree.join("helper.py")).unwrap(),
        b"def answer():\n    return 2\n"
    );
    assert_eq!(artifact.candidate_tree_hash, artifact.artifact_id);
    let blob = artifact
        .entries
        .iter()
        .find(|entry| entry.path == "helper.py")
        .unwrap()
        .blob_hash
        .as_ref()
        .unwrap();
    fs::write(root.join("blobs").join(blob), b"tampered").unwrap();
    assert!(
        store
            .materialize(
                &store.load("sealed_1").unwrap(),
                &fixture.temp.path().join("tampered")
            )
            .is_err()
    );
    let manifest = root.join("artifacts").join("sealed_1.json");
    fs::write(
        &manifest,
        b"{\"schema_version\":1,\"candidate_id\":\"other\"}",
    )
    .unwrap();
    assert!(store.load("sealed_1").is_err());
}

#[test]
fn artifact_rejects_links_traversal_and_protected_paths() {
    let fixture = Fixture::new();
    let root = fixture.temp.path().join("store");
    let store = ArtifactStore::freeze(&fixture.repo, &root).unwrap();
    assert!(normalized_path("../escape").is_err());
    assert!(normalized_path("a/../../escape").is_err());
    fs::create_dir(fixture.worktree.join("tests")).unwrap();
    fs::write(
        fixture.worktree.join("tests").join("test_hidden.py"),
        b"changed",
    )
    .unwrap();
    let artifact = store
        .capture(&fixture.worktree, "protected", Vec::new(), &[])
        .unwrap();
    assert!(
        validate_scope(
            &artifact,
            &["candidate.py".to_owned(), "helper.py".to_owned()],
            &["tests".to_owned()]
        )
        .is_err()
    );
    fs::remove_dir_all(fixture.worktree.join("tests")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.temp.path().join("public.json"),
            fixture.worktree.join("helper.py"),
        )
        .unwrap();
        assert!(
            store
                .capture(&fixture.worktree, "link", Vec::new(), &[])
                .is_err()
        );
    }
}

#[test]
fn generated_outputs_are_recorded_without_hiding_source_or_protected_changes() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        fixture.repo.join(".gitignore"),
        "target/\n__pycache__/\noutside/\n",
    )
    .unwrap();
    fs::create_dir(fixture.repo.join("target")).unwrap();
    fs::write(fixture.repo.join("target/tracked.rs"), "baseline").unwrap();
    git(&fixture.repo, &["add", ".gitignore"]);
    git(&fixture.repo, &["add", "-f", "target/tracked.rs"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Evaluator Test",
            "-c",
            "user.email=evaluator@example.invalid",
            "commit",
            "-qm",
            "baseline with ignored tracked file",
        ],
    );
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--detach",
            fixture.worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let root = fixture.temp.path().join("store");
    let store = ArtifactStore::freeze(&fixture.repo, &root).unwrap();
    fs::write(fixture.worktree.join("target/tracked.rs"), "changed").unwrap();
    fs::write(fixture.worktree.join("target/build.o"), b"generated").unwrap();
    fs::create_dir(fixture.worktree.join("__pycache__")).unwrap();
    fs::write(
        fixture.worktree.join("__pycache__/candidate.pyc"),
        b"generated",
    )
    .unwrap();
    fs::write(fixture.worktree.join("helper.py"), "def helper(): pass\n").unwrap();
    fs::create_dir_all(fixture.worktree.join("tests/__pycache__")).unwrap();
    fs::write(
        fixture.worktree.join("tests/__pycache__/poison.pyc"),
        b"protected",
    )
    .unwrap();
    fs::create_dir(fixture.worktree.join("outside")).unwrap();
    fs::write(
        fixture.worktree.join("outside/ignored.txt"),
        b"out of scope",
    )
    .unwrap();
    assert_eq!(
        git(&fixture.worktree, &["check-ignore", "target/build.o"]),
        "target/build.o"
    );
    let protected = ["tests".to_owned()];
    let artifact = store
        .capture(&fixture.worktree, "generated", Vec::new(), &protected)
        .unwrap();
    let changed = artifact
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert!(changed.contains(&"target/tracked.rs"));
    assert!(changed.contains(&"helper.py"));
    assert!(changed.contains(&"tests/__pycache__/poison.pyc"));
    assert!(changed.contains(&"outside/ignored.txt"));
    assert!(!changed.contains(&"target/build.o"));
    assert!(!changed.contains(&"__pycache__/candidate.pyc"));
    assert_eq!(
        artifact.excluded_generated_paths,
        ["__pycache__/candidate.pyc", "target/build.o"]
    );
    assert!(
        validate_scope(
            &artifact,
            &[
                "candidate.py".to_owned(),
                "helper.py".to_owned(),
                "target/tracked.rs".to_owned()
            ],
            &protected,
        )
        .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn trusted_oracle_beats_candidate_claim_and_replay_uses_seal() {
    if !bwrap_available() {
        return;
    }
    let fixture = Fixture::new();
    fs::write(
        fixture.worktree.join("candidate.py"),
        "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'status':'PASS','test_count':999}))\n",
    )
    .unwrap();
    let mut evaluator = fixture.freeze("claim");
    assert_eq!(
        evaluator.sandbox_probe["resource_limits"]["processes_rlimit"],
        4096
    );
    let failed = evaluator
        .evaluate(
            "claim_1",
            &fixture.worktree,
            true,
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(failed["status"], "FAIL");
    assert_eq!(failed["test_count"], 1);
    fixture.correct_candidate();
    let passed = evaluator
        .evaluate(
            "correct_1",
            &fixture.worktree,
            true,
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(passed["status"], "PASS");
    assert_eq!(passed["test_count"], 1);
    git(
        &fixture.repo,
        &[
            "worktree",
            "remove",
            "--force",
            fixture.worktree.to_str().unwrap(),
        ],
    );
    let replay = evaluator
        .replay("correct_1", &CancellationToken::new())
        .unwrap();
    assert_eq!(replay["status"], "PASS");
    assert_eq!(replay["test_count"], 2);
    assert_eq!(replay["artifact_id"], passed["artifact_id"]);
    assert_eq!(replay["candidate_tree_hash"], passed["candidate_tree_hash"]);
}

#[cfg(target_os = "linux")]
#[test]
fn missing_evidence_incomplete_worker_and_changed_oracle_are_rejected() {
    if !bwrap_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.correct_candidate();
    let mut evaluator = fixture.freeze("identity");
    assert!(
        evaluator
            .evaluate(
                "not_done",
                &fixture.worktree,
                false,
                &CancellationToken::new()
            )
            .is_err()
    );
    fs::write(&fixture.public_cases, r#"[{"input":1,"expected":999}]"#).unwrap();
    assert!(
        evaluator
            .evaluate(
                "changed_oracle",
                &fixture.worktree,
                true,
                &CancellationToken::new()
            )
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn evaluation_deadline_begins_after_generation_delay() {
    if !bwrap_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.correct_candidate();
    fixture.edit_spec(|spec| spec["max_wall_secs"] = json!(4));
    let mut evaluator = fixture.freeze("delayed_generation");
    std::thread::sleep(Duration::from_secs(5));
    let receipt = evaluator
        .evaluate(
            "delayed_candidate",
            &fixture.worktree,
            true,
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(receipt["status"], "PASS", "{receipt}");
    assert!(receipt["evaluation_started_at_ms"].as_u64().is_some());
    assert!(receipt["execution_deadline_at_ms"].as_u64().is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn timeout_and_cancellation_reap_descendants_inside_writable_sandbox() {
    if !bwrap_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let code = format!(
        "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',\"import time; time.sleep(2); open('marker','w').write('leak')\",{token:?}])\nopen('ready','w').write('spawned')\ntime.sleep(10)\n"
    );
    let run = |label: &str, timeout: Duration, cancel: Option<&CancellationToken>| {
        let scratch = root.path().join(label);
        fs::create_dir(&scratch).unwrap();
        let env = isolated_command(
            &source,
            &scratch,
            "/usr/bin/python3",
            vec!["-c".to_owned(), code.clone()],
            timeout,
        )
        .unwrap();
        let outcome = run_isolated(
            &env,
            b"",
            &root.path().join(format!("{label}.stdout")),
            &root.path().join(format!("{label}.stderr")),
            timeout,
            cancel,
        )
        .unwrap();
        (scratch, outcome)
    };
    let (control, control_run) = run("control", Duration::from_secs(4), None);
    assert_eq!(control_run.stop_reason.as_deref(), Some("timeout"));
    assert_eq!(fs::read(control.join("marker")).unwrap(), b"leak");
    let (timed, timed_run) = run("timed", Duration::from_secs(1), None);
    assert_eq!(timed_run.stop_reason.as_deref(), Some("timeout"));
    assert!(timed.join("ready").exists());
    std::thread::sleep(Duration::from_secs(3));
    assert!(!timed.join("marker").exists());
    let cancelled = CancellationToken::new();
    let scratch = root.path().join("cancelled");
    fs::create_dir(&scratch).unwrap();
    let env = isolated_command(
        &source,
        &scratch,
        "/usr/bin/python3",
        vec!["-c".to_owned(), code],
        Duration::from_secs(5),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            run_isolated(
                &env,
                b"",
                &root.path().join("cancelled.stdout"),
                &root.path().join("cancelled.stderr"),
                Duration::from_secs(5),
                Some(&cancelled),
            )
            .unwrap()
        });
        let started = Instant::now();
        while !scratch.join("ready").exists() && started.elapsed() < Duration::from_secs(4) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(scratch.join("ready").exists());
        cancelled.cancel();
        assert_eq!(
            handle.join().unwrap().stop_reason.as_deref(),
            Some("cancelled")
        );
    });
    std::thread::sleep(Duration::from_secs(3));
    assert!(!scratch.join("marker").exists());
    let survivors = fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().parse::<u32>().is_ok())
        .filter_map(|entry| fs::read(entry.path().join("cmdline")).ok())
        .filter(|cmdline| {
            cmdline
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        })
        .count();
    assert_eq!(survivors, 0);
}
