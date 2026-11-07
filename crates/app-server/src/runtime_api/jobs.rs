//! `/v1/jobs` — the client-facing shell job surface.
//!
//! One authority: every job lives on the thread's shared `ShellManager`, the
//! same manager the thread's engine uses for model-launched shell work. Jobs
//! created here carry an `api:{thread_id}` owner scope so an engine's
//! per-session completion drain never claims client-launched work as model
//! evidence — and the model's jobs never appear "client-owned" here.

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use codewhale_engine::runtime_threads::RuntimeThreadStore;
use codewhale_engine::runtime_threads::job_operations::{
    JobOperation, JobOperationRecord, JobOperationTransaction, JobOperationsBusy,
    request_fingerprint, validate_key,
};
use codewhale_engine::tools::shell::SharedShellManager;
use codewhale_engine::tools::shell::{
    PtyDimensions, ShellJobSnapshot, ShellLaunchNotStarted, ShellManager, ShellOutputChunk,
    ShellOutputStream, ShellResult, ShellStatus,
};

use super::{ApiError, RuntimeApiState, map_thread_err};

/// `owner_session_id` prefix for jobs launched through this API. A real
/// engine session id can never carry it, so `*_for_session` drains stay
/// model-owned and API listings can tell the two apart.
const API_JOB_SCOPE_PREFIX: &str = "api:";

const COMMAND_MAX_BYTES: usize = 32 * 1024;
const ENV_MAX_ENTRIES: usize = 64;
const ENV_KEY_MAX_BYTES: usize = 128;
const ENV_VALUE_MAX_BYTES: usize = 8 * 1024;
const OUTPUT_CHUNK_DEFAULT: usize = 64 * 1024;
const OUTPUT_CHUNK_MAX: usize = 512 * 1024;
const OUTPUT_WAIT_MAX_MS: u64 = 30_000;
const STDIN_MAX_BYTES: usize = 64 * 1024;
const JOB_ID_MAX_BYTES: usize = 128;

fn api_job_scope(thread_id: &str) -> String {
    format!("{API_JOB_SCOPE_PREFIX}{thread_id}")
}

fn job_owner(snapshot: &ShellJobSnapshot) -> &'static str {
    if snapshot.owner_agent_id.is_some() {
        "subagent"
    } else if snapshot.owner_session_id.starts_with(API_JOB_SCOPE_PREFIX) {
        "client"
    } else {
        "agent"
    }
}

fn map_job_err(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    if message.ends_with("not found") {
        ApiError::not_found(message)
    } else {
        ApiError::internal(message)
    }
}

#[derive(Debug, Serialize)]
pub(super) struct JobView {
    #[serde(flatten)]
    snapshot: ShellJobSnapshot,
    thread_id: String,
    /// `client` = launched through this API, `agent` = launched by the model's
    /// shell tool, `subagent` = owned by a delegated agent.
    owner: &'static str,
    /// Null for historical records whose original transport is not known.
    tty: Option<bool>,
    terminal_size: Option<PtyDimensions>,
}

