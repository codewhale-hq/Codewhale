mod artifact;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use artifact::{Artifact, ArtifactStore};
use codewhale_sandbox::SandboxResourceLimits;
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
use codewhale_sandbox::{CommandSpec, SandboxManager, SandboxPolicy, SandboxType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const AUTHORIZED_WORKFLOW_SHA256: &str =
    "55d5544578f0f6f5d7e046221a4e11f36fd97b28f94996e640daec271144ca36";

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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    version: u32,
    objective: String,
    baseline_sha: String,
    allowed_write_roots: Vec<String>,
    protected_paths: Vec<String>,
    isolation_backend: String,
    max_wall_secs: u64,
    hard_gates: Vec<Gate>,
}

pub(super) struct VerifiedCandidateEvaluator {
    spec: Spec,
    spec_path: PathBuf,
    spec_hash: String,
    manifest_hash: String,
    binary_hash: String,
    executable_hashes: HashMap<String, String>,
    gate_cases: Vec<Vec<Case>>,
    case_hashes: Vec<String>,
    store: ArtifactStore,
    sandbox_probe: Value,
    root: PathBuf,
    repo: PathBuf,
    started: Option<Instant>,
    started_at_ms: Option<u64>,
    evaluation_elapsed: Duration,
    evaluated: HashMap<String, Value>,
}

fn candidate_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("invalid candidate identifier".to_string());
    }
    Ok(())
}

fn validate_scope(
    artifact: &Artifact,
    allowed: &[String],
    protected: &[String],
) -> Result<(), String> {
    if artifact.entries.is_empty() {
        return Err("empty candidate artifact".to_string());
    }
    for entry in &artifact.entries {
        normalized_path(&entry.path)?;
        if !allowed.iter().any(|root| in_scope(&entry.path, root))
            || protected.iter().any(|root| in_scope(&entry.path, root))
            || protected_git_path(&entry.path)
            || protected_test_path(&entry.path)
            || in_scope(&entry.path, ".codewhale")
        {
            return Err(format!("protected or out-of-scope path: {}", entry.path));
        }
    }
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn authorized_workflow_source(source: &str) -> bool {
    hash(source.as_bytes()) == AUTHORIZED_WORKFLOW_SHA256
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

fn protected_git_path(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            ".git" | ".gitattributes" | ".gitmodules" | ".gitconfig" | ".lfsconfig"
        )
    })
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

