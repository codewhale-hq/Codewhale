mod artifact;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use artifact::{Artifact, ArtifactStore};
use codewhale_sandbox::{
    CommandSpec, SandboxManager, SandboxPolicy, SandboxResourceLimits, SandboxType,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{SubAgentResult, SubAgentStatus, TaskRequest};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Gate {
    candidate_argv: Vec<String>,
    cases_path: PathBuf,
    timeout_secs: u64,
    min_tests: u64,
    holdout: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    input: Value,
    expected: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CandidateStrategy {
    Direct,
    Focused,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TracePreference {
    Concise,
    EvidenceRefs,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExperimentalPolicyGenome {
    planning_budget: u8,
    max_repair_rounds: u8,
    extra_verification: bool,
    candidate_strategy: CandidateStrategy,
    trace_preference: TracePreference,
}

impl Default for ExperimentalPolicyGenome {
    fn default() -> Self {
        Self {
            planning_budget: 1,
            max_repair_rounds: 1,
            extra_verification: false,
            candidate_strategy: CandidateStrategy::Direct,
            trace_preference: TracePreference::EvidenceRefs,
        }
    }
}

impl ExperimentalPolicyGenome {
    fn validate(&self) -> Result<(), String> {
        if self.planning_budget > 3 || self.max_repair_rounds > 2 {
            return Err("shadow policy gene exceeds its bound".to_string());
        }
        Ok(())
    }
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
    max_model_turns: u32,
    max_wall_secs: u64,
    #[serde(default)]
    shadow_policy: ExperimentalPolicyGenome,
}

pub(super) struct ExperimentalSearch {
    spec: Spec,
    baseline_sha: String,
    hidden_hash: String,
    fixture_hash: String,
    gate_artifacts: Vec<(PathBuf, String)>,
    gate_cases: Vec<Vec<Case>>,
    gate_case_hashes: Vec<String>,
    store: ArtifactStore,
    sandbox_probe: Value,
    root: PathBuf,
    repo: PathBuf,
    started: Instant,
    admitted: u32,
    model_requests: u32,
    model_turns_reserved: u32,
    applied: HashMap<String, AppliedPolicy>,
    repair_counts: HashMap<String, u8>,
    evaluated: HashMap<String, Value>,
}

struct AppliedPolicy {
    genome: ExperimentalPolicyGenome,
    planner_calls: u8,
    dev_review_calls: u8,
    events: Vec<Value>,
    receipt: Value,
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
    #[cfg(unix)]
    if value.contains('\\') {
        return Err(format!("invalid repo-relative path {value:?}"));
    }
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

fn candidate_generation(candidate_id: &str) -> Option<u32> {
    let tail = candidate_id.strip_prefix("candidate_g")?;
    let (generation, slot) = tail.split_once('_')?;
    if generation.is_empty()
        || generation.len() > 2
        || slot.is_empty()
        || slot.len() > 3
        || !generation.bytes().all(|byte| byte.is_ascii_digit())
        || !slot.bytes().all(|byte| byte.is_ascii_digit())
        || slot.parse::<u32>().ok()? == 0
    {
        return None;
    }
    generation.parse().ok()
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

fn same_json(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            if a == b {
                return true;
            }
            if a.is_f64() == b.is_f64() {
                return false;
            }
            let (integer, decimal) = if a.is_f64() { (b, a) } else { (a, b) };
            let integer = integer
                .as_i64()
                .map(|value| value as i128)
                .or_else(|| integer.as_u64().map(|value| value as i128));
            integer.is_some_and(|value| {
                value.unsigned_abs() <= (1u128 << 53) && decimal.as_f64() == Some(value as f64)
            })
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_json(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same_json(value, other)))
        }
        _ => actual == expected,
    }
}

struct SandboxRun {
    exit_code: Option<i32>,
    stop_reason: Option<String>,
    stdout: String,
    stderr: String,
    wall_ms: u128,
}

fn isolated_command(
    source: &Path,
    scratch: &Path,
    program: &str,
    args: Vec<String>,
    timeout: Duration,
) -> Result<codewhale_sandbox::ExecEnv, String> {
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        let _ = (source, scratch, program, args, timeout);
        return Err("UNSUPPORTED_ISOLATION: R2 process gates require Linux bubblewrap".to_string());
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        let mut manager = SandboxManager::with_bwrap_preference(true);
        let mut readable = ["/usr", "/lib", "/lib64", "/etc/alternatives"]
            .into_iter()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        readable.push(source.to_path_buf());
        manager.set_readable_paths(readable);
        let spec = CommandSpec::program(program, args, scratch.to_path_buf(), timeout).with_policy(
            SandboxPolicy::WorkspaceWrite {
                writable_roots: Vec::new(),
                network_access: false,
                exclude_tmpdir: true,
                exclude_slash_tmp: true,
            },
        );
        let env = manager.prepare(&spec);
        if env.sandbox_type != SandboxType::LinuxBubblewrap
            || env.command.first().map(String::as_str) != Some("/usr/bin/bwrap")
            || !env.command.iter().any(|arg| arg == "--unshare-all")
            || !env.command.iter().any(|arg| arg == "--die-with-parent")
            || env.command.iter().any(|arg| arg == "--share-net")
            || !env.command.windows(3).any(|part| {
                part[0] == "--ro-bind"
                    && part[1] == source.to_string_lossy()
                    && part[2] == source.to_string_lossy()
            })
        {
            return Err(
                "UNSUPPORTED_ISOLATION: resolved sandbox is not the strict Linux backend"
                    .to_string(),
            );
        }
        Ok(env)
    }
}

fn run_isolated(
    env: &codewhale_sandbox::ExecEnv,
    input: &[u8],
    stdout_path: &Path,
    stderr_path: &Path,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<SandboxRun, String> {
    let input_path = stdout_path.with_extension("input.json");
    fs::write(&input_path, input).map_err(|error| error.to_string())?;
    let stdin = File::open(&input_path).map_err(|error| error.to_string())?;
    let stdout = File::create(stdout_path).map_err(|error| error.to_string())?;
    let stderr = File::create(stderr_path).map_err(|error| error.to_string())?;
    let mut command = Command::new(env.program());
    command
        .args(env.args())
        .current_dir(&env.cwd)
        .env_clear()
        .envs(&env.env)
        .env("HOME", &env.cwd)
        .env("PATH", "/usr/bin:/bin")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let started = Instant::now();
    let mut child = env
        .spawn_background_limited(
            command,
            SandboxResourceLimits {
                address_space_bytes: 512 * 1024 * 1024,
                file_size_bytes: 1024 * 1024,
                open_files: 64,
                processes: 32,
                cpu_seconds: timeout.as_secs().max(1).min(8),
            },
        )
        .map_err(|error| error.to_string())?;
    let (exit_code, stop_reason) = loop {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            let _ = child.kill();
            break (None, Some("cancelled".to_string()));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            break (None, Some("timeout".to_string()));
        }
        if fs::metadata(stdout_path).is_ok_and(|meta| meta.len() > 1024 * 1024)
            || fs::metadata(stderr_path).is_ok_and(|meta| meta.len() > 1024 * 1024)
        {
            let _ = child.kill();
            break (None, Some("output limit".to_string()));
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break (status.code(), None);
        }
        thread::sleep(Duration::from_millis(20));
    };
    child.wait().map_err(|error| error.to_string())?;
    let stdout = fs::read(stdout_path).map_err(|error| error.to_string())?;
    let stderr = fs::read(stderr_path).map_err(|error| error.to_string())?;
    Ok(SandboxRun {
        exit_code,
        stop_reason,
        stdout: String::from_utf8_lossy(&stdout[..stdout.len().min(1024 * 1024)]).into_owned(),
        stderr: String::from_utf8_lossy(&stderr[..stderr.len().min(1024 * 1024)]).into_owned(),
        wall_ms: started.elapsed().as_millis(),
    })
}

fn probe_isolation(root: &Path) -> Result<Value, String> {
    let source = root.join("probe-source");
    let scratch = root.join("probe-scratch");
    fs::create_dir(&source).map_err(|error| error.to_string())?;
    fs::create_dir(&scratch).map_err(|error| error.to_string())?;
    let secret = root.join("probe-secret");
    fs::write(&secret, b"synthetic marker").map_err(|error| error.to_string())?;
    let code = "import json,os,socket,sys\nsecret=sys.argv[1]\nblocked=False\ntry: open(secret).read()\nexcept (OSError,IOError): blocked=True\nopen('scratch-marker','w').write('ok')\nprint(json.dumps({'secret_blocked':blocked,'network_namespace':os.readlink('/proc/self/ns/net')}))";
    let env = isolated_command(
        &source,
        &scratch,
        "/usr/bin/python3",
        vec![
            "-c".to_string(),
            code.to_string(),
            secret.to_string_lossy().into_owned(),
        ],
        Duration::from_secs(5),
    )?;
    let run = run_isolated(
        &env,
        b"",
        &root.join("probe.stdout"),
        &root.join("probe.stderr"),
        Duration::from_secs(5),
        None,
    )?;
    let measured: Value = serde_json::from_str(&run.stdout).map_err(|_| {
        format!(
            "UNSUPPORTED_ISOLATION: probe output invalid: {}",
            run.stderr
        )
    })?;
    let host_net = fs::read_link("/proc/self/ns/net").map_err(|error| error.to_string())?;
    if run.exit_code != Some(0)
        || measured.get("secret_blocked") != Some(&Value::Bool(true))
        || measured.get("network_namespace").and_then(Value::as_str)
            == Some(host_net.to_string_lossy().as_ref())
        || fs::read(scratch.join("scratch-marker")).ok().as_deref() != Some(b"ok")
    {
        return Err(format!(
            "UNSUPPORTED_ISOLATION: capability probe failed: {}",
            run.stderr
        ));
    }
    let version = Command::new("/usr/bin/bwrap")
        .arg("--version")
        .output()
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "backend": "linux-bwrap", "version": String::from_utf8_lossy(&version.stdout).trim(),
        "read_grants": ["/usr", "/lib", "/lib64", "/etc/alternatives", "sealed candidate tree"],
        "writable_grants": ["private scratch", "private /tmp"],
        "network": "unshared", "pid_namespace": "unshared",
        "secret_blocked": true, "probe_exit": run.exit_code,
        "resource_limits": {"address_space_bytes": 512 * 1024 * 1024,
            "file_size_bytes": 1024 * 1024, "open_files": 64,
            "processes_rlimit": 32, "cpu_seconds_per_case": 8},
    }))
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
        if spec.version != 2
            || spec.objective != brief
            || spec.requested_model != requested_model
            || spec.max_candidates == 0
            || spec.max_candidates > 16
            || spec.max_generations == 0
            || spec.max_generations > 3
            || spec.max_model_turns == 0
            || spec.max_model_turns > 113
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
        spec.shadow_policy.validate()?;
        for root in &mut spec.allowed_write_roots {
            *root = normalized_path(root)?;
        }
        for path in &mut spec.protected_paths {
            *path = normalized_path(path)?;
        }
        for gate in &spec.hard_gates {
            if gate.candidate_argv.len() < 2
                || !Path::new(&gate.candidate_argv[0]).is_absolute()
                || gate.candidate_argv[1..]
                    .iter()
                    .any(|arg| arg.starts_with('-'))
                || gate.timeout_secs == 0
                || gate.timeout_secs > 1800
            {
                return Err("invalid process-level candidate command or timeout".to_string());
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
        let mut gate_cases = Vec::new();
        let mut gate_case_hashes = Vec::new();
        for gate in &spec.hard_gates {
            let cases_path = gate
                .cases_path
                .canonicalize()
                .map_err(|error| error.to_string())?;
            if cases_path.starts_with(&repo) {
                return Err("trusted cases must live outside the candidate repository".to_string());
            }
            let bytes = fs::read(&cases_path).map_err(|error| error.to_string())?;
            if bytes.len() > 2 * 1024 * 1024 {
                return Err("trusted case bundle exceeds 2 MiB".to_string());
            }
            let cases: Vec<Case> =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if cases.is_empty() || cases.len() > 256 || cases.len() < gate.min_tests as usize {
                return Err("trusted case bundle has invalid test count".to_string());
            }
            gate_artifacts.push((cases_path, hash(&bytes)));
            gate_case_hashes.push(hash(&bytes));
            gate_cases.push(cases);
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
            for arg in &gate.candidate_argv[1..] {
                let relative = normalized_path(arg)?;
                if !spec
                    .allowed_write_roots
                    .iter()
                    .any(|root| in_scope(&relative, root))
                {
                    return Err(format!(
                        "candidate program is outside write scope: {relative}"
                    ));
                }
            }
        }
        let baseline_sha = git_text(&repo, &["rev-parse", "HEAD"])?;
        let root = std::env::temp_dir().join(format!("codewhale-evolution-{run_id}"));
        fs::create_dir(&root).map_err(|error| error.to_string())?;
        let store = ArtifactStore::freeze(&repo, &root)?;
        let sandbox_probe = probe_isolation(&root)?;
        let manifest = json!({
            "version": 2,
            "run_id": run_id,
            "baseline_sha": baseline_sha,
            "baseline_id": store.baseline_id(),
            "objective_sha256": hash(brief.as_bytes()),
            "public_fixture_sha256": fixture_hash,
            "hidden_evaluator_sha256": hidden_hash,
            "spec_sha256": hash(&spec_bytes),
            "scorer_sha256": hash(&serde_json::to_vec(&spec.hard_gates).map_err(|error| error.to_string())?),
            "gate_artifact_sha256": gate_artifacts.iter().map(|(path, digest)| (path.to_string_lossy().to_string(), digest.clone())).collect::<HashMap<_, _>>(),
            "case_bundle_sha256": gate_case_hashes,
            "sandbox_probe": sandbox_probe,
            "allowed_write_roots": spec.allowed_write_roots,
            "protected_paths": spec.protected_paths,
            "hard_gates": spec.hard_gates,
            "requested_provider": spec.requested_provider,
            "requested_model": spec.requested_model,
            "resolved_provider": spec.resolved_provider,
            "resolved_model": spec.resolved_model,
            "max_candidates": spec.max_candidates,
            "max_generations": spec.max_generations,
            "max_model_turns": spec.max_model_turns,
            "max_wall_secs": spec.max_wall_secs,
            "shadow_policy": spec.shadow_policy,
            "selection_rule": "admissible FAIL repair slot, PASS elite slot, then public passed cases or complete known cost",
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
            gate_cases,
            gate_case_hashes,
            store,
            sandbox_probe,
            root,
            repo,
            started: Instant::now(),
            admitted: 0,
            model_requests: 0,
            model_turns_reserved: 0,
            applied: HashMap::new(),
            repair_counts: HashMap::new(),
            evaluated: HashMap::new(),
        })
    }

    pub(super) fn apply_policy(
        &mut self,
        candidate_id: &str,
        generation: u32,
        requested: &Value,
        lineage: &Value,
    ) -> Result<Value, String> {
        if self.applied.contains_key(candidate_id)
            || candidate_generation(candidate_id) != Some(generation)
            || generation >= self.spec.max_generations
            || self.started.elapsed().as_secs() >= self.spec.max_wall_secs
        {
            return Err("policy plan is duplicate or outside frozen bounds".to_string());
        }
        let genome: ExperimentalPolicyGenome =
            serde_json::from_value(requested.clone()).map_err(|error| error.to_string())?;
        genome.validate()?;
        if generation == 0 && genome != self.spec.shadow_policy {
            return Err("initial policy differs from frozen genome".to_string());
        }
        let mut feedback = String::new();
        let mut parent_artifact_id = None;
        let mut repair_root = None;
        let mut repair_parent = false;
        if generation > 0 {
            let parent_ids = lineage
                .get("parent_ids")
                .and_then(Value::as_array)
                .ok_or("policy mutation requires parent_ids")?;
            if parent_ids.len() != 1 {
                return Err("policy mutation requires one parent".to_string());
            }
            let parent_id = parent_ids[0].as_str().ok_or("invalid policy parent ID")?;
            let parent = self
                .evaluated
                .get(parent_id)
                .ok_or("policy parent not evaluated")?;
            if !matches!(
                parent.get("status").and_then(Value::as_str),
                Some("PASS" | "FAIL")
            ) || lineage.get("parent_policy_id") != parent.get("policy_id")
            {
                return Err("policy parent lacks a verified admissible receipt".to_string());
            }
            let parent_genome: ExperimentalPolicyGenome = serde_json::from_value(
                parent
                    .get("shadow_policy")
                    .cloned()
                    .ok_or("parent genome missing")?,
            )
            .map_err(|error| error.to_string())?;
            let parent_policy =
                serde_json::to_value(&parent_genome).map_err(|error| error.to_string())?;
            let delta = lineage
                .get("mutation_delta")
                .and_then(Value::as_object)
                .ok_or("mutation_delta missing")?;
            let operator = lineage
                .get("mutation_operator")
                .and_then(Value::as_str)
                .ok_or("mutation operator missing")?;
            let fixed = matches!(operator, "fixed_policy_repair" | "fixed_policy_elite");
            if (fixed && (!delta.is_empty() || genome != parent_genome))
                || (!fixed && (delta.is_empty() || delta.len() > 2))
            {
                return Err("declared policy delta does not match operator".to_string());
            }
            if !fixed
                && !matches!(
                    operator,
                    "targeted_assertion_repair"
                        | "alternate_implementation_strategy"
                        | "elite_reproduction"
                )
            {
                return Err("unknown policy mutation operator".to_string());
            }
            let mut inherited = parent_policy.clone();
            let inherited_map = inherited.as_object_mut().ok_or("invalid parent policy")?;
            for (name, value) in delta {
                if !inherited_map.contains_key(name) || inherited_map.get(name) == Some(value) {
                    return Err(format!("mutation gene is invalid or unchanged: {name}"));
                }
                inherited_map.insert(name.clone(), value.clone());
            }
            if serde_json::to_value(&genome).map_err(|error| error.to_string())? != inherited {
                return Err("child genome is not parent genome plus declared delta".to_string());
            }
            if (matches!(
                operator,
                "targeted_assertion_repair"
                    | "alternate_implementation_strategy"
                    | "fixed_policy_repair"
            ) && parent.get("status").and_then(Value::as_str) != Some("FAIL"))
                || (matches!(operator, "fixed_policy_elite" | "elite_reproduction")
                    && parent.get("status").and_then(Value::as_str) != Some("PASS"))
            {
                return Err("mutation operator does not match verified parent outcome".to_string());
            }
            for key in [
                "hypothesis",
                "predicted_effect",
                "evidence_refs",
                "target_scope",
            ] {
                if lineage.get(key).is_none() {
                    return Err(format!("mutation lineage missing {key}"));
                }
            }
            if lineage.get("target_scope") != Some(&json!(self.spec.allowed_write_roots)) {
                return Err("mutation target scope differs from manifest".to_string());
            }
            let parent_receipt = self.root.join(format!("{parent_id}-evaluation.json"));
            if !lineage
                .get("evidence_refs")
                .and_then(Value::as_array)
                .is_some_and(|refs| {
                    refs.iter()
                        .any(|value| value.as_str() == parent_receipt.to_str())
                })
            {
                return Err("mutation lacks parent evaluation receipt".to_string());
            }
            repair_root = Some(
                parent
                    .get("repair_lineage_root")
                    .and_then(Value::as_str)
                    .unwrap_or(parent_id)
                    .to_string(),
            );
            if parent.get("status").and_then(Value::as_str) == Some("FAIL") {
                repair_parent = true;
                let count = self
                    .repair_counts
                    .get(repair_root.as_deref().unwrap_or(parent_id))
                    .copied()
                    .unwrap_or(0);
                if count >= genome.max_repair_rounds {
                    return Err("host-enforced repair round limit reached".to_string());
                }
            }
            feedback = parent
                .get("public_failure_feedback")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(2400)
                .collect();
            if genome.trace_preference == TracePreference::EvidenceRefs {
                feedback.push_str("\nPARENT CODE MATERIAL:\n");
                feedback.extend(
                    parent
                        .get("review_material")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .take(8000),
                );
            }
            parent_artifact_id = parent.get("artifact_id").cloned();
        }
        let policy_id = hash(&serde_json::to_vec(&genome).map_err(|error| error.to_string())?);
        let plan = json!({
            "planning_passes": genome.planning_budget,
            "repair_round_limit": genome.max_repair_rounds,
            "extra_review_passes": u8::from(genome.extra_verification),
            "feedback_view": genome.trace_preference,
        });
        let plan_hash = hash(&serde_json::to_vec(&plan).map_err(|error| error.to_string())?);
        let authority_hash = hash(
            &serde_json::to_vec(&json!({
                "allowed_write_roots": self.spec.allowed_write_roots,
                "protected_paths": self.spec.protected_paths,
                "requested_model": self.spec.requested_model,
                "resolved_model": self.spec.resolved_model,
                "case_hashes": self.gate_case_hashes,
            }))
            .map_err(|error| error.to_string())?,
        );
        let path = self
            .root
            .join(format!("{candidate_id}-applied-policy.json"));
        let events = self
            .root
            .join(format!("{candidate_id}-policy-events.jsonl"));
        let applied = json!({
            "schema_version": 1,
            "candidate_id": candidate_id, "generation": generation,
            "requested_policy_id": policy_id, "effective_policy_id": policy_id,
            "genotype_hash": policy_id, "resolved_execution_plan_hash": plan_hash,
            "resolved_execution_plan": plan,
            "applied_genes": [
                {"name":"planning_budget","requested_value":genome.planning_budget,"effective_value":genome.planning_budget,"owner":"workflow_host","mode":"host_enforced"},
                {"name":"max_repair_rounds","requested_value":genome.max_repair_rounds,"effective_value":genome.max_repair_rounds,"owner":"workflow_host","mode":"host_enforced"},
                {"name":"extra_verification","requested_value":genome.extra_verification,"effective_value":genome.extra_verification,"owner":"workflow_host","mode":"host_enforced"},
                {"name":"trace_preference","requested_value":genome.trace_preference,"effective_value":genome.trace_preference,"owner":"workflow_host","mode":"host_applied"},
                {"name":"candidate_strategy","requested_value":genome.candidate_strategy,"effective_value":genome.candidate_strategy,"owner":"workflow_prompt","mode":"prompt_only"},
            ],
            "inherited_authority_hash": authority_hash,
            "parent_artifact_id": parent_artifact_id,
            "feedback_packet_sha256": hash(feedback.as_bytes()),
            "feedback_packet": feedback,
            "receipt_path": path, "events_path": events,
        });
        let bytes = serde_json::to_vec_pretty(&applied).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        if repair_parent {
            if let Some(root) = repair_root {
                *self.repair_counts.entry(root).or_default() += 1;
            }
        }
        self.applied.insert(
            candidate_id.to_string(),
            AppliedPolicy {
                genome,
                planner_calls: 0,
                dev_review_calls: 0,
                events: Vec::new(),
                receipt: applied.clone(),
            },
        );
        Ok(applied)
    }

    pub(super) fn policy_status(&self, candidate_id: &str) -> Result<Value, String> {
        let applied = self
            .applied
            .get(candidate_id)
            .ok_or("policy was not applied")?;
        Ok(json!({
            "receipt": applied.receipt,
            "planner_calls": applied.planner_calls,
            "dev_review_calls": applied.dev_review_calls,
            "events": applied.events,
        }))
    }

    pub(super) fn select_generation(
        &self,
        generation: u32,
        candidate_ids: &[String],
    ) -> Result<Value, String> {
        if generation >= self.spec.max_generations
            || candidate_ids.is_empty()
            || candidate_ids.len() > 16
            || candidate_ids
                .iter()
                .any(|id| candidate_generation(id) != Some(generation))
        {
            return Err("invalid generation selection panel".to_string());
        }
        let mut failed = Vec::new();
        let mut passing = Vec::new();
        let mut rows = Vec::new();
        for id in candidate_ids {
            let receipt = self.evaluated.get(id);
            let status = receipt
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN");
            let cost = receipt
                .and_then(|value| value.pointer("/usage/cost_microusd"))
                .and_then(Value::as_u64);
            let public_passed = receipt
                .and_then(|value| value.get("gates"))
                .and_then(Value::as_array)
                .map(|gates| {
                    gates
                        .iter()
                        .filter(|gate| gate["holdout"].as_bool() == Some(false))
                        .filter_map(|gate| gate["passed_count"].as_u64())
                        .sum::<u64>()
                });
            rows.push(json!({
                "candidate_id": id, "status": status, "cost_microusd": cost,
                "public_passed_count": public_passed,
                "evaluation_id": receipt.and_then(|value| value.get("evaluation_id")),
                "artifact_id": receipt.and_then(|value| value.get("artifact_id")),
                "missing": receipt.is_none(),
            }));
            match status {
                "PASS" => passing.push((id.clone(), cost.unwrap_or(u64::MAX))),
                "FAIL"
                    if receipt
                        .and_then(|value| value.get("public_failure_feedback"))
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty()) =>
                {
                    failed.push((id.clone(), public_passed.unwrap_or(0)));
                }
                _ => {}
            }
        }
        failed.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        passing.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let mut parent_ids = Vec::new();
        if let Some((id, _)) = failed.first() {
            parent_ids.push(id.clone());
        }
        if let Some((id, _)) = passing.first() {
            parent_ids.push(id.clone());
        }
        for (id, _) in failed.iter().skip(1) {
            parent_ids.push(id.clone());
        }
        for (id, _) in passing.iter().skip(1) {
            parent_ids.push(id.clone());
        }
        let body = json!({
            "schema_version": 1, "generation": generation,
            "panel": "same_frozen_dev_task_and_seed",
            "selection_rule": "admissible FAIL repair slot, PASS elite slot, then public passed cases or complete known cost",
            "candidates": rows, "parent_ids": parent_ids,
            "provisional_winner_id": passing.first().map(|(id, _)| id.as_str()),
            "unknown_cost_count": passing.iter().filter(|(_, cost)| *cost == u64::MAX).count(),
        });
        let selection_id = hash(&serde_json::to_vec(&body).map_err(|error| error.to_string())?);
        let path = self.root.join(format!("selection-g{generation}.json"));
        let value = json!({"selection_id": selection_id, "receipt_path": path, "body": body});
        let bytes = serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(value)
    }

    pub(super) fn admit(&mut self, request: &TaskRequest) -> Result<(), String> {
        let label = request
            .label
            .as_deref()
            .ok_or("experimental task label is required")?;
        let turns = request
            .max_steps
            .ok_or("experimental task requires a model-turn limit")?;
        if self.model_requests >= self.spec.max_candidates.saturating_mul(5).saturating_add(1)
            || self.model_turns_reserved.saturating_add(turns) > self.spec.max_model_turns
            || self.started.elapsed().as_secs() >= self.spec.max_wall_secs
        {
            return Err(
                "experimental request, model-turn reservation or wall ceiling reached".to_string(),
            );
        }
        let read_only = request.write_authority.as_deref() == Some("read_only")
            && !request.worktree
            && request.allowed_tools.as_ref().is_some_and(Vec::is_empty);
        let owner = if let Some(tail) = label.strip_prefix("planner_") {
            let (candidate_id, step) = tail.rsplit_once('_').ok_or("invalid planner label")?;
            let step = step.parse::<u8>().map_err(|error| error.to_string())?;
            let applied = self
                .applied
                .get_mut(candidate_id)
                .ok_or("planner lacks applied policy")?;
            if !read_only
                || request.model.as_deref() != Some(self.spec.requested_model.as_str())
                || turns != 1
                || request.schema_repair_attempts != Some(0)
                || step != applied.planner_calls + 1
                || step > applied.genome.planning_budget
            {
                return Err("planner exceeds applied policy or authority".to_string());
            }
            applied.planner_calls += 1;
            Some(candidate_id.to_string())
        } else if let Some(candidate_id) = label.strip_prefix("dev_review_") {
            let applied = self
                .applied
                .get_mut(candidate_id)
                .ok_or("review lacks applied policy")?;
            if !read_only
                || request.model.as_deref() != Some(self.spec.requested_model.as_str())
                || turns != 1
                || request.schema_repair_attempts != Some(0)
                || !applied.genome.extra_verification
                || applied.dev_review_calls != 0
            {
                return Err("extra review exceeds applied policy or authority".to_string());
            }
            applied.dev_review_calls += 1;
            Some(candidate_id.to_string())
        } else if label.starts_with("candidate_g") {
            let generation =
                candidate_generation(label).ok_or("invalid candidate generation label")?;
            let applied = self
                .applied
                .get(label)
                .ok_or("candidate lacks applied policy")?;
            if generation >= self.spec.max_generations
                || self.admitted >= self.spec.max_candidates
                || applied.planner_calls != applied.genome.planning_budget
                || !request.allowed_tools.as_ref().is_some_and(|tools| {
                    tools.len() == 2
                        && tools.iter().any(|tool| tool == "write_file")
                        && tools.iter().any(|tool| tool == "apply_patch")
                })
                || !request.worktree
                || request.write_authority.as_deref() != Some("worktree_write")
                || request.model.as_deref() != Some(self.spec.requested_model.as_str())
                || turns != 4
                || request.schema_repair_attempts != Some(0)
                || request.write_roots != self.spec.allowed_write_roots
            {
                return Err("candidate exceeds applied policy, route or authority".to_string());
            }
            self.admitted += 1;
            Some(label.to_string())
        } else if matches!(label, "evolution_reviewer" | "evaluator_reviewer") {
            if request.write_authority.as_deref() != Some("read_only")
                || request.worktree
                || !request.allowed_tools.as_ref().is_some_and(Vec::is_empty)
                || request.model.as_deref() != Some(self.spec.requested_model.as_str())
                || turns != 1
                || request.schema_repair_attempts != Some(0)
            {
                return Err("final reviewer must be read-only".to_string());
            }
            None
        } else {
            return Err(format!("unrecognized experimental task label: {label}"));
        };
        self.model_requests += 1;
        self.model_turns_reserved += turns;
        if let Some(candidate_id) = owner {
            let event = json!({"sequence": self.model_requests, "label": label,
                "model": request.model, "write_authority": request.write_authority,
                "max_model_turns": turns, "schema_repair_attempts": request.schema_repair_attempts});
            let applied = self
                .applied
                .get_mut(&candidate_id)
                .ok_or("applied policy missing")?;
            applied.events.push(event.clone());
            let path = self
                .root
                .join(format!("{candidate_id}-policy-events.jsonl"));
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
            writeln!(file, "{event}").map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
        }
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
        policy: &Value,
        result: &SubAgentResult,
        replay: bool,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        if candidate_generation(candidate_id) != Some(generation)
            || generation >= self.spec.max_generations
            || self.started.elapsed().as_secs() >= self.spec.max_wall_secs
            || cancel.is_cancelled()
            || !matches!(result.status, SubAgentStatus::Completed)
        {
            return Err("candidate unavailable, cancelled or outside frozen budget".to_string());
        }
        let policy: ExperimentalPolicyGenome = if policy.is_null() {
            self.spec.shadow_policy.clone()
        } else {
            serde_json::from_value(policy.clone()).map_err(|error| error.to_string())?
        };
        policy.validate()?;
        let applied = self
            .applied
            .get(candidate_id)
            .ok_or("candidate policy was not applied")?;
        if applied.genome != policy || applied.planner_calls != policy.planning_budget {
            return Err("candidate policy or planner events differ from applied plan".to_string());
        }
        if generation == 0 && policy != self.spec.shadow_policy {
            return Err("first-generation policy differs from frozen genome".to_string());
        }
        let parent_ids = if generation == 0 {
            Vec::new()
        } else {
            let parents = lineage
                .get("parent_ids")
                .and_then(Value::as_array)
                .ok_or("mutation requires parent_ids")?;
            if parents.len() != 1 {
                return Err("R2 mutation requires exactly one parent".to_string());
            }
            let parent_id = parents[0].as_str().ok_or("invalid parent ID")?;
            let parent = self
                .evaluated
                .get(parent_id)
                .ok_or("missing verified parent")?;
            let parent_status = parent.get("status").and_then(Value::as_str);
            let operator = lineage.get("mutation_operator").and_then(Value::as_str);
            let fixed = matches!(operator, Some("fixed_policy_repair" | "fixed_policy_elite"));
            if !matches!(parent_status, Some("PASS" | "FAIL"))
                || !matches!(
                    operator,
                    Some(
                        "targeted_assertion_repair"
                            | "alternate_implementation_strategy"
                            | "elite_reproduction"
                            | "fixed_policy_repair"
                            | "fixed_policy_elite"
                    )
                )
                || (matches!(operator, Some("fixed_policy_elite" | "elite_reproduction"))
                    && parent_status != Some("PASS"))
                || (!matches!(operator, Some("fixed_policy_elite" | "elite_reproduction"))
                    && (parent_status != Some("FAIL")
                        || parent
                            .get("public_failure_feedback")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)))
            {
                return Err("mutation parent lacks admissible verified outcome".to_string());
            }
            let parent_policy = parent.get("shadow_policy").ok_or("parent policy missing")?;
            if lineage.get("parent_policy") != Some(parent_policy)
                || lineage.get("parent_policy_id") != parent.get("policy_id")
            {
                return Err("mutation does not name the verified parent genome".to_string());
            }
            let mut inherited = parent_policy.clone();
            let delta = lineage
                .get("mutation_delta")
                .and_then(Value::as_object)
                .ok_or("mutation_delta missing")?;
            if (fixed && !delta.is_empty()) || (!fixed && (delta.is_empty() || delta.len() > 2)) {
                return Err("mutation_delta does not match policy operator".to_string());
            }
            let inherited_map = inherited.as_object_mut().ok_or("invalid parent policy")?;
            for (name, value) in delta {
                if !inherited_map.contains_key(name) || inherited_map.get(name) == Some(value) {
                    return Err(format!("mutation gene is invalid or unchanged: {name}"));
                }
                inherited_map.insert(name.clone(), value.clone());
            }
            if serde_json::to_value(&policy).map_err(|error| error.to_string())? != inherited {
                return Err("child genome is not parent genome plus declared delta".to_string());
            }
            for key in [
                "hypothesis",
                "predicted_effect",
                "evidence_refs",
                "target_scope",
            ] {
                if lineage.get(key).is_none() {
                    return Err(format!("mutation lineage missing {key}"));
                }
            }
            if lineage.get("target_scope") != Some(&json!(self.spec.allowed_write_roots)) {
                return Err("mutation target scope differs from manifest".to_string());
            }
            let parent_receipt = self.root.join(format!("{parent_id}-evaluation.json"));
            if !lineage
                .get("evidence_refs")
                .and_then(Value::as_array)
                .is_some_and(|refs| {
                    refs.iter()
                        .any(|value| value.as_str() == parent_receipt.to_str())
                })
            {
                return Err("mutation lacks parent evaluation receipt".to_string());
            }
            vec![
                parent
                    .get("artifact_id")
                    .and_then(Value::as_str)
                    .ok_or("parent artifact missing")?
                    .to_string(),
            ]
        };
        let repair_lineage_root = if generation == 0 {
            candidate_id.to_string()
        } else {
            let parent_id = lineage["parent_ids"][0]
                .as_str()
                .ok_or("invalid parent ID")?;
            self.evaluated
                .get(parent_id)
                .and_then(|receipt| receipt.get("repair_lineage_root"))
                .and_then(Value::as_str)
                .unwrap_or(parent_id)
                .to_string()
        };
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
        if hash(&fs::read(&self.spec.public_fixture_path).map_err(|error| error.to_string())?)
            != self.fixture_hash
            || hash(&fs::read(&self.spec.hidden_evaluator_path).map_err(|error| error.to_string())?)
                != self.hidden_hash
        {
            return Err("frozen task or evaluator source changed".to_string());
        }
        for (index, (path, digest)) in self.gate_artifacts.iter().enumerate() {
            if hash(&fs::read(path).map_err(|error| error.to_string())?) != *digest
                || self.gate_case_hashes[index] != *digest
            {
                return Err(format!("frozen case bundle changed: {}", path.display()));
            }
        }
        let artifact: Artifact = if replay {
            let original = self
                .evaluated
                .get(candidate_id)
                .ok_or("missing original evaluation")?;
            let path = self
                .root
                .join("artifacts")
                .join(format!("{candidate_id}.json"));
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            if original
                .get("artifact_manifest_sha256")
                .and_then(Value::as_str)
                != Some(hash(&bytes).as_str())
            {
                return Err("sealed artifact manifest changed after evaluation".to_string());
            }
            let artifact = self.store.load(candidate_id)?;
            if original.get("artifact_id").and_then(Value::as_str)
                != Some(artifact.artifact_id.as_str())
            {
                return Err("sealed artifact identity changed".to_string());
            }
            artifact
        } else {
            let workspace = result
                .workspace
                .as_ref()
                .ok_or("worker has no worktree receipt")?
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
                || git_text(&workspace, &["rev-parse", "HEAD"])? != self.baseline_sha
            {
                return Err("candidate worktree is not at the frozen baseline".to_string());
            }
            self.store.capture(&workspace, candidate_id, parent_ids)?
        };
        let paths = artifact
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
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
        });
        if paths.is_empty() || rejected.is_some() {
            let receipt = json!({
                "candidate_id": candidate_id, "generation": generation,
                "status": "REJECTED", "safety_status": "REJECTED",
                "task_status": "UNKNOWN", "execution_status": "NOT_STARTED",
                "reason": rejected.map_or("empty artifact".to_string(), |path| format!("protected or out-of-scope path: {path}")),
                "artifact_id": artifact.artifact_id,
                "repair_lineage_root": repair_lineage_root,
                "baseline_sha": self.baseline_sha, "lineage": lineage, "shadow_policy": policy,
            });
            self.receipt(candidate_id, replay, &receipt)?;
            if !replay {
                self.evaluated
                    .insert(candidate_id.to_string(), receipt.clone());
            }
            return Ok(receipt);
        }
        let artifact_path = self
            .root
            .join("artifacts")
            .join(format!("{candidate_id}.json"));
        let artifact_manifest_sha256 =
            hash(&fs::read(&artifact_path).map_err(|error| error.to_string())?);
        let patch_path = self.root.join(format!("{candidate_id}.patch"));
        let patch = if replay {
            fs::read(&patch_path).map_err(|error| error.to_string())?
        } else {
            let workspace = result
                .workspace
                .as_ref()
                .ok_or("missing candidate workspace")?;
            let patch = git(
                workspace,
                &[
                    "diff",
                    "--binary",
                    "--full-index",
                    "--no-renames",
                    "--no-ext-diff",
                    "--no-textconv",
                    &self.baseline_sha,
                    "--",
                ],
            )?
            .stdout;
            if patch.len() > 2 * 1024 * 1024 {
                return Err("review patch exceeds 2 MiB".to_string());
            }
            fs::write(&patch_path, &patch).map_err(|error| error.to_string())?;
            patch
        };
        let patch_hash = hash(&patch);
        if replay
            && self
                .evaluated
                .get(candidate_id)
                .and_then(|value| value.get("patch_sha256"))
                .and_then(Value::as_str)
                != Some(patch_hash.as_str())
        {
            return Err("review patch changed after evaluation".to_string());
        }
        let material = self.store.review_material(&artifact)?;
        let review_material_hash = material.as_ref().map(|value| hash(value.as_bytes()));
        let review_material_bytes = material.as_ref().map(String::len);
        let replay_dir = self.root.join(format!(
            "{candidate_id}-{}",
            if replay { "replay" } else { "eval" }
        ));
        self.store.materialize(&artifact, &replay_dir)?;
        let mut gate_receipts = Vec::new();
        let mut total_tests = 0u64;
        let mut failure = None;
        let mut public_feedback = String::new();
        let mut execution_status = "COMPLETE";
        for (gate_index, gate) in self.spec.hard_gates.iter().enumerate() {
            if gate.holdout && !replay {
                continue;
            }
            let mut passed_cases = 0u64;
            let mut observed_cases = 0u64;
            let mut logs = Vec::new();
            let gate_started = Instant::now();
            for (case_index, case) in self.gate_cases[gate_index].iter().enumerate() {
                let remaining =
                    Duration::from_secs(gate.timeout_secs).saturating_sub(gate_started.elapsed());
                if remaining.is_zero()
                    || self.started.elapsed().as_secs() >= self.spec.max_wall_secs
                {
                    execution_status = "TIMEOUT";
                    failure.get_or_insert_with(|| {
                        "gate or experiment wall budget exhausted".to_string()
                    });
                    break;
                }
                let scratch = self.root.join(format!(
                    "{candidate_id}-{}-g{gate_index}-c{case_index}-scratch",
                    if replay { "replay" } else { "eval" },
                ));
                fs::create_dir(&scratch).map_err(|error| error.to_string())?;
                let args = gate.candidate_argv[1..]
                    .iter()
                    .map(|arg| replay_dir.join(arg).to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                let env = isolated_command(
                    &replay_dir,
                    &scratch,
                    &gate.candidate_argv[0],
                    args,
                    remaining.min(Duration::from_secs(8)),
                )?;
                let log_root = self.root.join(format!(
                    "{candidate_id}-{}-g{gate_index}-c{case_index}",
                    if replay { "replay" } else { "eval" },
                ));
                let stdout_path = log_root.with_extension("stdout.log");
                let stderr_path = log_root.with_extension("stderr.log");
                let input = serde_json::to_vec(&case.input).map_err(|error| error.to_string())?;
                let run = run_isolated(
                    &env,
                    &input,
                    &stdout_path,
                    &stderr_path,
                    remaining.min(Duration::from_secs(8)),
                    Some(cancel),
                )?;
                observed_cases += 1;
                total_tests += 1;
                let actual = serde_json::from_str::<Value>(run.stdout.trim());
                let passed = run.exit_code == Some(0)
                    && run.stop_reason.is_none()
                    && actual
                        .as_ref()
                        .is_ok_and(|value| same_json(value, &case.expected));
                if passed {
                    passed_cases += 1;
                }
                if !passed && failure.is_none() {
                    failure = Some(format!("gate {gate_index} case {} failed", case_index + 1));
                }
                if !passed && !gate.holdout && public_feedback.len() < 2400 {
                    let feedback = format!(
                        "case {} input={} expected={} actual={} exit={:?} stop={:?} stderr={}\n",
                        case_index + 1,
                        case.input,
                        case.expected,
                        actual.map_or_else(|_| Value::String(run.stdout.clone()), |value| value),
                        run.exit_code,
                        run.stop_reason,
                        run.stderr.chars().take(200).collect::<String>(),
                    );
                    public_feedback.push_str(
                        &feedback
                            .chars()
                            .take(2400 - public_feedback.len())
                            .collect::<String>(),
                    );
                }
                if run.stop_reason.is_some() {
                    execution_status = "STOPPED";
                    break;
                }
                logs.push(json!({
                    "case": case_index + 1, "stdout": stdout_path, "stderr": stderr_path,
                    "exit_code": run.exit_code, "wall_ms": run.wall_ms,
                }));
            }
            let gate_pass = observed_cases == self.gate_cases[gate_index].len() as u64
                && passed_cases == observed_cases
                && passed_cases >= gate.min_tests;
            gate_receipts.push(json!({
                "candidate_argv": gate.candidate_argv, "cases_sha256": self.gate_case_hashes[gate_index],
                "holdout": gate.holdout, "test_count": observed_cases,
                "passed_count": passed_cases, "min_tests": gate.min_tests,
                "status": if gate_pass { "PASS" } else { "FAIL" }, "logs": logs,
            }));
            if !gate_pass {
                break;
            }
        }
        let status = if execution_status != "COMPLETE" {
            "UNKNOWN"
        } else if failure.is_none() && total_tests > 0 {
            "PASS"
        } else {
            "FAIL"
        };
        let policy_bytes = serde_json::to_vec(&policy).map_err(|error| error.to_string())?;
        let policy_id = hash(&policy_bytes);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis();
        let evaluation_id = hash(
            &serde_json::to_vec(&json!({
                "candidate_id": candidate_id, "generation": generation,
                "artifact_id": artifact.artifact_id, "policy_id": policy_id,
                "baseline_id": self.store.baseline_id(), "case_hashes": self.gate_case_hashes,
                "replay": replay, "time_ms": now,
            }))
            .map_err(|error| error.to_string())?,
        );
        let receipt = json!({
            "candidate_id": candidate_id, "generation": generation,
            "status": status, "safety_status": "ADMITTED",
            "task_status": if status == "PASS" { "PASS" } else if status == "FAIL" { "FAIL" } else { "UNKNOWN" },
            "execution_status": execution_status, "reason": failure,
            "artifact_id": artifact.artifact_id, "artifact_manifest_sha256": artifact_manifest_sha256,
            "candidate_tree_hash": artifact.candidate_tree_hash,
            "baseline_id": self.store.baseline_id(), "baseline_sha": self.baseline_sha,
            "policy_id": policy_id, "evaluation_id": evaluation_id,
            "patch_sha256": patch_hash, "patch": patch_path,
            "patch_excerpt": String::from_utf8_lossy(&patch).chars().take(6000).collect::<String>(),
            "review_material": material, "review_complete": material.is_some(),
            "review_material_sha256": review_material_hash,
            "review_material_bytes": review_material_bytes,
            "receipt_path": self.root.join(format!(
                "{candidate_id}-{}.json", if replay { "replay" } else { "evaluation" }
            )),
            "replay_worktree": replay_dir, "changed_paths": paths,
            "gates": gate_receipts, "test_count": total_tests,
            "public_failure_feedback": public_feedback, "lineage": lineage,
            "repair_lineage_root": repair_lineage_root,
            "shadow_policy": policy, "route": &result.child_route,
            "usage": &result.usage, "sandbox_probe": self.sandbox_probe,
            "applied_policy_receipt": applied.receipt.get("receipt_path"),
            "policy_events_path": applied.receipt.get("events_path"),
            "planner_calls": applied.planner_calls,
            "observed_at_ms": now, "review_only": true,
        });
        self.receipt(candidate_id, replay, &receipt)?;
        if !replay {
            self.evaluated
                .insert(candidate_id.to_string(), receipt.clone());
        }
        Ok(receipt)
    }

    pub(super) fn summary(&self) -> Value {
        json!({"manifest": self.root.join("manifest.json"), "baseline_sha": self.baseline_sha, "baseline_id": self.store.baseline_id(), "admitted": self.admitted, "model_requests": self.model_requests, "max_model_requests": self.spec.max_candidates.saturating_mul(5).saturating_add(1), "model_turns_reserved": self.model_turns_reserved, "max_model_turns": self.spec.max_model_turns, "max_candidates": self.spec.max_candidates, "max_generations": self.spec.max_generations, "shadow_policy": self.spec.shadow_policy, "sandbox_probe": self.sandbox_probe, "receipts": self.root})
    }
}
