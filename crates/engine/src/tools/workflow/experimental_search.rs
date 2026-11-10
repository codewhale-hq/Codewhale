use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{SubAgentResult, SubAgentStatus, TaskRequest};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Gate {
    argv: Vec<String>,
    timeout_secs: u64,
    min_tests: u64,
    holdout: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    version: u32,
    objective: String,
    public_fixture_path: PathBuf,
    hidden_evaluator_path: PathBuf,
    allowed_write_roots: Vec<String>,
    protected_paths: Vec<String>,
    hard_gates: Vec<Gate>,
    requested_provider: String,
    requested_model: String,
    resolved_provider: String,
    resolved_model: String,
    max_candidates: u32,
    max_generations: u32,
    max_wall_secs: u64,
}

pub(super) struct ExperimentalSearch {
    spec: Spec,
    baseline_sha: String,
    hidden_hash: String,
    fixture_hash: String,
    gate_artifacts: Vec<(PathBuf, String)>,
    root: PathBuf,
    repo: PathBuf,
    started: Instant,
    admitted: u32,
    evaluated: HashMap<String, Value>,
    patch_hashes: HashSet<String>,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn git(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(cwd, args)?.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|error| error.to_string())
}

fn normalized_path(value: &str) -> Result<String, String> {
    let value = value.replace('\\', "/");
    if value.is_empty()
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(&value)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("invalid repo-relative path {value:?}"));
    }
    Ok(value)
}

fn in_scope(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

fn protected_test_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("tests/")
        || path.contains("/tests/")
        || file.starts_with("test_")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs")
        || file.ends_with(".test.js")
        || file.ends_with(".test.ts")
        || file.ends_with(".spec.js")
        || file.ends_with(".spec.ts")
}

fn nul_paths(bytes: &[u8]) -> Result<Vec<String>, String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec())
                .map_err(|error| error.to_string())
                .and_then(|path| normalized_path(&path))
        })
        .collect()
}

