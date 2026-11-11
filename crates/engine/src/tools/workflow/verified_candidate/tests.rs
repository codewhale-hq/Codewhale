use super::*;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
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
            public_cases,
        }
    }

    fn edit_spec(&self, edit: impl FnOnce(&mut Value)) {
        let mut spec: Value = serde_json::from_slice(&fs::read(&self.manifest).unwrap()).unwrap();
        edit(&mut spec);
        fs::write(&self.manifest, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();
    }

    fn correct_candidate(&self) {
        fs::write(
            self.worktree.join("candidate.py"),
            "import json,sys\nprint(json.dumps(json.load(sys.stdin)+1))\n",
        )
        .unwrap();
    }

    fn freeze(&self, id: &str) -> VerifiedCandidateEvaluator {
        VerifiedCandidateEvaluator::freeze(&self.manifest, &self.repo, id, "increment an integer")
            .unwrap()
    }
}

fn bwrap_available() -> bool {
    Path::new("/usr/bin/bwrap").exists()
        && Command::new("/usr/bin/bwrap")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
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
        .capture(&fixture.worktree, "sealed_1", Vec::new())
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
        .capture(&fixture.worktree, "protected", Vec::new())
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
                .capture(&fixture.worktree, "link", Vec::new())
                .is_err()
        );
    }
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
fn timeout_and_cancellation_do_not_leave_descendants() {
    if !bwrap_available() {
        return;
    }
    let fixture = Fixture::new();
    let marker = fixture.temp.path().join("descendant-marker");
    let code = format!(
        "import subprocess,time\nsubprocess.Popen(['/usr/bin/python3','-c',\"import time; time.sleep(2); open({:?},'w').write('leak')\"])\ntime.sleep(20)\n",
        marker.to_str().unwrap()
    );
    fs::write(fixture.worktree.join("candidate.py"), code).unwrap();
    fixture.edit_spec(|spec| {
        spec["hard_gates"][0]["timeout_secs"] = json!(1);
    });
    let mut evaluator = fixture.freeze("descendants");
    let timed = evaluator
        .evaluate("timed", &fixture.worktree, true, &CancellationToken::new())
        .unwrap();
    assert_eq!(timed["status"], "UNKNOWN");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(!marker.exists());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        evaluator
            .evaluate("cancelled", &fixture.worktree, true, &cancelled)
            .is_err()
    );
}