impl JobView {
    fn new(snapshot: ShellJobSnapshot, thread_id: String, manager: &ShellManager) -> Self {
        let owner = job_owner(&snapshot);
        let terminal_size = manager.job_terminal_size(&snapshot.job_id);
        let tty = (!snapshot.stale).then_some(terminal_size.is_some());
        Self {
            snapshot,
            thread_id,
            owner,
            tty,
            terminal_size,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct JobListResponse {
    jobs: Vec<JobView>,
}

/// `GET /v1/jobs` — every live and known-stale job across all threads.
pub(super) async fn list_jobs(
    State(state): State<RuntimeApiState>,
) -> Result<Json<JobListResponse>, ApiError> {
    let managers = state.runtime_threads.shell_managers_snapshot().await;
    let jobs = tokio::task::spawn_blocking(move || {
        let mut jobs = Vec::new();
        for (thread_id, manager) in managers {
            let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
            jobs.extend(
                guard
                    .list_jobs()
                    .into_iter()
                    .map(|snapshot| JobView::new(snapshot, thread_id.clone(), &guard)),
            );
        }
        jobs
    })
    .await
    .map_err(|_| ApiError::internal("job listing failed"))?;
    Ok(Json(JobListResponse { jobs }))
}

async fn thread_manager(
    state: &RuntimeApiState,
    thread_id: &str,
    create: bool,
) -> Result<codewhale_engine::tools::shell::SharedShellManager, ApiError> {
    state
        .runtime_threads
        .thread_shell_manager(thread_id, create)
        .await
        .map_err(map_thread_err)?
        .ok_or_else(|| ApiError::not_found(format!("thread {thread_id} has no jobs")))
}

/// `GET /v1/threads/{id}/jobs` — all jobs owned by one thread's manager:
/// model-launched, subagent-launched, and client-launched together.
pub(super) async fn list_thread_jobs(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
) -> Result<Json<JobListResponse>, ApiError> {
    let Some(manager) = state
        .runtime_threads
        .thread_shell_manager(&thread_id, false)
        .await
        .map_err(map_thread_err)?
    else {
        return Ok(Json(JobListResponse { jobs: Vec::new() }));
    };
    let jobs = tokio::task::spawn_blocking(move || {
        let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .list_jobs()
            .into_iter()
            .map(|snapshot| JobView::new(snapshot, thread_id.clone(), &guard))
            .collect()
    })
    .await
    .map_err(|_| ApiError::internal("job listing failed"))?;
    Ok(Json(JobListResponse { jobs }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateJobRequest {
    command: String,
    /// Working directory. Omitted = the thread's workspace.
    cwd: Option<String>,
    /// Bounds the foreground-wait contract inside the manager; background
    /// jobs are never killed at timeout.
    timeout_ms: Option<u64>,
    /// Run under a PTY: stderr merges into stdout and the command sees a
    /// terminal. Required for interactive programs.
    #[serde(default)]
    tty: bool,
    #[serde(default)]
    env: HashMap<String, String>,
    operation_key: Option<String>,
    purpose: Option<String>,
    expected_store_id: Option<String>,
    expected_workspace: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct CreateJobResponse {
    job: Option<JobView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<JobOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_fingerprint: Option<String>,
}

pub(super) type JobManagers = Vec<(String, SharedShellManager)>;

pub(super) fn operation_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<JobOperationsBusy>().is_some() {
        ApiError::conflict("job operation admission is busy").with_code("job_operation_pending")
    } else {
        ApiError::internal("job operation storage unavailable")
    }
}

pub(super) fn project_operation(
    transaction: &JobOperationTransaction<'_>,
    record: &mut JobOperationRecord,
    managers: &JobManagers,
    generation: &str,
) -> Result<(JobOperation, Option<JobView>), ApiError> {
    let mut operation = record.operation.clone();
    let mut view = None;
    if !matches!(operation.status.as_str(), "completed" | "rejected") {
        operation.status = "unknown".to_string();
        if record.owner_generation == generation
            && let Some(job_id) = record.operation.job_id.clone()
            && let Some((_, manager)) = managers
                .iter()
                .find(|(thread, _)| thread == &operation.thread_id)
            && let Ok(mut manager) = manager.lock()
            && let Some(witness) = manager.job_receipt_witness(&job_id)
            && !witness.wait_failed
            && (witness.status == ShellStatus::Running || witness.terminal_confirmed)
            && witness.owner_session_id == api_job_scope(&operation.thread_id)
            && witness.cwd == operation.cwd
        {
            operation.status = if witness.status == ShellStatus::Running {
                "running"
            } else {
                "completed"
            }
            .to_string();
            if operation.status == "completed" {
                operation.job_status = Some(format!("{:?}", witness.status));
                operation.exit_code = witness.exit_code;
                operation.updated_at_ms = chrono::Utc::now()
                    .timestamp_millis()
                    .max(operation.created_at_ms);
                record.operation = operation.clone();
                transaction.save(record).map_err(operation_error)?;
                manager.release_job_receipt(&job_id);
            }
            if let Ok(detail) = manager.inspect_job(&job_id)
                && !detail.snapshot.stale
            {
                view = Some(JobView::new(
                    detail.snapshot,
                    operation.thread_id.clone(),
                    &manager,
                ));
            }
        }
    } else if record.owner_generation == generation
        && let Some(job_id) = operation.job_id.as_deref()
        && let Some((_, manager)) = managers
            .iter()
            .find(|(thread, _)| thread == &operation.thread_id)
        && let Ok(mut manager) = manager.lock()
        && let Ok(detail) = manager.inspect_job(job_id)
        && !detail.snapshot.stale
        && detail.snapshot.owner_session_id == api_job_scope(&operation.thread_id)
        && detail.snapshot.cwd == operation.cwd
    {
        view = Some(JobView::new(
            detail.snapshot,
            operation.thread_id.clone(),
            &manager,
        ));
    }
    Ok((operation, view))
}

fn observe_operation(
    store: RuntimeThreadStore,
    runtime_owner: std::sync::Weak<
        codewhale_engine::runtime_threads::manager::RuntimeThreadManager,
    >,
    manager: std::sync::Weak<std::sync::Mutex<ShellManager>>,
    thread_id: String,
    generation: String,
    id: String,
) {
    tokio::spawn(async move {
        let mut delay = std::time::Duration::from_millis(500);
        loop {
            tokio::time::sleep(delay).await;
            let store = store.clone();
            let runtime_owner = runtime_owner.clone();
            let manager = manager.clone();
            let thread_id = thread_id.clone();
            let generation = generation.clone();
            let id = id.clone();
            let result = tokio::task::spawn_blocking(move || {
                let Some(runtime_owner) = runtime_owner.upgrade() else {
                    return Ok(None);
                };
                if runtime_owner.job_operation_generation().ok().as_deref()
                    != Some(generation.as_str())
                {
                    return Ok(None);
                }
                let Some(manager) = manager.upgrade() else {
                    return Ok(None);
                };
                let managers = vec![(thread_id, manager)];
                store.with_job_operations(|transaction| {
                    let Some(mut record) = transaction.get(&id)? else {
                        return Ok(None);
                    };
                    let projected =
                        project_operation(transaction, &mut record, &managers, &generation)
                            .map_err(|_| anyhow::anyhow!("job observation unavailable"))?;
                    match projected.0.status.as_str() {
                        "running" => Ok(Some(true)),
                        "unknown" => {
                            let available =
                                record.operation.job_id.as_deref().is_some_and(|job_id| {
                                    managers[0].1.lock().ok().is_some_and(|mut manager| {
                                        manager.job_receipt_witness(job_id).is_some()
                                    })
                                });
                            Ok(available.then_some(false))
                        }
                        _ => Ok(None),
                    }
                })
            })
            .await;
            match result {
                Ok(Ok(Some(true))) => delay = std::time::Duration::from_millis(500),
                Ok(Ok(Some(false))) => delay = (delay * 2).min(std::time::Duration::from_secs(10)),
                Ok(Ok(None)) => break,
                _ => delay = (delay * 2).min(std::time::Duration::from_secs(10)),
            }
        }
    });
}

pub(super) fn overlaps(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[derive(Serialize)]
pub(super) struct JobOperationsResponse {
    operations: Vec<JobOperation>,
}

pub(super) async fn list_job_operations(
    State(state): State<RuntimeApiState>,
) -> Result<Json<JobOperationsResponse>, ApiError> {
    list_operations(&state, None).await
}

pub(super) async fn list_thread_job_operations(
    State(state): State<RuntimeApiState>,
    Path(thread): Path<String>,
) -> Result<Json<JobOperationsResponse>, ApiError> {
    state
        .runtime_threads
        .get_thread(&thread)
        .await
        .map_err(map_thread_err)?;
    list_operations(&state, Some(thread)).await
}

async fn list_operations(
    state: &RuntimeApiState,
    thread: Option<String>,
) -> Result<Json<JobOperationsResponse>, ApiError> {
    let managers = state.runtime_threads.shell_managers_snapshot().await;
    let generation = state
        .runtime_threads
        .job_operation_generation()
        .map_err(operation_error)?;
    let store = state.runtime_threads.job_operation_store();
    let operations = tokio::task::spawn_blocking(move || {
        store.with_job_operations(|transaction| {
            let mut operations = Vec::new();
            for mut record in transaction.list()? {
                if thread
                    .as_ref()
                    .is_none_or(|thread| thread == &record.operation.thread_id)
                {
                    let (operation, _) =
                        project_operation(transaction, &mut record, &managers, &generation)
                            .map_err(|_| anyhow::anyhow!("job operation projection failed"))?;
                    operations.push(operation);
                }
            }
            Ok(operations)
        })
    })
    .await
    .map_err(|_| ApiError::internal("job operation listing failed"))?
    .map_err(operation_error)?;
    Ok(Json(JobOperationsResponse { operations }))
}

pub(super) async fn get_job_operation(
    State(state): State<RuntimeApiState>,
    Path((thread, key)): Path<(String, String)>,
) -> Result<Json<CreateJobResponse>, ApiError> {
    validate_key(&key).map_err(|_| ApiError::bad_request("invalid job operation key"))?;
    state
        .runtime_threads
        .get_thread(&thread)
        .await
        .map_err(map_thread_err)?;
    let managers = state.runtime_threads.shell_managers_snapshot().await;
    let generation = state
        .runtime_threads
        .job_operation_generation()
        .map_err(operation_error)?;
    let store = state.runtime_threads.job_operation_store();
    let result = tokio::task::spawn_blocking(move || {
        store.with_job_operations(|transaction| {
            let id = transaction.operation_id(&thread, &key)?;
            let Some(mut record) = transaction.get(&id)? else {
                return Ok(None);
            };
            let result = project_operation(transaction, &mut record, &managers, &generation)
                .map_err(|_| anyhow::anyhow!("job operation projection failed"))?;
            Ok(Some((
                result.0,
                result.1,
                record.request_fingerprint.clone(),
            )))
        })
    })
    .await
    .map_err(|_| ApiError::internal("job operation lookup failed"))?
    .map_err(operation_error)?;
    let (operation, job, fingerprint) =
        result.ok_or_else(|| ApiError::not_found("job operation not found"))?;
    Ok(Json(CreateJobResponse {
        job,
        operation: Some(operation),
        request_fingerprint: Some(fingerprint),
    }))
}

/// Resolve a job `cwd` against the thread workspace, never the server's cwd.
/// Outside trust mode the directory must stay inside the workspace once
/// symlinks resolve. This is stricter than the shell tool's `resolve_path`:
/// the route has no `workspace_follow_symlinks` or `/trust add` roots and no
/// `~` expansion, so a symlink leading out of the workspace is refused.
///
/// The manager receives the canonical directory that was checked, not the
/// caller's spelling: a symlink retargeted between this check and the launch
/// cannot move the job elsewhere. The manager takes a `&str`, so a directory
/// whose name is not UTF-8 is refused rather than lossily renamed into a
/// different one. On Windows the `\\?\` verbatim prefix `canonicalize` adds
/// is dropped ([`plain_windows_path`]) because `cmd.exe` refuses it as a
/// current directory.
fn resolve_job_cwd(
    workspace: &std::path::Path,
    raw: &str,
    trust_mode: bool,
) -> Result<String, ApiError> {
    let requested = std::path::Path::new(raw);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    let not_a_directory = || ApiError::bad_request("cwd must be an existing directory");
    let resolved = std::fs::canonicalize(&candidate).map_err(|_| not_a_directory())?;
    if !std::fs::metadata(&resolved).is_ok_and(|meta| meta.is_dir()) {
        return Err(not_a_directory());
    }
    if !trust_mode {
        let root = std::fs::canonicalize(workspace)
            .map_err(|_| ApiError::internal("thread workspace is unavailable"))?;
        if !resolved.starts_with(&root) {
            return Err(ApiError::forbidden(
                "cwd must stay inside the thread workspace",
            ));
        }
    }
    let resolved = resolved
        .into_os_string()
        .into_string()
        .map_err(|_| ApiError::bad_request("cwd must resolve to a UTF-8 path"))?;
    Ok(if cfg!(windows) {
        plain_windows_path(&resolved)
    } else {
        resolved
    })
}

/// `\\?\C:\dir` -> `C:\dir` and `\\?\UNC\host\share` -> `\\host\share`; any
/// other spelling is returned unchanged. A string rule so every host tests it.
pub(super) fn plain_windows_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest)
            if rest.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && rest.as_bytes().get(1) == Some(&b':') =>
        {
            rest.to_string()
        }
        _ => path.to_string(),
    }
}

fn canonical_job_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let path = std::fs::canonicalize(path).ok()?;
    if cfg!(windows) {
        path.to_str()
            .map(|raw| std::path::PathBuf::from(plain_windows_path(raw)))
    } else {
        Some(path)
    }
}

/// `POST /v1/threads/{id}/jobs` — launch a client-owned background job under
/// the thread's own sandbox policy. The client asking is the approval; the
/// thread's posture still bounds what the job may touch.
pub(super) async fn create_thread_job(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
    Json(request): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<CreateJobResponse>), ApiError> {
    let command = request.command.trim();
    if command.is_empty() {
        return Err(ApiError::bad_request("command is required"));
    }
    if command.contains('\0') {
        return Err(ApiError::bad_request("command contains a NUL byte"));
    }
    if command.len() > COMMAND_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "command must be at most {COMMAND_MAX_BYTES} bytes"
        )));
    }
    if request.env.len() > ENV_MAX_ENTRIES {
        return Err(ApiError::bad_request(format!(
            "env may carry at most {ENV_MAX_ENTRIES} entries"
        )));
    }
    for (key, value) in &request.env {
        if key.is_empty() || key.len() > ENV_KEY_MAX_BYTES || key.contains(['=', '\0']) {
            return Err(ApiError::bad_request("invalid env key"));
        }
        if value.len() > ENV_VALUE_MAX_BYTES || value.contains('\0') {
            return Err(ApiError::bad_request("invalid env value"));
        }
    }
    #[cfg(target_env = "ohos")]
    if request.tty {
        return Err(ApiError::bad_request(
            "TTY shell mode is not supported on HarmonyOS/OpenHarmony",
        ));
    }

    let keyed = match (
        &request.operation_key,
        &request.purpose,
        &request.expected_store_id,
        &request.expected_workspace,
    ) {
        (None, None, None, None) => false,
        (Some(key), Some(purpose), Some(expected), Some(workspace)) => {
            if workspace.trim().is_empty() {
                return Err(ApiError::bad_request("expected_workspace is required"));
            }
            validate_key(key).map_err(|_| ApiError::bad_request("invalid job operation key"))?;
            if !matches!(purpose.as_str(), "setup" | "run" | "archive" | "command") {
                return Err(ApiError::bad_request("invalid job operation purpose"));
            }
            if expected != state.runtime_threads.job_operation_store_id() {
                return Err(ApiError::conflict("job operation store changed")
                    .with_code("job_operation_store_changed"));
            }
            true
        }
        _ => {
            return Err(ApiError::bad_request(
                "operation_key, purpose, expected_store_id and expected_workspace must be supplied together",
            ));
        }
    };
    let admission = state.runtime_threads.session_checkpoint_guard().await;
    let thread = state
        .runtime_threads
        .get_thread(&thread_id)
        .await
        .map_err(map_thread_err)?;
    let command = command.to_string();
    let fingerprint = if let Some(key) = request.operation_key.as_deref() {
        let env: BTreeMap<_, _> = request.env.iter().collect();
        let env: Vec<_> = env.into_iter().collect();
        let payload = serde_json::to_vec(&(
            2,
            &thread_id,
            &request.expected_workspace,
            &command,
            &request.cwd,
            request.timeout_ms.unwrap_or(120_000),
            request.tty,
            env,
            &request.purpose,
        ))
        .map_err(|_| ApiError::internal("job operation validation failed"))?;
        Some(
            request_fingerprint(key, &payload)
                .map_err(|_| ApiError::bad_request("invalid job operation key"))?,
        )
    } else {
        None
    };
    let generation = state
        .runtime_threads
        .job_operation_generation()
        .map_err(operation_error)?;
    let store = state.runtime_threads.job_operation_store();
    if let Some(key) = request.operation_key.clone() {
        let lookup_store = store.clone();
        let lookup_thread = thread_id.clone();
        let lookup_fingerprint = fingerprint.clone();
        let lookup_generation = generation.clone();
        let managers = state.runtime_threads.shell_managers_snapshot().await;
        let replay = tokio::task::spawn_blocking(move || {
            lookup_store.with_job_operations(|transaction| {
                let id = transaction.operation_id(&lookup_thread, &key)?;
                let Some(mut record) = transaction.get(&id)? else {
                    return Ok(None);
                };
                if Some(&record.request_fingerprint) != lookup_fingerprint.as_ref() {
                    return Ok(Some(Err(ApiError::conflict(
                        "job operation key belongs to a different request",
                    )
                    .with_code("job_operation_mismatch"))));
                }
                Ok(Some(
                    project_operation(transaction, &mut record, &managers, &lookup_generation).map(
                        |(operation, job)| (operation, job, record.request_fingerprint.clone()),
                    ),
                ))
            })
        })
        .await
        .map_err(|_| ApiError::internal("job operation lookup failed"))?
        .map_err(operation_error)?;
        if let Some(result) = replay {
            let (operation, job, fingerprint) = result?;
            return Ok((
                StatusCode::OK,
                Json(CreateJobResponse {
                    job,
                    operation: Some(operation),
                    request_fingerprint: Some(fingerprint),
                }),
            ));
        }
    }
    if !thread.allow_shell {
        return Err(ApiError::forbidden(
            "this thread does not allow shell commands",
        ));
    }
    let requested_workspace = thread.workspace.clone();
    let expected_workspace = request.expected_workspace.clone();
    let workspace = tokio::task::spawn_blocking(move || {
        let workspace = canonical_job_path(&requested_workspace)
            .ok_or_else(|| ApiError::internal("thread workspace is unavailable"))?;
        if expected_workspace.as_deref().is_some_and(|expected| {
            canonical_job_path(std::path::Path::new(expected)).as_ref() != Some(&workspace)
        }) {
            return Err(ApiError::conflict("job operation workspace changed")
                .with_code("job_operation_workspace_changed"));
        }
        Ok(workspace)
    })
    .await
    .map_err(|_| ApiError::internal("thread workspace resolution failed"))??;
    state
        .runtime_threads
        .validate_shell_access_policy(
            &workspace,
            state.config_path.as_deref(),
            state.config_profile.as_deref(),
        )
        .await
        .map_err(|error| ApiError::forbidden(error.to_string()))?;
    let external_backend = codewhale_sandbox::backend::SandboxKind::parse(
        state
            .runtime_threads
            .read_config()
            .sandbox_backend
            .as_deref()
            .unwrap_or("none"),
    ) != Some(codewhale_sandbox::backend::SandboxKind::None);
    if external_backend {
        return Err(ApiError::forbidden(
            "this server runs commands in an external sandbox backend, and a thread job starts a local process; run the command through the thread's shell tool instead",
        ));
    }
    let mut policy_thread = thread.clone();
    policy_thread.workspace = workspace.clone();
    let policy = state
        .runtime_threads
        .thread_job_sandbox_policy(&policy_thread)
        .await;
    let manager = thread_manager(&state, &thread_id, true).await?;
    let managers = state.runtime_threads.shell_managers_snapshot().await;
    let store_id = state.runtime_threads.job_operation_store_id().to_string();
    let runtime_owner = state.runtime_threads.clone();
    let scope = api_job_scope(&thread_id);
    let result = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        store.with_job_operations(|transaction| {
            Ok((|| -> Result<(StatusCode, CreateJobResponse), ApiError> {
                let id = request
                    .operation_key
                    .as_deref()
                    .map(|key| transaction.operation_id(&thread_id, key))
                    .transpose()
                    .map_err(operation_error)?;
                if let Some(id) = &id
                    && let Some(mut existing) = transaction.get(id).map_err(operation_error)?
                {
                    if Some(&existing.request_fingerprint) != fingerprint.as_ref() {
                        return Err(ApiError::conflict(
                            "job operation key belongs to a different request",
                        )
                        .with_code("job_operation_mismatch"));
                    }
                    let (operation, job) =
                        project_operation(transaction, &mut existing, &managers, &generation)?;
                    return Ok((
                        StatusCode::OK,
                        CreateJobResponse {
                            job,
                            operation: Some(operation),
                            request_fingerprint: Some(existing.request_fingerprint.clone()),
                        },
                    ));
                }
                if canonical_job_path(&thread.workspace).as_ref() != Some(&workspace)
                    || request
                        .expected_workspace
                        .as_deref()
                        .is_some_and(|expected| {
                            canonical_job_path(std::path::Path::new(expected)).as_ref()
                                != Some(&workspace)
                        })
                {
                    return Err(ApiError::conflict("job operation workspace changed")
                        .with_code("job_operation_workspace_changed"));
                }
                let raw_cwd = request.cwd.as_deref().unwrap_or(".");
                let cwd = resolve_job_cwd(&workspace, raw_cwd, thread.trust_mode)?;
                let cwd_path = std::path::PathBuf::from(&cwd);
                for mut record in transaction.list().map_err(operation_error)? {
                    let (operation, _) =
                        project_operation(transaction, &mut record, &managers, &generation)?;
                    if overlaps(&workspace, &operation.workspace)
                        || overlaps(&workspace, &operation.cwd)
                        || overlaps(&cwd_path, &operation.workspace)
                        || overlaps(&cwd_path, &operation.cwd)
                    {
                        if operation.status == "unknown" {
                            return Err(ApiError::conflict(
                                "an earlier job operation is unconfirmed",
                            )
                            .with_code("job_operation_unconfirmed"));
                        }
                        if keyed
                            && request.purpose.as_deref() != Some("command")
                            && operation.status == "running"
                            && operation.purpose != "command"
                        {
                            return Err(ApiError::conflict(
                                "a workspace script is already running",
                            )
                            .with_code("job_operation_busy"));
                        }
                    }
                }
                runtime_owner
                    .validate_job_operation_thread(&thread)
                    .map_err(|_| {
                        ApiError::conflict("job operation workspace or shell policy changed")
                            .with_code("job_operation_workspace_changed")
                    })?;
                if runtime_owner.job_operation_generation().ok().as_deref()
                    != Some(generation.as_str())
                {
                    return Err(ApiError::internal("job operation owner changed"));
                }
                let mut guard = manager
                    .lock()
                    .map_err(|_| ApiError::internal("job manager unavailable"))?;
                guard
                    .validate_background_job(
                        &command,
                        &cwd_path,
                        request.timeout_ms.unwrap_or(120_000),
                        &policy,
                        &request.env,
                    )
                    .map_err(|error| ApiError::forbidden(error.to_string()))?;
                if canonical_job_path(&thread.workspace).as_ref() != Some(&workspace)
                    || canonical_job_path(&cwd_path).as_ref() != Some(&cwd_path)
                {
                    return Err(ApiError::conflict("job operation workspace changed")
                        .with_code("job_operation_workspace_changed"));
                }
                let mut record = if let Some(id) = id {
                    let now = chrono::Utc::now().timestamp_millis();
                    let record = JobOperationRecord {
                        schema_version: 1,
                        operation: JobOperation {
                            id,
                            store_id,
                            thread_id: thread_id.clone(),
                            workspace,
                            cwd: cwd_path,
                            purpose: request.purpose.clone().expect("keyed purpose"),
                            status: "unknown".to_string(),
                            job_id: None,
                            job_status: None,
                            exit_code: None,
                            created_at_ms: now,
                            updated_at_ms: now,
                        },
                        request_fingerprint: fingerprint.expect("keyed fingerprint"),
                        owner_generation: generation.clone(),
                    };
                    transaction.save(&record).map_err(operation_error)?;
                    Some(record)
                } else {
                    None
                };
                if runtime_owner.job_operation_generation().ok().as_deref()
                    != Some(generation.as_str())
                {
                    return Err(ApiError::internal("job operation owner changed")
                        .with_code("job_operation_unconfirmed"));
                }
                runtime_owner
                    .validate_job_operation_thread(&thread)
                    .map_err(|_| {
                        ApiError::internal("job operation thread changed")
                            .with_code("job_operation_unconfirmed")
                    })?;
                let result = match guard.execute_with_options_env_for_session(
                    &command,
                    Some(&cwd),
                    request.timeout_ms.unwrap_or(120_000),
                    true,
                    None,
                    request.tty,
                    Some(policy),
                    request.env,
                    &scope,
                ) {
                    Ok(result) => result,
                    Err(error) if error.downcast_ref::<ShellLaunchNotStarted>().is_some() => {
                        if let Some(record) = &mut record {
                            record.operation.status = "rejected".to_string();
                            record.operation.updated_at_ms = chrono::Utc::now()
                                .timestamp_millis()
                                .max(record.operation.created_at_ms);
                            transaction.save(record).map_err(|_| {
                                ApiError::internal("job launch could not be confirmed")
                                    .with_code("job_operation_unconfirmed")
                            })?;
                            return Err(ApiError::conflict("job did not start")
                                .with_code("job_operation_rejected"));
                        }
                        return Err(ApiError::internal("job did not start"));
                    }
                    Err(_) => {
                        return Err(ApiError::internal("job launch could not be confirmed")
                            .with_code("job_operation_unconfirmed"));
                    }
                };
                let task_id = result.task_id.ok_or_else(|| {
                    ApiError::internal("job launch could not be confirmed")
                        .with_code("job_operation_unconfirmed")
                })?;
                if record.is_some() {
                    guard.pin_job_receipt(&task_id).map_err(|_| {
                        ApiError::internal("job launch could not be confirmed")
                            .with_code("job_operation_unconfirmed")
                    })?;
                }
                let snapshot = guard
                    .inspect_job(&task_id)
                    .map_err(|_| {
                        ApiError::internal("job launch could not be confirmed")
                            .with_code("job_operation_unconfirmed")
                    })?
                    .snapshot;
                let job = JobView::new(snapshot, thread_id.clone(), &guard);
                if let Some(record) = &mut record {
                    record.operation.job_id = Some(task_id);
                    record.operation.status = "running".to_string();
                    record.operation.updated_at_ms = chrono::Utc::now()
                        .timestamp_millis()
                        .max(record.operation.created_at_ms);
                    transaction.save(record).map_err(operation_error)?;
                    observe_operation(
                        store.clone(),
                        std::sync::Arc::downgrade(&runtime_owner),
                        std::sync::Arc::downgrade(&manager),
                        thread_id,
                        generation,
                        record.operation.id.clone(),
                    );
                }
                Ok((
                    StatusCode::CREATED,
                    CreateJobResponse {
                        job: Some(job),
                        request_fingerprint: record
                            .as_ref()
                            .map(|record| record.request_fingerprint.clone()),
                        operation: record.map(|record| record.operation),
                    },
                ))
            })())
        })
    })
    .await
    .map_err(|_| ApiError::internal("job launch failed"))?
    .map_err(operation_error)??;
    Ok((result.0, Json(result.1)))
}