fn path_has_link(root: &Path, relative: &str) -> Result<bool, String> {
    let mut current = root.to_path_buf();
    for part in relative.split('/') {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(false)
}

fn test_count(output: &str) -> u64 {
    let patterns = [
        r"test result: (\d+) passed;",
        r"(?m)^Ran (\d+) tests? in ",
        r"(?m)(?:^| )([0-9]+) passed(?:,| in |$)",
    ];
    for pattern in patterns {
        if let Ok(regex) = Regex::new(pattern) {
            let counts = regex
                .captures_iter(output)
                .filter_map(|capture| capture.get(1)?.as_str().parse::<u64>().ok())
                .collect::<Vec<_>>();
            if !counts.is_empty() {
                return counts.into_iter().sum();
            }
        }
    }
    0
}

impl ExperimentalSearch {
    pub(super) fn freeze(
        manifest_path: &Path,
        workspace: &Path,
        run_id: &str,
        brief: &str,
        requested_model: &str,
    ) -> Result<Self, String> {
        let repo = PathBuf::from(git_text(workspace, &["rev-parse", "--show-toplevel"])?);
        let repo = repo.canonicalize().map_err(|error| error.to_string())?;
        if !git_text(
            &repo,
            &["status", "--porcelain", "--untracked-files=normal"],
        )?
        .is_empty()
        {
            return Err("experimental search requires a clean parent checkout".to_string());
        }
        let source = manifest_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if source.starts_with(&repo) {
            return Err(
                "evaluator manifest must live outside the candidate repository".to_string(),
            );
        }
        let spec_bytes = fs::read(&source).map_err(|error| error.to_string())?;
        if spec_bytes.len() > 128 * 1024 {
            return Err("evaluator manifest exceeds 128 KiB".to_string());
        }
        let mut spec: Spec =
            serde_json::from_slice(&spec_bytes).map_err(|error| error.to_string())?;
        if spec.version != 1
            || spec.objective != brief
            || spec.requested_model != requested_model
            || spec.max_candidates == 0
            || spec.max_candidates > 16
            || spec.max_generations == 0
            || spec.max_generations > 3
            || spec.max_wall_secs == 0
            || spec.max_wall_secs > 21_600
            || spec.allowed_write_roots.is_empty()
            || spec.hard_gates.is_empty()
            || spec.hard_gates.len() > 12
            || spec.requested_provider.is_empty()
            || spec.resolved_provider.is_empty()
            || spec.resolved_model.is_empty()
        {
            return Err(
                "invalid experimental search bounds, route, objective or gates".to_string(),
            );
        }
        for root in &mut spec.allowed_write_roots {
            *root = normalized_path(root)?;
        }
        for path in &mut spec.protected_paths {
            *path = normalized_path(path)?;
        }
        for gate in &spec.hard_gates {
            if gate.argv.is_empty()
                || gate.argv[0].is_empty()
                || gate.timeout_secs == 0
                || gate.timeout_secs > 1800
            {
                return Err("invalid hard gate command or timeout".to_string());
            }
        }
        if !spec
            .hard_gates
            .iter()
            .any(|gate| gate.min_tests > 0 && !gate.holdout)
            || !spec
                .hard_gates
                .iter()
                .any(|gate| gate.min_tests > 0 && gate.holdout)
        {
            return Err(
                "public and held-out test gates must each require nonzero tests".to_string(),
            );
        }
        let hidden = spec
            .hidden_evaluator_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if hidden.starts_with(&repo) {
            return Err("hidden evaluator must live outside the candidate repository".to_string());
        }
        if fs::metadata(&hidden)
            .map_err(|error| error.to_string())?
            .len()
            > 1024 * 1024
            || fs::metadata(&spec.public_fixture_path)
                .map_err(|error| error.to_string())?
                .len()
                > 1024 * 1024
        {
            return Err("evaluator or public fixture exceeds 1 MiB".to_string());
        }
        let hidden_hash = hash(&fs::read(&hidden).map_err(|error| error.to_string())?);
        let fixture_hash =
            hash(&fs::read(&spec.public_fixture_path).map_err(|error| error.to_string())?);
        let mut gate_artifacts = Vec::new();
        for gate in &spec.hard_gates {
            for arg in &gate.argv {
                let path = Path::new(arg);
                if path.is_file() {
                    let path = path.canonicalize().map_err(|error| error.to_string())?;
                    let digest = hash(&fs::read(&path).map_err(|error| error.to_string())?);
                    if !gate_artifacts.iter().any(|(known, _)| known == &path) {
                        gate_artifacts.push((path, digest));
                    }
                }
            }
        }
        let fixture_path = spec
            .public_fixture_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if let Ok(relative) = fixture_path.strip_prefix(&repo) {
            let relative = normalized_path(&relative.to_string_lossy())?;
            if !spec.protected_paths.contains(&relative) {
                spec.protected_paths.push(relative);
            }
        }
        for gate in &spec.hard_gates {
            for arg in &gate.argv {
                let candidate = repo.join(arg);
                if candidate.is_file() {
                    let resolved = candidate
                        .canonicalize()
                        .map_err(|error| error.to_string())?;
                    if let Ok(relative) = resolved.strip_prefix(&repo) {
                        let relative = normalized_path(&relative.to_string_lossy())?;
                        if !spec.protected_paths.contains(&relative) {
                            spec.protected_paths.push(relative);
                        }
                    }
                }
            }
        }
        let baseline_sha = git_text(&repo, &["rev-parse", "HEAD"])?;
        let root = std::env::temp_dir().join(format!("codewhale-evolution-{run_id}"));
        fs::create_dir(&root).map_err(|error| error.to_string())?;
        let manifest = json!({
            "version": 1,
            "run_id": run_id,
            "baseline_sha": baseline_sha,
            "objective_sha256": hash(brief.as_bytes()),
            "public_fixture_sha256": fixture_hash,
            "hidden_evaluator_sha256": hidden_hash,
            "spec_sha256": hash(&spec_bytes),
            "scorer_sha256": hash(&serde_json::to_vec(&spec.hard_gates).map_err(|error| error.to_string())?),
            "gate_artifact_sha256": gate_artifacts.iter().map(|(path, digest)| (path.to_string_lossy().to_string(), digest.clone())).collect::<HashMap<_, _>>(),
            "allowed_write_roots": spec.allowed_write_roots,
            "protected_paths": spec.protected_paths,
            "hard_gates": spec.hard_gates,
            "requested_provider": spec.requested_provider,
            "requested_model": spec.requested_model,
            "resolved_provider": spec.resolved_provider,
            "resolved_model": spec.resolved_model,
            "max_candidates": spec.max_candidates,
            "max_generations": spec.max_generations,
            "max_wall_secs": spec.max_wall_secs,
            "integration_policy": "review_only"
        });
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            spec,
            baseline_sha,
            hidden_hash,
            fixture_hash,
            gate_artifacts,
            root,
            repo,
            started: Instant::now(),
            admitted: 0,
            evaluated: HashMap::new(),
            patch_hashes: HashSet::new(),
        })
    }

    pub(super) fn admit(&mut self, request: &TaskRequest) -> Result<(), String> {
        let Some(label) = request.label.as_deref() else {
            return Ok(());
        };
        if !label.starts_with("candidate_g") {
            return Ok(());
        }
        let generation = label
            .strip_prefix("candidate_g")
            .and_then(|tail| tail.split('_').next())
            .and_then(|number| number.parse::<u32>().ok())
            .ok_or_else(|| "invalid candidate generation label".to_string())?;
        if generation >= self.spec.max_generations
            || self.admitted >= self.spec.max_candidates
            || self.started.elapsed().as_secs() >= self.spec.max_wall_secs
            || !request.worktree
            || request.write_authority.as_deref() != Some("worktree_write")
            || request.model.as_deref() != Some(self.spec.requested_model.as_str())
            || request.write_roots != self.spec.allowed_write_roots
        {
            return Err(
                "experimental candidate exceeds frozen budget, route or authority".to_string(),
            );
        }
        self.admitted += 1;
        Ok(())
    }

    fn receipt(&self, candidate_id: &str, replay: bool, value: &Value) -> Result<(), String> {
        let suffix = if replay { "replay" } else { "evaluation" };
        let path = self.root.join(format!("{candidate_id}-{suffix}.json"));
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn evaluate(
        &mut self,
        candidate_id: &str,
        generation: u32,
        lineage: &Value,
        result: &SubAgentResult,
        replay: bool,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        if !candidate_id.starts_with("candidate_g")
            || generation >= self.spec.max_generations
            || self.started.elapsed().as_secs() >= self.spec.max_wall_secs
            || cancel.is_cancelled()
            || !matches!(result.status, SubAgentStatus::Completed)
        {
            return Err("candidate unavailable, cancelled or outside frozen budget".to_string());
        }
        let label_generation = candidate_id
            .strip_prefix("candidate_g")
            .and_then(|tail| tail.split('_').next())
            .and_then(|number| number.parse::<u32>().ok());
        if label_generation != Some(generation) {
            return Err("candidate generation does not match its task label".to_string());
        }
        if replay {
            if self
                .evaluated
                .get(candidate_id)
                .and_then(|value| value.get("status"))
                != Some(&Value::String("PASS".to_string()))
            {
                return Err("only a verified provisional winner can be replayed".to_string());
            }
        } else if self.evaluated.contains_key(candidate_id) {
            return Err("candidate already evaluated".to_string());
        }
        if generation > 0 {
            let parents = lineage
                .get("parent_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| "mutation requires parent_ids".to_string())?;
            if parents.is_empty()
                || !parents.iter().all(|id| {
                    id.as_str()
                        .is_some_and(|id| self.evaluated.contains_key(id))
                })
                || [
                    "hypothesis",
                    "predicted_effect",
                    "mutation_operator",
                    "evidence_refs",
                    "target_scope",
                ]
                .iter()
                .any(|key| lineage.get(*key).is_none())
            {
                return Err(
                    "mutation lineage lacks verified parents or required fields".to_string()
                );
            }
            let refs = lineage
                .get("evidence_refs")
                .and_then(Value::as_array)
                .ok_or_else(|| "mutation evidence_refs must be an array".to_string())?;
            if !parents.iter().all(|parent| {
                let Some(parent) = parent.as_str() else {
                    return false;
                };
                let expected = self.root.join(format!("{parent}-evaluation.json"));
                refs.iter().any(|value| value.as_str() == expected.to_str())
            }) {
                return Err("mutation evidence_refs do not point to parent receipts".to_string());
            }
        }
        if hash(&fs::read(&self.spec.public_fixture_path).map_err(|error| error.to_string())?)
            != self.fixture_hash
        {
            return Err("public fixture changed after freezing".to_string());
        }
        for (path, digest) in &self.gate_artifacts {
            if hash(&fs::read(path).map_err(|error| error.to_string())?) != *digest {
                return Err(format!(
                    "hard gate artifact changed after freezing: {}",
                    path.display()
                ));
            }
        }
        let workspace = result
            .workspace
            .as_ref()
            .ok_or_else(|| "worker has no worktree receipt".to_string())?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if workspace == self.repo
            || result.child_route.as_ref().is_none_or(|route| {
                route.provider_id != self.spec.resolved_provider
                    || route.model_id != self.spec.resolved_model
                    || route.fallback_note.is_some()
            })
        {
            return Err(
                "worker isolation or resolved route differs from frozen manifest".to_string(),
            );
        }
        let common = git_text(&workspace, &["rev-parse", "--git-common-dir"])?;
        let common_path = Path::new(&common);
        let common = if common_path.is_absolute() {
            common_path.to_path_buf()
        } else {
            workspace.join(common_path)
        };
        if common.canonicalize().map_err(|error| error.to_string())?
            != self
                .repo
                .join(".git")
                .canonicalize()
                .map_err(|error| error.to_string())?
        {
            return Err("candidate worktree belongs to another repository".to_string());
        }
        git(
            &workspace,
            &["merge-base", "--is-ancestor", &self.baseline_sha, "HEAD"],
        )?;
        let changed = nul_paths(
            &git(
                &workspace,
                &[
                    "diff",
                    "--name-only",
                    "--no-renames",
                    "-z",
                    &self.baseline_sha,
                    "--",
                ],
            )?
            .stdout,
        )?;
        let untracked = nul_paths(
            &git(
                &workspace,
                &["ls-files", "--others", "--exclude-standard", "-z"],
            )?
            .stdout,
        )?;
        let paths = changed.iter().chain(untracked.iter()).collect::<Vec<_>>();
        let rejected = paths.iter().find(|path| {
            !self
                .spec
                .allowed_write_roots
                .iter()
                .any(|root| in_scope(path, root))
                || self
                    .spec
                    .protected_paths
                    .iter()
                    .any(|root| in_scope(path, root))
                || path.starts_with(".git/")
                || path.starts_with(".codewhale/")
                || protected_test_path(path)
                || path_has_link(&workspace, path).unwrap_or(true)
        });
        if let Some(path) = rejected {
            let receipt = json!({"candidate_id": candidate_id, "status": "REJECTED", "reason": format!("protected or out-of-scope path: {path}"), "baseline_sha": self.baseline_sha, "lineage": lineage});
            self.receipt(candidate_id, replay, &receipt)?;
            if !replay {
                self.evaluated
                    .insert(candidate_id.to_string(), receipt.clone());
            }
            return Ok(receipt);
        }
        let patch = git(
            &workspace,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-renames",
                &self.baseline_sha,
                "--",
            ],
        )?
        .stdout;
        let mut patch_fingerprint = Sha256::new();
        patch_fingerprint.update(&patch);
        for path in &untracked {
            if fs::metadata(workspace.join(path))
                .map_err(|error| error.to_string())?
                .len()
                > 2 * 1024 * 1024
            {
                return Err(format!("untracked file too large: {path}"));
            }
            let bytes = fs::read(workspace.join(path)).map_err(|error| error.to_string())?;
            patch_fingerprint.update(path.as_bytes());
            patch_fingerprint.update(&bytes);
        }
        let patch_hash: String = patch_fingerprint
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let original_hash = self
            .evaluated
            .get(candidate_id)
            .and_then(|value| value.get("patch_sha256"));
        if replay && original_hash != Some(&Value::String(patch_hash.clone())) {
            return Err("winner patch changed after first evaluation".to_string());
        }
        if !replay && !self.patch_hashes.insert(patch_hash.clone()) {
            let receipt = json!({"candidate_id": candidate_id, "status": "DUPLICATE", "patch_sha256": patch_hash, "reason": "identical patch already evaluated", "lineage": lineage});
            self.receipt(candidate_id, false, &receipt)?;
            self.evaluated
                .insert(candidate_id.to_string(), receipt.clone());
            return Ok(receipt);
        }
        if patch.len() > 2 * 1024 * 1024 || paths.is_empty() {
            let receipt = json!({"candidate_id": candidate_id, "status": "REJECTED", "patch_sha256": patch_hash, "reason": "empty or oversized patch", "lineage": lineage});
            self.receipt(candidate_id, replay, &receipt)?;
            if !replay {
                self.evaluated
                    .insert(candidate_id.to_string(), receipt.clone());
            }
            return Ok(receipt);
        }
        let patch_path = self.root.join(format!("{candidate_id}.patch"));
        if !replay {
            fs::write(&patch_path, &patch).map_err(|error| error.to_string())?;
        }
        let replay_dir = self.root.join(format!(
            "{candidate_id}-{}",
            if replay { "replay" } else { "eval" }
        ));
        let replay_string = replay_dir.to_string_lossy().to_string();
        let baseline = self.baseline_sha.clone();
        let apply_result = git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--",
                &replay_string,
                &baseline,
            ],
        )
        .and_then(|_| {
            if !patch.is_empty() {
                let patch_string = patch_path.to_string_lossy().to_string();
                git(&replay_dir, &["apply", "--check", &patch_string])?;
                git(&replay_dir, &["apply", &patch_string])?;
            }
            for path in &untracked {
                if path_has_link(&replay_dir, path)? {
                    return Err(format!("replay path is a link: {path}"));
                }
                let target = replay_dir.join(path);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::copy(workspace.join(path), target).map_err(|error| error.to_string())?;
            }
            Ok(())
        });
        if let Err(reason) = apply_result {
            let receipt = json!({"candidate_id": candidate_id, "status": "REJECTED", "patch_sha256": patch_hash, "reason": reason, "lineage": lineage});
            self.receipt(candidate_id, replay, &receipt)?;
            if !replay {
                self.evaluated
                    .insert(candidate_id.to_string(), receipt.clone());
            }
            return Ok(receipt);
        }
        let mut gate_receipts = Vec::new();
        let mut total_tests = 0u64;
        let mut failure = None;
        let mut public_feedback = String::new();
        for (index, gate) in self.spec.hard_gates.iter().enumerate() {
            if gate.holdout && !replay {
                continue;
            }
            if hash(&fs::read(&self.spec.hidden_evaluator_path).map_err(|error| error.to_string())?)
                != self.hidden_hash
            {
                failure = Some("hidden evaluator changed".to_string());
                break;
            }
            let gate_path = self.root.join(format!(
                "{candidate_id}-{}-gate-{index}.log",
                if replay { "replay" } else { "eval" }
            ));
            let file = File::create(&gate_path).map_err(|error| error.to_string())?;
            let stderr = file.try_clone().map_err(|error| error.to_string())?;
            let mut command = Command::new(&gate.argv[0]);
            command.env_clear();
            for name in [
                "PATH",
                "CARGO_HOME",
                "RUSTUP_HOME",
                "RUSTFLAGS",
                "SYSTEMROOT",
            ] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            command.env("HOME", &self.root);
            command
                .args(&gate.argv[1..])
                .current_dir(&replay_dir)
                .stdout(Stdio::from(file))
                .stderr(Stdio::from(stderr));
            let started = Instant::now();
            let mut child = command.spawn().map_err(|error| error.to_string())?;
            let (exit_code, stop_reason) = loop {
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    break (None, Some("cancelled".to_string()));
                }
                if started.elapsed() >= Duration::from_secs(gate.timeout_secs) {
                    let _ = child.kill();
                    break (None, Some("gate timeout".to_string()));
                }
                if self.started.elapsed() >= Duration::from_secs(self.spec.max_wall_secs) {
                    let _ = child.kill();
                    break (None, Some("experiment wall budget exhausted".to_string()));
                }
                if fs::metadata(&gate_path).is_ok_and(|meta| meta.len() > 32 * 1024 * 1024) {
                    let _ = child.kill();
                    break (None, Some("gate log exceeds 32 MiB".to_string()));
                }
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    break (status.code(), None);
                }
                thread::sleep(Duration::from_millis(100));
            };
            let log_bytes = fs::read(&gate_path).map_err(|error| error.to_string())?;
            let log = String::from_utf8_lossy(&log_bytes);
            let count = test_count(&log);
            total_tests += count;
            let passed = exit_code == Some(0) && count >= gate.min_tests && stop_reason.is_none();
            if !passed && failure.is_none() {
                failure = Some(stop_reason.unwrap_or_else(|| {
                    format!("gate {index} failed: exit={exit_code:?}, tests={count}")
                }));
                if !gate.holdout {
                    public_feedback = log
                        .chars()
                        .rev()
                        .take(2400)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                }
            }
            gate_receipts.push(json!({"argv": gate.argv, "exit_code": exit_code, "test_count": count, "min_tests": gate.min_tests, "holdout": gate.holdout, "wall_ms": started.elapsed().as_millis(), "log": gate_path, "status": if passed { "PASS" } else { "FAIL" }}));
            if !passed {
                break;
            }
        }
        let status = if failure.is_none() && total_tests > 0 {
            "PASS"
        } else {
            "FAIL"
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis();
        let receipt = json!({
            "candidate_id": candidate_id,
            "generation": generation,
            "status": status,
            "reason": failure,
            "patch_sha256": patch_hash,
            "baseline_sha": self.baseline_sha,
            "manifest": self.root.join("manifest.json"),
            "patch": patch_path,
            "patch_excerpt": String::from_utf8_lossy(&patch).chars().take(6000).collect::<String>(),
            "receipt_path": self.root.join(format!("{candidate_id}-{}.json", if replay { "replay" } else { "evaluation" })),
            "replay_worktree": replay_dir,
            "changed_paths": paths,
            "gates": gate_receipts,
            "test_count": total_tests,
            "public_failure_feedback": public_feedback,
            "lineage": lineage,
            "route": &result.child_route,
            "usage": &result.usage,
            "observed_at_ms": now,
            "review_only": true
        });
        self.receipt(candidate_id, replay, &receipt)?;
        if !replay {
            self.evaluated
                .insert(candidate_id.to_string(), receipt.clone());
        }
        Ok(receipt)
    }

    pub(super) fn summary(&self) -> Value {
        json!({"manifest": self.root.join("manifest.json"), "baseline_sha": self.baseline_sha, "admitted": self.admitted, "max_candidates": self.spec.max_candidates, "max_generations": self.spec.max_generations, "receipts": self.root})
    }
}
