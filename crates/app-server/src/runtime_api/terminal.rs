//! `/v1/threads/{id}/terminals` — the Engine's terminal byte stream.
//!
//! The owner is [`codewhale_engine::tools::terminal_session`]: the same PTY-backed shell
//! the agent's terminal tools drive, so a client attaching here sees the
//! session the model is already working in rather than a second shell beside
//! it. These routes never create a session — an identity with no live session
//! is a 404, because conjuring a shell from an HTTP request would give the app
//! a terminal the Engine does not know about.
//!
//! A terminal is addressed by the thread that owns it and the identity the
//! Engine issued when the shell started (`terminal_id`). Every call resolves
//! the thread's recorded workspace again and answers only that thread's shells
//! in it, so another thread's or workspace's shell with the same name is never
//! reached. Reset and restart issue new identities; an old identity stays a
//! 404 instead of attaching to its replacement.
//!
//! Authentication is the `/v1` route layer's bearer token (see
//! [`super::auth`]); nothing here re-implements or bypasses it.
//!
//! Wire shape deliberately follows `/v1/threads/{id}/jobs/{job_id}/output`:
//! `cursor` / `max_bytes` / `format` in, `offset` / `next_cursor` / `total` /
//! `dropped` out. Two byte streams in one product should not speak two
//! dialects.
//!
//! Known limitations, recorded because a reader will otherwise assume them:
//!
//! - **No long poll.** `wait_ms` is not accepted; a client polls the cursor.
//!   The jobs route can block because a job owns a notification; a terminal
//!   session's ring has no wake-up channel yet, and inventing one here would
//!   be a second mechanism rather than a reuse.
//! - **No scrollback recovery.** `dropped` reports what the 512 KiB ring
//!   discarded; those bytes are gone with the process, not on disk.
//! - **Live sessions only.** Persistence is identity and lifecycle, never
//!   output, so a restarted Engine reports a new `instance` and none of the
//!   old identities rather than pretending to reattach (#34 acceptance:
//!   "Restart truthfully reports lost live PTYs").
//! - **Thread-owned shells only.** A shell started outside a runtime thread
//!   (the TUI dock, a headless run) has no route here.
//! - **Unix only.** The owner is `#[cfg(all(unix, not(target_env = "ohos")))]` end to end; on Windows these
//!   routes do not exist yet. ConPTY qualification is its own slice.

#[cfg(all(unix, not(target_env = "ohos")))]
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, Query, State};
#[cfg(all(unix, not(target_env = "ohos")))]
use base64::Engine as _;
use serde::{Deserialize, Serialize};

// The owner does not exist on ohos (`tools/mod.rs`), so neither does any
// handler that drives it; the stubs below answer there instead.
#[cfg(all(unix, not(target_env = "ohos")))]
use codewhale_engine::tools::terminal_session;

#[cfg(all(unix, not(target_env = "ohos")))]
use super::workspace::thread_workspace;
use super::{ApiError, RuntimeApiState};

/// Default per-response ceiling; the owner clamps to its own `READ_LIMIT`.
#[cfg(all(unix, not(target_env = "ohos")))]
const TERMINAL_CHUNK_DEFAULT: usize = 64 * 1024;
/// Identities are issued UUIDs; this only bounds the lookup.
#[cfg(all(unix, not(target_env = "ohos")))]
const TERMINAL_ID_MAX_BYTES: usize = 64;
/// One input frame. Interactive typing is bytes, not uploads.
#[cfg(all(unix, not(target_env = "ohos")))]
const TERMINAL_INPUT_MAX_BYTES: usize = 64 * 1024;
#[cfg(all(unix, not(target_env = "ohos")))]
const TERMINAL_DIMENSION_MAX: u16 = 1000;