struct GateExecution {
    receipts: Vec<Value>,
    test_count: u64,
    failure: Option<String>,
    feedback: String,
    stop_reason: Option<String>,
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
        Err("UNSUPPORTED_ISOLATION: candidate gates require Linux bubblewrap".to_string())
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
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                // The evaluator command needs only stdin/stdout/stderr. Close
                // every inherited host descriptor before bubblewrap starts.
                let result = libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
                if result != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let started = Instant::now();
    let mut child = env
        .spawn_background_limited(
            command,
            SandboxResourceLimits {
                address_space_bytes: 512 * 1024 * 1024,
                file_size_bytes: 1024 * 1024,
                open_files: 64,
                processes: 4096,
                cpu_seconds: timeout.as_secs().clamp(1, 8),
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

fn probe_isolation(root: &Path, denied_paths: &[PathBuf]) -> Result<Value, String> {
    let source = root.join("probe-source");
    let scratch = root.join("probe-scratch");
    fs::create_dir(&source).map_err(|error| error.to_string())?;
    fs::create_dir(&scratch).map_err(|error| error.to_string())?;
    let secret = root.join("probe-secret");
    fs::write(&secret, b"synthetic marker").map_err(|error| error.to_string())?;
    let code = r#"import json,os,sys
blocked=[]
for path in sys.argv[1:]:
    try:
        open(path).read()
        blocked.append(False)
    except OSError:
        blocked.append(True)
open('scratch-marker','w').write('ok')
print(json.dumps({'secret_blocked':blocked[0],
                  'oracle_paths_blocked':all(blocked[1:]),
                  'network_namespace':os.readlink('/proc/self/ns/net')}))
"#;
    let mut args = vec![
        "-c".to_string(),
        code.to_string(),
        secret.to_string_lossy().into_owned(),
    ];
    args.extend(
        denied_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    let env = isolated_command(
        &source,
        &scratch,
        "/usr/bin/python3",
        args,
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
        || measured.get("oracle_paths_blocked") != Some(&Value::Bool(true))
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
        "secret_blocked": true, "oracle_paths_blocked": true,
        "probe_exit": run.exit_code,
        "resource_limits": {"address_space_bytes": 512 * 1024 * 1024,
            "file_size_bytes": 1024 * 1024, "open_files": 64,
            "processes_rlimit": 4096, "cpu_seconds_per_case": 8},
    }))
}

impl VerifiedCandidateEvaluator {
    pub(super) fn freeze(
        manifest_path: &Path,
        workspace: &Path,
        run_id: &str,
        brief: &str,
    ) -> Result<Self, String> {
        candidate_id(run_id)?;
        let repo = PathBuf::from(git_text(workspace, &["rev-parse", "--show-toplevel"])?)
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let status = git_text(
            &repo,
            &["status", "--porcelain", "--untracked-files=normal"],
        )?;
        if status.lines().any(|line| line != "?? .codewhale/") {
            return Err("evaluator requires a clean parent checkout".to_string());
        }
        let spec_path = manifest_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if spec_path.starts_with(&repo) {
            return Err("evaluator spec must live outside the candidate checkout".to_string());
        }
        let spec_bytes = fs::read(&spec_path).map_err(|error| error.to_string())?;
        if spec_bytes.len() > 128 * 1024 {
            return Err("evaluator spec exceeds 128 KiB".to_string());
        }
        let mut spec: Spec =
            serde_json::from_slice(&spec_bytes).map_err(|error| error.to_string())?;
        let baseline_sha = git_text(&repo, &["rev-parse", "HEAD"])?;
        if spec.version != 1
            || spec.objective != brief
            || spec.baseline_sha != baseline_sha
            || spec.isolation_backend != "linux-bwrap"
            || spec.allowed_write_roots.is_empty()
            || spec.hard_gates.is_empty()
            || spec.hard_gates.len() > 12
            || spec.max_wall_secs == 0
            || spec.max_wall_secs > 3600
        {
            return Err(
                "evaluator spec, baseline or bounds differ from frozen request".to_string(),
            );
        }
        for root in &mut spec.allowed_write_roots {
            *root = normalized_path(root)?;
        }
        for root in &mut spec.protected_paths {
            *root = normalized_path(root)?;
        }
        if !spec
            .hard_gates
            .iter()
            .any(|gate| !gate.holdout && gate.min_tests > 0)
            || !spec
                .hard_gates
                .iter()
                .any(|gate| gate.holdout && gate.min_tests > 0)
        {
            return Err("public and held-out gates must both require nonzero cases".to_string());
        }
        let mut gate_cases = Vec::new();
        let mut case_hashes = Vec::new();
        for gate in &mut spec.hard_gates {
            if gate.candidate_argv.len() != 2
                || !Path::new(&gate.candidate_argv[0]).is_absolute()
                || gate.timeout_secs == 0
                || gate.timeout_secs > 1800
                || gate.min_tests == 0
            {
                return Err("invalid process-level gate command or bound".to_string());
            }
            let candidate_program = normalized_path(&gate.candidate_argv[1])?;
            if !spec
                .allowed_write_roots
                .iter()
                .any(|root| in_scope(&candidate_program, root))
            {
                return Err("candidate program is outside allowed write scope".to_string());
            }
            let cases_path = gate
                .cases_path
                .canonicalize()
                .map_err(|error| error.to_string())?;
            if cases_path.starts_with(&repo) {
                return Err("trusted cases must live outside candidate checkout".to_string());
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
            gate_cases.push(cases);
            case_hashes.push(hash(&bytes));
            gate.cases_path = cases_path;
        }
        let root = std::env::temp_dir().join(format!(
            "codewhale-evaluator-{run_id}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&root).map_err(|error| error.to_string())?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&root).map_err(|error| error.to_string())?;
        let frozen = (|| {
            let store = ArtifactStore::freeze(&repo, &root)?;
            let denied_paths = std::iter::once(spec_path.clone())
                .chain(spec.hard_gates.iter().map(|gate| gate.cases_path.clone()))
                .collect::<Vec<_>>();
            let sandbox_probe = probe_isolation(&root, &denied_paths)?;
            let binary_hash = hash(
                &fs::read(std::env::current_exe().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?,
            );
            let mut executable_hashes = HashMap::new();
            for executable in std::iter::once("/usr/bin/bwrap").chain(
                spec.hard_gates
                    .iter()
                    .map(|gate| gate.candidate_argv[0].as_str()),
            ) {
                let bytes = fs::read(executable).map_err(|error| error.to_string())?;
                executable_hashes.insert(executable.to_string(), hash(&bytes));
            }
            let manifest = json!({
                "version": 1,
                "run_id": run_id,
                "spec_sha256": hash(&spec_bytes),
                "baseline_sha": spec.baseline_sha,
                "baseline_id": store.baseline_id(),
                "public_fixture_sha256": case_hashes.iter().zip(&spec.hard_gates)
                    .filter(|(_, gate)| !gate.holdout)
                    .map(|(digest, _)| digest).collect::<Vec<_>>(),
                "oracle_sha256": case_hashes,
                "gate_policy_sha256": hash(&serde_json::to_vec(&spec.hard_gates)
                    .map_err(|error| error.to_string())?),
                "native_binary_sha256": binary_hash,
                "executable_sha256": executable_hashes,
                "sandbox_probe": sandbox_probe,
                "isolation_backend": spec.isolation_backend,
                "allowed_write_roots": spec.allowed_write_roots,
                "protected_paths": spec.protected_paths,
                "capture_policy": artifact::capture_policy(),
                "review_only": true,
            });
            let manifest_bytes =
                serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join("manifest.json"))
                .map_err(|error| error.to_string())?;
            file.write_all(&manifest_bytes)
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            Ok(Self {
                spec,
                spec_path,
                spec_hash: hash(&spec_bytes),
                manifest_hash: hash(&manifest_bytes),
                binary_hash,
                executable_hashes,
                gate_cases,
                case_hashes,
                store,
                sandbox_probe,
                root: root.clone(),
                repo,
                started: None,
                started_at_ms: None,
                evaluation_elapsed: Duration::ZERO,
                evaluated: HashMap::new(),
            })
        })();
        if frozen.is_err() {
            let _ = fs::remove_dir_all(&root);
        }
        frozen
    }

    fn verify_frozen_inputs(&self) -> Result<(), String> {
        self.store.verify_baseline()?;
        if hash(&fs::read(&self.spec_path).map_err(|error| error.to_string())?) != self.spec_hash
            || hash(&fs::read(self.root.join("manifest.json")).map_err(|error| error.to_string())?)
                != self.manifest_hash
            || hash(
                &fs::read(std::env::current_exe().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?,
            ) != self.binary_hash
        {
            return Err("frozen spec, manifest or evaluator binary changed".to_string());
        }
        for (gate, digest) in self.spec.hard_gates.iter().zip(&self.case_hashes) {
            if hash(&fs::read(&gate.cases_path).map_err(|error| error.to_string())?) != *digest {
                return Err("frozen oracle/fixture bundle changed".to_string());
            }
        }
        for (executable, digest) in &self.executable_hashes {
            if hash(&fs::read(executable).map_err(|error| error.to_string())?) != *digest {
                return Err(format!("frozen execution binary changed: {executable}"));
            }
        }
        Ok(())
    }

    pub(super) fn summary(&self) -> Value {
        json!({
            "manifest": self.root.join("manifest.json"),
            "run_root": self.root,
            "baseline_sha": self.spec.baseline_sha,
            "baseline_id": self.store.baseline_id(),
            "spec_sha256": self.spec_hash,
            "sandbox_probe": self.sandbox_probe,
            "capture_policy": artifact::capture_policy(),
            "evaluation_started_at_ms": self.started_at_ms,
            "evaluation_elapsed_ms": self.evaluation_elapsed.as_millis(),
            "review_only": true
        })
    }

    pub(super) fn evaluate(
        &mut self,
        id: &str,
        worktree: &Path,
        completed: bool,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        candidate_id(id)?;
        if !completed || cancel.is_cancelled() {
            return Err("candidate worker has not completed or run was cancelled".to_string());
        }
        if self.evaluated.contains_key(id)
            || self.root.join(format!("{id}-evaluation.json")).exists()
        {
            return Err("candidate already evaluated".to_string());
        }
        self.verify_frozen_inputs()?;
        let worktree = worktree.canonicalize().map_err(|error| error.to_string())?;
        if worktree == self.repo
            || git_text(&worktree, &["rev-parse", "HEAD"])? != self.spec.baseline_sha
        {
            return Err(
                "candidate worktree starting HEAD differs from frozen baseline".to_string(),
            );
        }
        let common_dir = |cwd: &Path| -> Result<PathBuf, String> {
            let raw = git_text(cwd, &["rev-parse", "--git-common-dir"])?;
            let path = PathBuf::from(raw);
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            path.canonicalize().map_err(|error| error.to_string())
        };
        if common_dir(&worktree)? != common_dir(&self.repo)? {
            return Err("candidate worktree does not belong to frozen repository".to_string());
        }
        self.begin_evaluation()?;
        let result = (|| {
            let artifact = self
                .store
                .capture(&worktree, id, Vec::new(), &self.spec.protected_paths)?;
            self.run_candidate(id, &artifact, false, cancel)
        })();
        self.finish_evaluation();
        if let Ok(receipt) = &result {
            self.evaluated.insert(id.to_string(), receipt.clone());
        }
        result
    }

    pub(super) fn replay(&mut self, id: &str, cancel: &CancellationToken) -> Result<Value, String> {
        candidate_id(id)?;
        if cancel.is_cancelled() {
            return Err("evaluation cancelled".to_string());
        }
        self.verify_frozen_inputs()?;
        let original = self
            .evaluated
            .get(id)
            .ok_or("missing original evaluation")?;
        if original.get("status").and_then(Value::as_str) != Some("PASS") {
            return Err("only a verified public candidate can be replayed".to_string());
        }
        let bytes = fs::read(self.root.join("artifacts").join(format!("{id}.json")))
            .map_err(|error| error.to_string())?;
        let digest = hash(&bytes);
        if original
            .get("artifact_manifest_sha256")
            .and_then(Value::as_str)
            != Some(digest.as_str())
        {
            return Err("sealed artifact manifest changed".to_string());
        }
        let artifact = self.store.load(id)?;
        for (key, expected) in [
            ("artifact_id", artifact.artifact_id.as_str()),
            ("candidate_tree_hash", artifact.candidate_tree_hash.as_str()),
            ("baseline_id", self.store.baseline_id()),
            ("baseline_sha", self.spec.baseline_sha.as_str()),
            ("spec_sha256", self.spec_hash.as_str()),
            ("frozen_manifest_sha256", self.manifest_hash.as_str()),
            ("native_binary_sha256", self.binary_hash.as_str()),
        ] {
            if original.get(key).and_then(Value::as_str) != Some(expected) {
                return Err(format!(
                    "original receipt {key} differs from frozen spec/artifact"
                ));
            }
        }
        if original.get("oracle_sha256") != Some(&json!(self.case_hashes))
            || original.get("executable_sha256") != Some(&json!(self.executable_hashes))
            || original.get("sandbox_probe") != Some(&self.sandbox_probe)
        {
            return Err(
                "original receipt oracle or execution identity differs from frozen spec"
                    .to_string(),
            );
        }
        self.begin_evaluation()?;
        let result = self.run_candidate(id, &artifact, true, cancel);
        self.finish_evaluation();
        result
    }

    fn begin_evaluation(&mut self) -> Result<(), String> {
        self.started_at_ms = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_millis() as u64,
        );
        self.started = Some(Instant::now());
        Ok(())
    }

    fn finish_evaluation(&mut self) {
        if let Some(started) = self.started.take() {
            self.evaluation_elapsed += started.elapsed();
        }
    }

    fn run_candidate(
        &self,
        id: &str,
        artifact: &Artifact,
        replay: bool,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        let suffix = if replay { "replay" } else { "evaluation" };
        let artifact_bytes = fs::read(self.root.join("artifacts").join(format!("{id}.json")))
            .map_err(|error| error.to_string())?;
        let artifact_manifest_sha256 = hash(&artifact_bytes);
        let scope = validate_scope(
            artifact,
            &self.spec.allowed_write_roots,
            &self.spec.protected_paths,
        );
        let mut status = "REJECTED";
        let mut execution_status = "NOT_STARTED";
        let mut reason = scope.as_ref().err().cloned();
        let mut gates = Vec::new();
        let mut test_count = 0u64;
        let mut public_feedback = String::new();
        let mut stop_reason = None;
        if scope.is_ok() {
            let execution = self.execute_gates(id, artifact, replay, cancel);
            match execution {
                Ok(outcome) => {
                    gates = outcome.receipts;
                    test_count = outcome.test_count;
                    public_feedback = outcome.feedback;
                    stop_reason = outcome.stop_reason;
                    execution_status = if stop_reason.is_some() {
                        "STOPPED"
                    } else {
                        "COMPLETE"
                    };
                    reason = outcome.failure.or_else(|| stop_reason.clone());
                    status = if stop_reason.is_some() {
                        "UNKNOWN"
                    } else if reason.is_none() && test_count > 0 {
                        "PASS"
                    } else {
                        "FAIL"
                    };
                }
                Err(error) => {
                    execution_status = "INCOMPLETE";
                    status = "UNKNOWN";
                    reason = Some(error);
                }
            }
        }
        let material = self.store.review_material(artifact)?;
        let review_complete = material.is_some();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis();
        let receipt_path = self.root.join(format!("{id}-{suffix}.json"));
        let receipt = json!({
            "version": 1,
            "candidate_id": id,
            "status": status,
            "execution_status": execution_status,
            "stop_reason": stop_reason,
            "reason": reason,
            "spec_sha256": self.spec_hash,
            "frozen_manifest_sha256": self.manifest_hash,
            "baseline_sha": self.spec.baseline_sha,
            "baseline_id": self.store.baseline_id(),
            "artifact_id": artifact.artifact_id,
            "candidate_tree_hash": artifact.candidate_tree_hash,
            "artifact_manifest_sha256": artifact_manifest_sha256,
            "oracle_sha256": self.case_hashes,
            "native_binary_sha256": self.binary_hash,
            "executable_sha256": self.executable_hashes,
            "sandbox_probe": self.sandbox_probe,
            "changed_paths": artifact.entries.iter().map(|entry| &entry.path).collect::<Vec<_>>(),
            "excluded_generated_paths": artifact.excluded_generated_paths,
            "capture_policy": artifact::capture_policy(),
            "gates": gates,
            "test_count": test_count,
            "public_failure_feedback": public_feedback,
            "review_material_sha256": material.as_ref().map(|text| hash(text.as_bytes())),
            "review_material_bytes": material.as_ref().map(String::len),
            "review_material": material,
            "review_complete": review_complete,
            "evaluation_started_at_ms": self.started_at_ms,
            "evaluation_elapsed_ms": self.evaluation_elapsed.as_millis() + self.started.map_or(0, |started| started.elapsed().as_millis()),
            "execution_deadline_at_ms": self.started_at_ms.map(|started| started + Duration::from_secs(self.spec.max_wall_secs).saturating_sub(self.evaluation_elapsed).as_millis() as u64),
            "receipt_path": receipt_path,
            "observed_at_ms": now,
            "review_only": true,
        });
        let bytes = serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt_path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(receipt)
    }

    fn execute_gates(
        &self,
        id: &str,
        artifact: &Artifact,
        replay: bool,
        cancel: &CancellationToken,
    ) -> Result<GateExecution, String> {
        let owned = tempfile::Builder::new()
            .prefix("candidate-")
            .tempdir_in(&self.root)
            .map_err(|error| error.to_string())?;
        let source = owned.path().join("source");
        self.store.materialize(artifact, &source)?;
        let mut receipts = Vec::new();
        let mut test_count = 0u64;
        let mut failure = None;
        let mut feedback = String::new();
        let mut stop_reason = None;
        let overall_started = self.started.ok_or("evaluation clock was not started")?;
        for (gate_index, gate) in self.spec.hard_gates.iter().enumerate() {
            if gate.holdout && !replay {
                continue;
            }
            let started = Instant::now();
            let mut passed_count = 0u64;
            let mut observed_count = 0u64;
            let mut logs = Vec::new();
            for (case_index, case) in self.gate_cases[gate_index].iter().enumerate() {
                let gate_remaining =
                    Duration::from_secs(gate.timeout_secs).saturating_sub(started.elapsed());
                let overall_remaining = Duration::from_secs(self.spec.max_wall_secs)
                    .saturating_sub(self.evaluation_elapsed + overall_started.elapsed());
                let reason = if cancel.is_cancelled() {
                    Some("cancelled")
                } else if overall_remaining.is_zero() {
                    Some("aggregate_evaluation_deadline")
                } else if gate_remaining.is_zero() {
                    Some("gate_timeout")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    stop_reason = Some(reason.to_string());
                    failure.get_or_insert_with(|| reason.to_string());
                    break;
                }
                let remaining = gate_remaining.min(overall_remaining);
                let scratch = owned
                    .path()
                    .join(format!("scratch-{gate_index}-{case_index}"));
                fs::create_dir(&scratch).map_err(|error| error.to_string())?;
                let program = &gate.candidate_argv[0];
                let args = vec![
                    source
                        .join(&gate.candidate_argv[1])
                        .to_string_lossy()
                        .into_owned(),
                ];
                let timeout = remaining.min(Duration::from_secs(8));
                let env = isolated_command(&source, &scratch, program, args, timeout)?;
                let log_name = format!(
                    "{id}-{}-g{gate_index}-c{case_index}",
                    if replay { "replay" } else { "evaluation" }
                );
                let stdout = self.root.join(format!("{log_name}.stdout.log"));
                let stderr = self.root.join(format!("{log_name}.stderr.log"));
                let input = serde_json::to_vec(&case.input).map_err(|error| error.to_string())?;
                let run = run_isolated(&env, &input, &stdout, &stderr, timeout, Some(cancel))?;
                observed_count += 1;
                test_count += 1;
                let actual = serde_json::from_str::<Value>(run.stdout.trim());
                let passed = run.exit_code == Some(0)
                    && run.stop_reason.is_none()
                    && actual
                        .as_ref()
                        .is_ok_and(|value| same_json(value, &case.expected));
                if passed {
                    passed_count += 1;
                } else {
                    failure.get_or_insert_with(|| {
                        format!("gate {gate_index} case {} failed", case_index + 1)
                    });
                    if !gate.holdout && feedback.len() < 2400 {
                        feedback.push_str(&format!(
                            "case {} input={} expected={} actual={} exit={:?} stop={:?} stderr={}\n",
                            case_index + 1, case.input, case.expected,
                            actual.unwrap_or_else(|_| Value::String(run.stdout.clone())),
                            run.exit_code, run.stop_reason,
                            run.stderr.chars().take(200).collect::<String>()
                        ).chars().take(2400 - feedback.len()).collect::<String>());
                    }
                }
                logs.push(json!({
                    "case": case_index + 1, "stdout": stdout, "stderr": stderr,
                    "sandbox_argv": env.command,
                    "exit_code": run.exit_code, "stop_reason": run.stop_reason,
                    "wall_ms": run.wall_ms, "passed": passed
                }));
                if run.stop_reason.is_some() {
                    stop_reason = Some(match run.stop_reason.as_deref() {
                        Some("timeout")
                            if self.evaluation_elapsed + overall_started.elapsed()
                                >= Duration::from_secs(self.spec.max_wall_secs) =>
                        {
                            "aggregate_evaluation_deadline".to_string()
                        }
                        Some("timeout") if started.elapsed() >= Duration::from_secs(gate.timeout_secs) => {
                            "gate_timeout".to_string()
                        }
                        Some("timeout") => "case_timeout".to_string(),
                        Some(reason) => reason.to_string(),
                        None => unreachable!(),
                    });
                    break;
                }
            }
            let gate_pass = observed_count == self.gate_cases[gate_index].len() as u64
                && passed_count == observed_count
                && passed_count >= gate.min_tests;
            receipts.push(json!({
                "candidate_argv": gate.candidate_argv,
                "cases_sha256": self.case_hashes[gate_index],
                "holdout": gate.holdout,
                "test_count": observed_count,
                "passed_count": passed_count,
                "min_tests": gate.min_tests,
                "status": if gate_pass { "PASS" } else { "FAIL" },
                "logs": logs
            }));
            if !gate_pass {
                if failure.is_none() {
                    failure = Some(format!(
                        "gate {gate_index} did not execute all required cases"
                    ));
                }
                break;
            }
        }
        Ok(GateExecution {
            receipts,
            test_count,
            failure,
            feedback,
            stop_reason,
        })
    }
}
