//! Trusted, nonexecuting DSH preparation. Runs only the embedded reviewer,
//! never a package-selected script. Called from the installer's blocking worker.
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const REVIEW: &[u8] = include_bytes!("../../extension-host/dist/dsh-composition-review.mjs");
const MAX_INPUT: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_STDERR: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);

/// Synchronous installer seam; the caller must run outside a Tokio worker.
/// No source value or reviewer stderr becomes a public diagnostic.
pub(crate) fn review(input: &Value) -> Result<Value, String> {
    let input = serde_json::to_vec(input).map_err(|_| "invalid composition request")?;
    if input.len() > MAX_INPUT {
        return Err("composition review request exceeds 8 MiB".into());
    }
    let manager = super::manager();
    let options = &manager.shared.options;
    let pin = manager.shared.runtime.lock().expect("runtime lock");
    let pinned = pin.pinned.clone();
    let bun_failed = pin.bun_failed;
    drop(pin);
    let root = super::host_root(options)?;
    let review_digest = super::hex(sha2::Sha256::digest(REVIEW));
    let dir = root
        .join("extension-host")
        .join(format!("review-{review_digest}"));
    let bundle = super::materialize_file(&dir, "dsh-composition-review.mjs", REVIEW)?;
    super::materialize_file(&dir, super::NOTICES_FILE_NAME, super::NOTICES)?;
    let runtime = match pinned {
        Some((runtime, _)) => runtime,
        None => {
            let choice = if bun_failed {
                codewhale_config::extension_host::ExtensionHostRuntime::Node
            } else {
                options.runtime
            };
            codewhale_dependencies::resolve_extension_host_runtime(
                choice,
                options.node_override.as_deref(),
                options.bun_override.as_deref(),
                crate::extension_host::bundle_sha256(),
            )
            .selected
            .ok_or("composition reviewer runtime is unavailable")?
        }
    };
    let launch = super::supervisor::plan_launch(
        super::tier::HostTier::Plugin,
        &runtime,
        &bundle,
        &root,
        options.supervision.memory_cap,
        &[],
    )?;
    // A bounded process runner, not another Engine or session runtime.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "composition review worker is unavailable")?
        .block_on(run(launch, input))
}

async fn bounded_read(pipe: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    pipe.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "composition reviewer pipe failed")?;
    if bytes.len() > limit {
        return Err("composition reviewer output exceeded its bound".into());
    }
    Ok(bytes)
}

async fn run(launch: super::supervisor::HostLaunch, input: Vec<u8>) -> Result<Value, String> {
    use std::process::Stdio;
    let mut command = tokio::process::Command::new(&launch.program);
    codewhale_util::utils::suppress_tokio_console_window(&mut command);
    command
        .args(&launch.args)
        .current_dir(&launch.cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let overrides = launch
        .sandbox_env
        .iter()
        .chain(&launch.runtime_env)
        .map(|(key, value)| (key.as_str(), value.as_str()));
    for (key, value) in
        codewhale_runtime::child_env::sanitized_plugin_mcp_env_from(std::env::vars_os(), overrides)
    {
        command.env(key, value);
    }
    #[cfg(unix)]
    command.process_group(0);
    super::supervisor::limit_child_memory(&mut command, launch.memory_cap);
    let mut child = command
        .spawn()
        .map_err(|_| "composition reviewer could not start")?;
    let tree = Arc::new(
        codewhale_runtime::process_tree::ProcessTree::attach_tokio(&child)
            .map_err(|_| "composition reviewer containment failed")?,
    );
    #[cfg(windows)]
    tree.limit_process_memory(launch.memory_cap)
        .map_err(|_| "composition reviewer memory cap failed")?;
    let pid = child
        .id()
        .ok_or("composition reviewer pid is unavailable")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("composition reviewer stdin is unavailable")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("composition reviewer stdout is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("composition reviewer stderr is unavailable")?;
    let feed = async move {
        stdin
            .write_all(&input)
            .await
            .map_err(|_| "composition reviewer input failed".to_string())?;
        stdin
            .shutdown()
            .await
            .map_err(|_| "composition reviewer input failed".to_string())?;
        // The reviewer parses a finite document. Closing the pipe is its
        // end-of-input signal; shutdown alone leaves this handle alive.
        drop(stdin);
        Ok::<(), String>(())
    };
    let work = async {
        let (status, output, _stderr, ()) = tokio::try_join!(
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| "composition reviewer wait failed".to_string())
            },
            bounded_read(stdout, MAX_OUTPUT),
            bounded_read(stderr, MAX_STDERR),
            feed,
        )?;
        if !status.success() {
            return Err("composition reviewer refused preparation".into());
        }
        serde_json::from_slice(&output)
            .map_err(|_| "composition reviewer returned invalid JSON".into())
    };
    let answer = tokio::select! {
        answer = tokio::time::timeout(DEADLINE,work) => answer.map_err(|_| "composition reviewer deadline elapsed")?,
        failure = monitor_memory(pid, launch.memory_cap) => { failure?; unreachable!() },
    };
    // Always reap descendants, including any that kept a pipe open; source
    // modules were never imported. Drop/timeout kills the same guarded tree.
    let _ = tree.kill();
    answer
}

use sha2::Digest;

async fn monitor_memory(pid: u32, cap: u64) -> Result<(), String> {
    let _ = (pid, cap);
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        #[cfg(target_os = "macos")]
        if super::supervisor::resident_bytes(pid).is_some_and(|bytes| bytes > cap) {
            return Err("composition reviewer exceeded its sampled memory cap".into());
        }
    }
}