// The request shape is the contract on every platform; only the Unix
// handlers read it, so the 501 builds expect the fields to stay unread.
#[cfg_attr(any(not(unix), target_env = "ohos"), expect(dead_code))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalOutputQuery {
    /// Absolute byte offset into the session's lifetime output.
    #[serde(default)]
    cursor: Option<u64>,
    /// Per-response byte ceiling, default 64 KiB, clamped by the owner.
    #[serde(default)]
    max_bytes: Option<usize>,
    /// `base64` (default, exact bytes) or `text` (lossy UTF-8).
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalView {
    terminal_id: String,
    name: String,
    state: String,
    running: bool,
    exit_code: Option<u32>,
    updated_at: String,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalListResponse {
    thread_id: String,
    workspace: std::path::PathBuf,
    instance: String,
    terminals: Vec<TerminalView>,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalOutputResponse {
    terminal_id: String,
    /// Absolute offset of `data[0]`; exceeds `cursor` when the ring already
    /// discarded that prefix (`dropped` reports the cutoff).
    offset: u64,
    /// Pass back as `cursor` to continue.
    next_cursor: u64,
    /// Everything the session has produced, including discarded bytes.
    total: u64,
    /// Leading bytes the bounded ring permanently discarded.
    dropped: u64,
    gap: bool,
    encoding: &'static str,
    data: String,
    /// False once the shell has exited and no bytes remain past `next_cursor`.
    running: bool,
    exit_code: Option<i64>,
}

#[cfg_attr(any(not(unix), target_env = "ohos"), expect(dead_code))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalInputRequest {
    data: String,
    /// `base64` (default, exact bytes) or `text`.
    #[serde(default)]
    encoding: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalWriteResponse {
    terminal_id: String,
    written: usize,
}

#[cfg_attr(any(not(unix), target_env = "ohos"), expect(dead_code))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalResizeRequest {
    rows: u16,
    cols: u16,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalResizeResponse {
    terminal_id: String,
    rows: u16,
    cols: u16,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalKillResponse {
    terminal_id: String,
    killed: bool,
}

/// `base64` keeps bytes exact; `text` is the lossy convenience form.
#[cfg(all(unix, not(target_env = "ohos")))]
fn chunk_encoding(format: &str) -> Result<&'static str, ApiError> {
    match format {
        "base64" => Ok("base64"),
        "text" => Ok("text"),
        _ => Err(ApiError::bad_request("format must be base64 or text")),
    }
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn encode_bytes(bytes: &[u8], encoding: &str) -> String {
    if encoding == "base64" {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn decode_bytes(data: &str, encoding: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = match encoding {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| ApiError::bad_request("data is not valid base64"))?,
        "text" => data.as_bytes().to_vec(),
        _ => return Err(ApiError::bad_request("encoding must be base64 or text")),
    };
    if bytes.len() > TERMINAL_INPUT_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "input exceeds {TERMINAL_INPUT_MAX_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn bounded_max_bytes(requested: Option<usize>) -> Result<usize, ApiError> {
    let max_bytes = requested.unwrap_or(TERMINAL_CHUNK_DEFAULT);
    if !(1..=terminal_session::READ_LIMIT).contains(&max_bytes) {
        return Err(ApiError::bad_request(format!(
            "max_bytes must be between 1 and {}",
            terminal_session::READ_LIMIT
        )));
    }
    Ok(max_bytes)
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn bounded_dimension(value: u16, field: &str) -> Result<u16, ApiError> {
    if !(1..=TERMINAL_DIMENSION_MAX).contains(&value) {
        return Err(ApiError::bad_request(format!(
            "{field} must be between 1 and {TERMINAL_DIMENSION_MAX}"
        )));
    }
    Ok(value)
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn terminal_not_found() -> ApiError {
    ApiError::not_found("no live terminal with this identity in this thread")
        .with_code("terminal_not_found")
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn lock(
    session: &terminal_session::SharedSession,
) -> Result<std::sync::MutexGuard<'_, terminal_session::TerminalSession>, ApiError> {
    session
        .lock()
        .map_err(|_| ApiError::internal("terminal session lock poisoned"))
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn state_label(state: terminal_session::DurableTerminalState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(all(unix, not(target_env = "ohos")))]
fn input_error(error: terminal_session::HumanInputError) -> ApiError {
    use terminal_session::HumanInputError;
    match &error {
        HumanInputError::Gone => terminal_not_found(),
        HumanInputError::Posture => {
            ApiError::forbidden(error.to_string()).with_code("terminal_posture_changed")
        }
        HumanInputError::Exited => {
            ApiError::conflict(error.to_string()).with_code("terminal_exited")
        }
        HumanInputError::Rejected(message) => ApiError::bad_request(message.clone()),
        HumanInputError::Failed(message) => ApiError::internal(message.clone()),
    }
}

/// How many terminal route calls may occupy blocking threads at once. The
/// rest wait in `with_terminal` asynchronously, so a client that disconnects
/// while queued simply disappears instead of holding a pool thread, and one
/// stuck write (a child that stopped reading, lock held) can stall terminal
/// routes but never the runtime's other blocking work.
#[cfg(all(unix, not(target_env = "ohos")))]
static ROUTE_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// Resolve one of the thread's live shells by identity and run one operation
/// against it on the blocking pool.
///
/// The session mutex and the PTY behind it are synchronous: the agent's own
/// tool holds the lock across a whole command, and a write to a child that
/// stopped reading blocks until the kernel buffer drains. Neither may park a
/// runtime worker (#6149), so a route never touches the session inline, and
/// `ROUTE_GATE` bounds how many such touches can be in flight.
#[cfg(all(unix, not(target_env = "ohos")))]
async fn with_terminal<T>(
    workspace: PathBuf,
    thread_id: String,
    terminal_id: String,
    operation: impl FnOnce(&terminal_session::SharedSession) -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
{
    if terminal_id.is_empty() || terminal_id.len() > TERMINAL_ID_MAX_BYTES {
        return Err(terminal_not_found());
    }
    let permit = ROUTE_GATE
        .acquire()
        .await
        .map_err(|_| ApiError::internal("terminal route gate closed"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let session = terminal_session::lookup(&workspace, Some(&thread_id), &terminal_id)
            .ok_or_else(terminal_not_found)?;
        operation(&session)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
}

/// `GET /v1/threads/{id}/terminals` — the thread's live shells, newest
/// activity first, with the registry `instance` they belong to.
#[cfg(all(unix, not(target_env = "ohos")))]
pub(super) async fn list_thread_terminals(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
) -> Result<Json<TerminalListResponse>, ApiError> {
    let (_, workspace) = thread_workspace(&state, &thread_id).await?;
    let permit = ROUTE_GATE
        .acquire()
        .await
        .map_err(|_| ApiError::internal("terminal route gate closed"))?;
    let scope = workspace.clone();
    let owner = thread_id.clone();
    let sessions = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        terminal_session::owned_sessions(&scope, &owner).map_err(ApiError::internal)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(TerminalListResponse {
        thread_id,
        workspace,
        instance: terminal_session::instance_id().to_string(),
        terminals: sessions
            .into_iter()
            .map(|info| TerminalView {
                running: info.exit_code.is_none(),
                terminal_id: info.id,
                name: info.name,
                state: if info.exit_code.is_some() {
                    "exited".to_string()
                } else {
                    state_label(info.state)
                },
                exit_code: info.exit_code,
                updated_at: info.updated_at,
            })
            .collect(),
    }))
}

/// `GET /v1/threads/{id}/terminals/{terminal_id}/output` — the resumable byte
/// stream.
///
/// Reads are non-consuming: several clients may hold independent cursors, and
/// polling here never steals output from the agent's own consuming read.
#[cfg(all(unix, not(target_env = "ohos")))]
pub(super) async fn terminal_output(
    State(state): State<RuntimeApiState>,
    Path((thread_id, terminal_id)): Path<(String, String)>,
    Query(query): Query<TerminalOutputQuery>,
) -> Result<Json<TerminalOutputResponse>, ApiError> {
    let encoding = chunk_encoding(query.format.as_deref().unwrap_or("base64"))?;
    let max_bytes = bounded_max_bytes(query.max_bytes)?;
    let cursor = query.cursor.unwrap_or(0);
    let (_, workspace) = thread_workspace(&state, &thread_id).await?;
    let (chunk, exit) = with_terminal(workspace, thread_id, terminal_id.clone(), move |session| {
        let mut session = lock(session)?;
        let chunk = terminal_session::read_session_since(&session, cursor, max_bytes)
            .map_err(ApiError::internal)?;
        let exit =
            terminal_session::session_exit_status(&mut session).map_err(ApiError::internal)?;
        Ok((chunk, exit))
    })
    .await?;
    let running = exit.is_none();
    Ok(Json(TerminalOutputResponse {
        terminal_id,
        offset: chunk.offset,
        next_cursor: chunk.next_cursor,
        total: chunk.total,
        dropped: chunk.dropped,
        gap: chunk.gap,
        encoding,
        data: encode_bytes(&chunk.bytes, encoding),
        // A gap means bytes were lost; `running` alone must not imply there is
        // nothing behind us, so drain state is reported independently.
        running,
        exit_code: exit.map(|status| i64::from(status.exit_code())),
    }))
}

/// `POST /v1/threads/{id}/terminals/{terminal_id}/input` — bytes into the live
/// shell. The thread's shell consent and shell policy apply, and so does the
/// sandbox-reuse guard the TUI dock applies to a person's input.
///
/// Input attribution is the caller's: this route is the client's writer, and
/// the agent's writer is `terminal_send`. Nothing here re-labels one as the
/// other.
#[cfg(all(unix, not(target_env = "ohos")))]
pub(super) async fn terminal_input(
    State(state): State<RuntimeApiState>,
    Path((thread_id, terminal_id)): Path<(String, String)>,
    Json(request): Json<TerminalInputRequest>,
) -> Result<Json<TerminalWriteResponse>, ApiError> {
    let bytes = decode_bytes(
        &request.data,
        request.encoding.as_deref().unwrap_or("base64"),
    )?;
    let (thread, workspace) = thread_workspace(&state, &thread_id).await?;
    if !thread.allow_shell {
        return Err(
            ApiError::forbidden("this thread does not allow shell commands")
                .with_code("thread_shell_disabled"),
        );
    }
    state
        .runtime_threads
        .validate_shell_access_policy(
            &workspace,
            state.config_path.as_deref(),
            state.config_profile.as_deref(),
        )
        .await
        .map_err(|error| ApiError::forbidden(error.to_string()))?;
    let policy = state
        .runtime_threads
        .thread_job_sandbox_policy(&thread)
        .await;
    let written = bytes.len();
    with_terminal(workspace, thread_id, terminal_id.clone(), move |session| {
        terminal_session::write_human_input(session, &bytes, &policy).map_err(input_error)
    })
    .await?;
    Ok(Json(TerminalWriteResponse {
        terminal_id,
        written,
    }))
}

/// `POST /v1/threads/{id}/terminals/{terminal_id}/resize` — the window the
/// child should draw for.
#[cfg(all(unix, not(target_env = "ohos")))]
pub(super) async fn terminal_resize(
    State(state): State<RuntimeApiState>,
    Path((thread_id, terminal_id)): Path<(String, String)>,
    Json(request): Json<TerminalResizeRequest>,
) -> Result<Json<TerminalResizeResponse>, ApiError> {
    let rows = bounded_dimension(request.rows, "rows")?;
    let cols = bounded_dimension(request.cols, "cols")?;
    let (_, workspace) = thread_workspace(&state, &thread_id).await?;
    with_terminal(workspace, thread_id, terminal_id.clone(), move |session| {
        let session = lock(session)?;
        terminal_session::resize_session(&session, rows, cols).map_err(ApiError::internal)
    })
    .await?;
    Ok(Json(TerminalResizeResponse {
        terminal_id,
        rows,
        cols,
    }))
}

/// `POST /v1/threads/{id}/terminals/{terminal_id}/kill` — end the shell.
///
/// The exit itself is observed through `output` (`running` / `exit_code`),
/// so a client that kills and then polls learns the truth instead of an
/// optimistic acknowledgement. `killed` is false when the shell had already
/// exited.
#[cfg(all(unix, not(target_env = "ohos")))]
pub(super) async fn terminal_kill(
    State(state): State<RuntimeApiState>,
    Path((thread_id, terminal_id)): Path<(String, String)>,
) -> Result<Json<TerminalKillResponse>, ApiError> {
    let (_, workspace) = thread_workspace(&state, &thread_id).await?;
    let killed = with_terminal(workspace, thread_id, terminal_id.clone(), |session| {
        let mut session = lock(session)?;
        if terminal_session::session_exit_status(&mut session)
            .map_err(ApiError::internal)?
            .is_some()
        {
            return Ok(false);
        }
        terminal_session::kill_session(&mut session).map_err(ApiError::internal)?;
        Ok(true)
    })
    .await?;
    Ok(Json(TerminalKillResponse {
        terminal_id,
        killed,
    }))
}

/// `/v1/terminal/{name}/*` addressed a shell by a reusable name at the
/// server's workspace, which could reach another thread's or workspace's
/// shell. It answers 410 on every platform so a client built for it fails
/// loudly instead of attaching to the wrong shell.
pub(super) async fn retired_terminal_route() -> ApiError {
    ApiError::gone(
        "name-addressed terminal routes were retired; list GET /v1/threads/{id}/terminals and address a terminal by its terminal_id",
    )
    .with_code("terminal_route_retired")
}

/// Windows build: the owner is `#[cfg(all(unix, not(target_env = "ohos")))]` end to end, so the contract
/// exists but cannot be served. These answer 501 rather than 404 so a client
/// can tell "this Engine build cannot do terminals" apart from "that session
/// is gone" — and so the ConPTY slice has one place to replace.
#[cfg(any(not(unix), target_env = "ohos"))]
mod platform {
    use super::*;

    fn unsupported() -> ApiError {
        ApiError::not_implemented(
            "terminal sessions are Unix-only in this build; native Windows PTY support is not implemented yet",
        )
    }

    pub(crate) async fn list_thread_terminals(
        State(_): State<RuntimeApiState>,
        Path(_): Path<String>,
    ) -> Result<Json<TerminalListResponse>, ApiError> {
        Err(unsupported())
    }

    pub(crate) async fn terminal_output(
        State(_): State<RuntimeApiState>,
        Path(_): Path<(String, String)>,
        Query(_): Query<TerminalOutputQuery>,
    ) -> Result<Json<TerminalOutputResponse>, ApiError> {
        Err(unsupported())
    }

    pub(crate) async fn terminal_input(
        State(_): State<RuntimeApiState>,
        Path(_): Path<(String, String)>,
        Json(_): Json<TerminalInputRequest>,
    ) -> Result<Json<TerminalWriteResponse>, ApiError> {
        Err(unsupported())
    }

    pub(crate) async fn terminal_resize(
        State(_): State<RuntimeApiState>,
        Path(_): Path<(String, String)>,
        Json(_): Json<TerminalResizeRequest>,
    ) -> Result<Json<TerminalResizeResponse>, ApiError> {
        Err(unsupported())
    }

    pub(crate) async fn terminal_kill(
        State(_): State<RuntimeApiState>,
        Path(_): Path<(String, String)>,
    ) -> Result<Json<TerminalKillResponse>, ApiError> {
        Err(unsupported())
    }
}

#[cfg(any(not(unix), target_env = "ohos"))]
pub(super) use platform::{
    list_thread_terminals, terminal_input, terminal_kill, terminal_output, terminal_resize,
};

#[cfg(all(test, unix, not(target_env = "ohos")))]
mod tests {
    use super::*;

    #[test]
    fn encodings_round_trip_exact_bytes_and_stay_lossy_only_on_request() {
        // Non-UTF-8 bytes survive base64 and are the reason it is the default.
        let raw = [0xf0, 0x9f, 0x90, 0x8b, 0x00, 0xff];
        let encoded = encode_bytes(&raw, "base64");
        assert_eq!(decode_bytes(&encoded, "base64").unwrap(), raw);
        // The lossy form is explicit and cannot be mistaken for fidelity.
        let text = encode_bytes(&raw, "text");
        assert!(text.contains('\u{fffd}'));
        assert_eq!(decode_bytes(&text, "text").unwrap(), text.as_bytes());
    }

    #[test]
    fn encoding_names_are_closed_sets() {
        for good in ["base64", "text"] {
            assert_eq!(chunk_encoding(good).unwrap(), good);
        }
        for bad in ["utf8", "raw", "Base64", ""] {
            assert!(chunk_encoding(bad).is_err(), "{bad} must not be accepted");
            assert!(decode_bytes("", bad).is_err(), "{bad} must not decode");
        }
        // A base64 decoder that ignores padding would accept junk bytes.
        assert!(decode_bytes("not base64!!", "base64").is_err());
    }

    #[test]
    fn chunk_and_dimension_bounds_reject_the_edges() {
        assert_eq!(bounded_max_bytes(None).unwrap(), TERMINAL_CHUNK_DEFAULT);
        assert_eq!(
            bounded_max_bytes(Some(terminal_session::READ_LIMIT)).unwrap(),
            terminal_session::READ_LIMIT
        );
        assert!(bounded_max_bytes(Some(0)).is_err());
        assert!(bounded_max_bytes(Some(terminal_session::READ_LIMIT + 1)).is_err());
        assert_eq!(bounded_dimension(24, "rows").unwrap(), 24);
        assert!(bounded_dimension(0, "rows").is_err());
        assert!(bounded_dimension(TERMINAL_DIMENSION_MAX + 1, "cols").is_err());
    }

    #[test]
    fn input_is_bounded_before_it_reaches_the_pty() {
        let too_much =
            base64::engine::general_purpose::STANDARD
                .encode(vec![b'a'; TERMINAL_INPUT_MAX_BYTES + 1]);
        assert!(decode_bytes(&too_much, "base64").is_err());
        let at_limit =
            base64::engine::general_purpose::STANDARD.encode(vec![b'a'; TERMINAL_INPUT_MAX_BYTES]);
        assert_eq!(
            decode_bytes(&at_limit, "base64").unwrap().len(),
            TERMINAL_INPUT_MAX_BYTES
        );
    }
}