#[derive(Debug, Serialize)]
pub(super) struct JobDetailResponse {
    job: JobView,
    stdout_tail: String,
    stderr_tail: String,
}

/// `GET /v1/threads/{id}/jobs/{job_id}` — snapshot plus the retained output
/// tails. For the full stream, follow `output` with a cursor instead.
pub(super) async fn get_thread_job(
    State(state): State<RuntimeApiState>,
    Path((thread_id, job_id)): Path<(String, String)>,
) -> Result<Json<JobDetailResponse>, ApiError> {
    if job_id.len() > JOB_ID_MAX_BYTES {
        return Err(ApiError::not_found("job not found"));
    }
    let manager = thread_manager(&state, &thread_id, false).await?;
    tokio::task::spawn_blocking(move || {
        let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        let detail = guard.inspect_job(&job_id).map_err(map_job_err)?;
        Ok(Json(JobDetailResponse {
            job: JobView::new(detail.snapshot, thread_id, &guard),
            stdout_tail: detail.stdout,
            stderr_tail: detail.stderr,
        }))
    })
    .await
    .map_err(|_| ApiError::internal("job inspect failed"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JobOutputQuery {
    /// `stdout` (default) or `stderr`; PTY jobs merge stderr into stdout.
    #[serde(default)]
    stream: Option<String>,
    /// Absolute byte offset into the stream's lifetime output.
    #[serde(default)]
    cursor: Option<usize>,
    /// Per-request byte ceiling, default 64 KiB, max 512 KiB.
    #[serde(default)]
    max_bytes: Option<usize>,
    /// Long-poll bound for new bytes on a running job, max 30s.
    #[serde(default)]
    wait_ms: Option<u64>,
    /// `base64` (default, exact bytes) or `text` (lossy UTF-8).
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct JobOutputResponse {
    job_id: String,
    stream: &'static str,
    /// Absolute offset of `data[0]`; exceeds `cursor` when the bounded buffer
    /// already discarded that prefix (`dropped` reports the cutoff).
    offset: usize,
    /// Next cursor: pass it back to continue the stream.
    next_cursor: usize,
    /// Total bytes the stream has produced, including discarded bytes.
    total: usize,
    /// Leading bytes permanently discarded by the in-flight bound.
    dropped: usize,
    encoding: &'static str,
    data: String,
    status: ShellStatus,
    exit_code: Option<i64>,
    /// Terminal status and no bytes remain past `next_cursor`.
    done: bool,
}

/// `GET /v1/threads/{id}/jobs/{job_id}/output` — the resumable byte stream.
/// Reads are non-consuming: several clients may hold independent cursors, and
/// polling here never steals output from the engine's own delta consumer.
pub(super) async fn get_thread_job_output(
    State(state): State<RuntimeApiState>,
    Path((thread_id, job_id)): Path<(String, String)>,
    Query(query): Query<JobOutputQuery>,
) -> Result<Json<JobOutputResponse>, ApiError> {
    if job_id.len() > JOB_ID_MAX_BYTES {
        return Err(ApiError::not_found("job not found"));
    }
    let (stream, stream_name) = match query.stream.as_deref().unwrap_or("stdout") {
        "stdout" => (ShellOutputStream::Stdout, "stdout"),
        "stderr" => (ShellOutputStream::Stderr, "stderr"),
        _ => return Err(ApiError::bad_request("stream must be stdout or stderr")),
    };
    let cursor = query.cursor.unwrap_or(0);
    let max_bytes = query.max_bytes.unwrap_or(OUTPUT_CHUNK_DEFAULT);
    if !(1..=OUTPUT_CHUNK_MAX).contains(&max_bytes) {
        return Err(ApiError::bad_request(format!(
            "max_bytes must be between 1 and {OUTPUT_CHUNK_MAX}"
        )));
    }
    let wait_ms = query.wait_ms.unwrap_or(0).min(OUTPUT_WAIT_MAX_MS);
    let format = query.format.as_deref().unwrap_or("base64");
    if !matches!(format, "base64" | "text") {
        return Err(ApiError::bad_request("format must be base64 or text"));
    }
    let manager = thread_manager(&state, &thread_id, false).await?;
    let chunk = tokio::task::spawn_blocking({
        let job_id = job_id.clone();
        move || -> Result<ShellOutputChunk, ApiError> {
            // Wait between non-consuming snapshots, never while owning the
            // thread's shared ShellManager. Input, resize, and kill stay live.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
            loop {
                let chunk = {
                    let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                    guard
                        .read_output_chunk(&job_id, stream, cursor, max_bytes, 0)
                        .map_err(map_job_err)?
                };
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if chunk.total > cursor
                    || chunk.status != ShellStatus::Running
                    || remaining.is_zero()
                {
                    break Ok(chunk);
                }
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(50)));
            }
        }
    })
    .await
    .map_err(|_| ApiError::internal("job output read failed"))??;
    Ok(Json(encode_chunk(&job_id, stream_name, chunk, format)))
}

fn encode_chunk(
    job_id: &str,
    stream_name: &'static str,
    chunk: ShellOutputChunk,
    format: &str,
) -> JobOutputResponse {
    let (encoding, data) = match format {
        "text" => ("utf-8", String::from_utf8_lossy(&chunk.bytes).into_owned()),
        _ => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(&chunk.bytes),
        ),
    };
    let done = chunk.status != ShellStatus::Running && chunk.next_offset >= chunk.total;
    JobOutputResponse {
        job_id: job_id.to_string(),
        stream: stream_name,
        offset: chunk.offset,
        next_cursor: chunk.next_offset,
        total: chunk.total,
        dropped: chunk.dropped,
        encoding,
        data,
        status: chunk.status,
        exit_code: chunk.exit_code,
        done,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JobStdinRequest {
    /// UTF-8 text (default) or base64 for arbitrary bytes.
    data: String,
    #[serde(default)]
    encoding: Option<String>,
    /// Close stdin after writing (EOF).
    #[serde(default)]
    close: bool,
}

/// `POST /v1/threads/{id}/jobs/{job_id}/stdin` — write to a running job's
/// stdin. Works for PTY and piped jobs alike.
pub(super) async fn write_thread_job_stdin(
    State(state): State<RuntimeApiState>,
    Path((thread_id, job_id)): Path<(String, String)>,
    Json(request): Json<JobStdinRequest>,
) -> Result<StatusCode, ApiError> {
    if job_id.len() > JOB_ID_MAX_BYTES {
        return Err(ApiError::not_found("job not found"));
    }
    let input = match request.encoding.as_deref().unwrap_or("utf-8") {
        "utf-8" => {
            if request.data.len() > STDIN_MAX_BYTES {
                return Err(ApiError::bad_request(format!(
                    "data must be at most {STDIN_MAX_BYTES} bytes"
                )));
            }
            request.data.into_bytes()
        }
        "base64" => {
            if request.data.len() > STDIN_MAX_BYTES * 2 {
                return Err(ApiError::bad_request("data exceeds the stdin limit"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&request.data)
                .map_err(|_| ApiError::bad_request("data is not valid base64"))?;
            if bytes.len() > STDIN_MAX_BYTES {
                return Err(ApiError::bad_request(format!(
                    "data must be at most {STDIN_MAX_BYTES} decoded bytes"
                )));
            }
            bytes
        }
        _ => return Err(ApiError::bad_request("encoding must be utf-8 or base64")),
    };
    let close = request.close;
    let manager = thread_manager(&state, &thread_id, false).await?;
    tokio::task::spawn_blocking(move || {
        let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .write_stdin_bytes(&job_id, &input, close)
            .map_err(map_job_err)
    })
    .await
    .map_err(|_| ApiError::internal("job stdin write failed"))??;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub(super) struct KillJobResponse {
    job: JobView,
    result: ShellResult,
}

/// `POST /v1/threads/{id}/jobs/{job_id}/kill` — bounded SIGTERM → SIGKILL
/// escalation on the whole process group; the final snapshot rides along.
pub(super) async fn kill_thread_job(
    State(state): State<RuntimeApiState>,
    Path((thread_id, job_id)): Path<(String, String)>,
) -> Result<Json<KillJobResponse>, ApiError> {
    if job_id.len() > JOB_ID_MAX_BYTES {
        return Err(ApiError::not_found("job not found"));
    }
    let admission = state.runtime_threads.session_checkpoint_guard().await;
    let manager = thread_manager(&state, &thread_id, false).await?;
    let store = state.runtime_threads.job_operation_store();
    let generation = state
        .runtime_threads
        .job_operation_generation()
        .map_err(operation_error)?;
    let runtime_owner = state.runtime_threads.clone();
    tokio::task::spawn_blocking(move || {
        let _admission = admission;
        if runtime_owner.job_operation_generation().ok().as_deref() != Some(generation.as_str()) {
            return Err(ApiError::internal("job operation owner changed"));
        }
        let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        let result = guard.kill(&job_id).map_err(map_job_err)?;
        guard.mark_cancel_reported(&result);
        let snapshot = guard.inspect_job(&job_id).map_err(map_job_err)?.snapshot;
        let pinned = guard.job_receipt_witness(&job_id).is_some();
        let job = JobView::new(snapshot, thread_id.clone(), &guard);
        drop(guard);
        if !pinned {
            return Ok(Json(KillJobResponse { job, result }));
        }
        let managers = vec![(thread_id.clone(), manager)];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let settlement = store.with_job_operations(|transaction| {
                let mut records: Vec<_> = transaction
                    .list()?
                    .into_iter()
                    .filter(|record| {
                        record.operation.thread_id == thread_id
                            && record.operation.job_id.as_deref() == Some(job_id.as_str())
                    })
                    .collect();
                anyhow::ensure!(records.len() <= 1, "duplicate job operation binding");
                let Some(mut record) = records.pop() else {
                    return Ok(false);
                };
                let (operation, _) =
                    project_operation(transaction, &mut record, &managers, &generation)
                        .map_err(|_| anyhow::anyhow!("job settlement unavailable"))?;
                Ok(operation.status == "completed")
            });
            match settlement {
                Ok(true) => return Ok(Json(KillJobResponse { job, result })),
                Ok(false) => {}
                Err(error) if error.downcast_ref::<JobOperationsBusy>().is_some() => {}
                Err(_) => {
                    return Err(ApiError::internal("job stop could not be confirmed")
                        .with_code("job_operation_unconfirmed"));
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(ApiError::internal("job stop could not be confirmed")
                    .with_code("job_operation_unconfirmed"));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    })
    .await
    .map_err(|_| ApiError::internal("job kill failed"))?
}

/// `POST /v1/threads/{id}/jobs/{job_id}/resize` — resize this existing PTY.
pub(super) async fn resize_thread_job(
    State(state): State<RuntimeApiState>,
    Path((thread_id, job_id)): Path<(String, String)>,
    Json(size): Json<PtyDimensions>,
) -> Result<Json<JobDetailResponse>, ApiError> {
    let size = size
        .validate()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if job_id.len() > JOB_ID_MAX_BYTES {
        return Err(ApiError::not_found("job not found"));
    }
    let manager = thread_manager(&state, &thread_id, false).await?;
    tokio::task::spawn_blocking(move || {
        let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        let detail = guard.inspect_job(&job_id).map_err(map_job_err)?;
        if detail.snapshot.stale || detail.snapshot.status != ShellStatus::Running {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                message: "This job is no longer running".into(),
                code: None,
            });
        }
        if guard.job_terminal_size(&job_id).is_none() {
            return Err(ApiError::bad_request("This job is not a PTY"));
        }
        guard.resize_pty(&job_id, size).map_err(map_job_err)?;
        let detail = guard.inspect_job(&job_id).map_err(map_job_err)?;
        Ok(Json(JobDetailResponse {
            job: JobView::new(detail.snapshot, thread_id, &guard),
            stdout_tail: detail.stdout,
            stderr_tail: detail.stderr,
        }))
    })
    .await
    .map_err(|_| ApiError::internal("PTY resize failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(path: &std::path::Path) -> String {
        let canonical = path.canonicalize().unwrap().to_string_lossy().into_owned();
        if cfg!(windows) {
            plain_windows_path(&canonical)
        } else {
            canonical
        }
    }

    #[test]
    fn job_cwd_resolves_against_the_thread_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("packages/app")).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        // Relative and absolute requests both come back as the canonical
        // directory that was checked, never the caller's spelling.
        let resolved = resolve_job_cwd(&workspace, "packages/app", false).unwrap();
        assert!(std::path::Path::new(&resolved).is_absolute(), "{resolved}");
        assert_eq!(resolved, canonical(&workspace.join("packages/app")));
        let absolute = workspace.join("packages");
        assert_eq!(
            resolve_job_cwd(&workspace, &absolute.to_string_lossy(), false).unwrap(),
            canonical(&absolute)
        );

        let missing = resolve_job_cwd(&workspace, "packages/none", false).unwrap_err();
        assert_eq!(missing.status, StatusCode::BAD_REQUEST);
        let outside_raw = outside.to_string_lossy().into_owned();
        for raw in [outside_raw.as_str(), "../outside"] {
            let error = resolve_job_cwd(&workspace, raw, false).unwrap_err();
            assert_eq!(error.status, StatusCode::FORBIDDEN, "{raw}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, workspace.join("escape")).unwrap();
            let error = resolve_job_cwd(&workspace, "escape", false).unwrap_err();
            assert_eq!(error.status, StatusCode::FORBIDDEN);
        }
        assert_eq!(
            resolve_job_cwd(&workspace, "../outside", true).unwrap(),
            canonical(&outside)
        );
    }

    /// The cwd handed to the launcher is the directory whose containment was
    /// checked: retargeting the requested symlink afterwards does not move
    /// the child out of the workspace.
    #[cfg(unix)]
    #[test]
    fn job_cwd_launches_in_the_checked_directory_after_a_retarget() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let inside = workspace.join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let link = workspace.join("link");
        std::os::unix::fs::symlink(&inside, &link).unwrap();

        let resolved = resolve_job_cwd(&workspace, "link", false).unwrap();
        assert_eq!(resolved, canonical(&inside));

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let output = std::process::Command::new("pwd")
            .arg("-P")
            .current_dir(&resolved)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            canonical(&inside)
        );
    }

    /// A directory whose name is not UTF-8 is refused before launch rather
    /// than lossily renamed into a `U+FFFD` sibling. Linux only: macOS and
    /// Windows filesystems do not store non-UTF-8 names.
    #[cfg(target_os = "linux")]
    #[test]
    fn job_cwd_refuses_a_directory_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let raw_name = std::ffi::OsStr::from_bytes(b"dir-\xff");
        std::fs::create_dir_all(workspace.join(raw_name)).unwrap();
        std::fs::create_dir_all(workspace.join("dir-\u{fffd}")).unwrap();
        std::os::unix::fs::symlink(workspace.join(raw_name), workspace.join("alias")).unwrap();

        let error = resolve_job_cwd(&workspace, "alias", false).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn plain_windows_path_drops_only_the_verbatim_prefix() {
        assert_eq!(plain_windows_path(r"\\?\C:\ws\app"), r"C:\ws\app");
        assert_eq!(
            plain_windows_path(r"\\?\UNC\host\share\ws"),
            r"\\host\share\ws"
        );
        for unchanged in [r"C:\ws", r"\\host\share", r"\\?\Volume{0}\ws", "/tmp/ws"] {
            assert_eq!(plain_windows_path(unchanged), unchanged);
        }
    }
}
