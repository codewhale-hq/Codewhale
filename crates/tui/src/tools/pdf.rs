//! Shared PDF-to-text adapter.
//!
//! PDF parsing is intentionally delegated to the optional `pdftotext`
//! executable. Keeping the adapter here gives file and web tools one error
//! contract without carrying a second parser and font stack in Codewhale.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use super::spec::ToolError;

const PDF_TEXT_TIMEOUT: Duration = Duration::from_secs(30);
const PDF_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_PDF_STDOUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PDF_STDERR_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PdfTextError {
    BinaryUnavailable,
    Cancelled,
    TimedOut,
    Execution(String),
}

impl fmt::Display for PdfTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BinaryUnavailable => formatter.write_str(
                "PDF text extraction requires the optional `pdftotext` executable (Poppler)",
            ),
            Self::Cancelled => formatter.write_str("PDF text extraction was cancelled"),
            Self::TimedOut => write!(
                formatter,
                "PDF text extraction timed out after {} seconds",
                PDF_TEXT_TIMEOUT.as_secs()
            ),
            Self::Execution(message) => formatter.write_str(message),
        }
    }
}

/// One typed mapping shared by local-file and fetched-PDF consumers.
///
/// The missing-binary message is deliberately a small JSON object. The
/// `NotAvailable` variant gives the runtime a failed terminal status while
/// callers that inspect the variant retain machine-readable recovery data.
pub(super) fn into_tool_error(error: PdfTextError) -> ToolError {
    match error {
        PdfTextError::BinaryUnavailable => ToolError::not_available(
            json!({
                "type": "binary_unavailable",
                "kind": "pdf",
                "binary": "pdftotext",
                "reason": "optional pdftotext executable is not installed",
                "hint": "install Poppler and ensure pdftotext is on PATH"
            })
            .to_string(),
        ),
        PdfTextError::Cancelled => ToolError::cancelled("PDF text extraction was cancelled"),
        PdfTextError::TimedOut => ToolError::Timeout {
            seconds: PDF_TEXT_TIMEOUT.as_secs(),
        },
        PdfTextError::Execution(message) => ToolError::execution_failed(message),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PdfTextCommand<'a> {
    binary: &'a OsStr,
    timeout: Duration,
    cancel: Option<&'a CancellationToken>,
    context: Option<&'a super::spec::ToolContext>,
}

impl<'a> PdfTextCommand<'a> {
    pub(super) fn context(self) -> Option<&'a super::spec::ToolContext> {
        self.context
    }
    pub(super) fn system(context: Option<&'a super::spec::ToolContext>) -> Self {
        Self {
            binary: OsStr::new("pdftotext"),
            timeout: PDF_TEXT_TIMEOUT,
            cancel: context.and_then(|context| context.cancel_token.as_ref()),
            context,
        }
    }

    #[cfg(test)]
    pub(super) fn test(
        binary: &'a OsStr,
        timeout: Duration,
        cancel: Option<&'a CancellationToken>,
    ) -> Self {
        Self {
            binary,
            timeout,
            cancel,
            context: None,
        }
    }

    #[cfg(all(test, unix))]
    pub(super) fn with_context(mut self, context: &'a super::spec::ToolContext) -> Self {
        self.context = Some(context);
        if self.cancel.is_none() {
            self.cancel = context.cancel_token.as_ref();
        }
        self
    }
}

pub(super) async fn extract_path(
    path: &Path,
    page_range: Option<(u32, u32)>,
    command: PdfTextCommand<'_>,
) -> Result<String, PdfTextError> {
    extract_captured(
        CapturedPdf {
            binary: command.binary.to_os_string(),
            path: path.to_path_buf(),
            page_range,
            timeout: command.timeout,
            _staged: None,
        },
        command,
    )
    .await
}

pub(super) async fn extract_bytes(
    bytes: &[u8],
    command: PdfTextCommand<'_>,
) -> Result<String, PdfTextError> {
    let mut input = tempfile::NamedTempFile::new().map_err(|error| {
        PdfTextError::Execution(format!("failed to stage fetched PDF: {error}"))
    })?;
    input.write_all(bytes).map_err(|error| {
        PdfTextError::Execution(format!("failed to stage fetched PDF: {error}"))
    })?;
    input.flush().map_err(|error| {
        PdfTextError::Execution(format!("failed to stage fetched PDF: {error}"))
    })?;
    let path = input.path().to_path_buf();
    extract_captured(
        CapturedPdf {
            binary: command.binary.to_os_string(),
            path,
            page_range: None,
            timeout: command.timeout,
            _staged: Some(input),
        },
        command,
    )
    .await
}

/// Private admitted parser request. No command, path or staged PDF crosses IPC.
pub(crate) struct CapturedPdf {
    pub(crate) binary: OsString,
    pub(crate) path: PathBuf,
    pub(crate) page_range: Option<(u32, u32)>,
    pub(crate) timeout: Duration,
    _staged: Option<tempfile::NamedTempFile>,
}
impl CapturedPdf {
    pub(crate) fn digest(&self) -> String {
        let mut bytes = self.binary.as_encoded_bytes().to_vec();
        bytes.push(0);
        bytes.extend_from_slice(self.path.as_os_str().as_encoded_bytes());
        bytes.push(0);
        if let Some((start, end)) = self.page_range {
            bytes.extend_from_slice(&start.to_le_bytes());
            bytes.extend_from_slice(&end.to_le_bytes());
        }
        codewhale_runtime::hashing::sha256_hex(&bytes)
    }
}

/// The existing bounded process result stays private in one Execution job.
pub(crate) struct PdfProcessOutcome {
    output: Result<(BoundedOutput, BoundedOutput, std::process::ExitStatus), PdfTextError>,
}
impl PdfProcessOutcome {
    pub(crate) fn projection(&self) -> serde_json::Value {
        match &self.output {
            Ok((stdout, stderr, status)) => json!({"kind":"pdf_process","state":"complete",
                "success":status.success(),"exit_code":status.code(),
                "stdout_truncated":stdout.truncated,"stderr":sanitized_text(&stderr.bytes),
                "stderr_truncated":stderr.truncated}),
            Err(error) => json!({"kind":"pdf_process","state":match error {
                PdfTextError::BinaryUnavailable=>"binary_unavailable", PdfTextError::Cancelled=>"cancelled",
                PdfTextError::TimedOut=>"timed_out", PdfTextError::Execution(_)=>"execution",
            },"message":error.to_string()}),
        }
    }
    fn into_rust_text(self) -> Result<String, PdfTextError> {
        let (stdout, stderr, status) = self.output?;
        if stdout.truncated {
            return Err(PdfTextError::Execution(format!(
                "pdftotext output exceeded the {} byte safety limit",
                MAX_PDF_STDOUT_BYTES
            )));
        }
        if !status.success() {
            let suffix = if stderr.truncated { " [truncated]" } else { "" };
            let stderr = sanitized_text(&stderr.bytes);
            let stderr = if stderr.is_empty() {
                "no diagnostic output".to_string()
            } else {
                stderr
            };
            return Err(PdfTextError::Execution(format!(
                "pdftotext failed (exit {:?}): {stderr}{suffix}",
                status.code()
            )));
        }
        Ok(String::from_utf8_lossy(&stdout.bytes).into_owned())
    }
    fn into_host_text(self, result: super::spec::ToolResult) -> Result<String, PdfTextError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Decision {
            kind: String,
            code: String,
            message: Option<String>,
        }
        let invalid = || {
            PdfTextError::Execution("PDF host returned a malformed or inconsistent decision; no Rust fallback was attempted".into())
        };
        if !result.success {
            return Err(invalid());
        }
        let decision: Decision =
            serde_json::from_value(result.metadata.ok_or_else(invalid)?).map_err(|_| invalid())?;
        if decision.kind != "pdf_decision" {
            return Err(invalid());
        }
        match self.output {
            // Cancellation/unavailability keep their real typed terminal status.
            Err(error) => {
                let code = match &error {
                    PdfTextError::BinaryUnavailable => "binary_unavailable",
                    PdfTextError::Cancelled => "cancelled",
                    PdfTextError::TimedOut => "timed_out",
                    PdfTextError::Execution(_) => "execution",
                };
                if decision.code != code {
                    return Err(invalid());
                }
                Err(error)
            }
            Ok((stdout, _, status)) => match decision.code.as_str() {
                // Mandatory data-loss/process-success guard remains Core-owned.
                "success"
                    if !stdout.truncated && status.success() && decision.message.is_none() =>
                {
                    Ok(String::from_utf8_lossy(&stdout.bytes).into_owned())
                }
                "execution" if stdout.truncated || !status.success() => Err(
                    PdfTextError::Execution(decision.message.ok_or_else(invalid)?),
                ),
                _ => Err(invalid()),
            },
        }
    }
}

async fn extract_captured(
    input: CapturedPdf,
    request: PdfTextCommand<'_>,
) -> Result<String, PdfTextError> {
    if request.cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(PdfTextError::Cancelled);
    }
    if let Some(context) = request.context.filter(|context| {
        context
            .features
            .enabled(codewhale_runtime::features::Feature::PdfHost)
    }) {
        // Capture the existing Core deadline before entering the broker. An
        // expired job's final receipt check can beat its timeout response; the
        // generic RPC error must not erase the real Core terminal outcome.
        let deadline = context
            .turn_deadline
            .map(|deadline| deadline.min(tokio::time::Instant::now() + input.timeout))
            .unwrap_or_else(|| tokio::time::Instant::now() + input.timeout);
        let result = crate::extension_host::manager()
            .execute_pdf(input, context)
            .await;
        return match result {
            Ok((outcome, decision)) => outcome.into_host_text(decision),
            Err(error) => Err(host_terminal_error(error, deadline, request.cancel)),
        };
    }
    run_pdf_driver(input, request.cancel).await.into_rust_text()
}

fn host_terminal_error(
    error: ToolError,
    deadline: tokio::time::Instant,
    cancel: Option<&CancellationToken>,
) -> PdfTextError {
    if cancel.is_some_and(CancellationToken::is_cancelled)
        || matches!(error, ToolError::Cancelled { .. })
    {
        PdfTextError::Cancelled
    } else if tokio::time::Instant::now() >= deadline || matches!(error, ToolError::Timeout { .. })
    {
        PdfTextError::TimedOut
    } else {
        PdfTextError::Execution(format!(
            "PDF host failed: {error}; no Rust fallback was attempted"
        ))
    }
}

/// One actual process driver shared by default Rust and the admitted Host job.
/// Host only receives the small process projection; stdout stays in this result.
pub(crate) async fn run_pdf_driver(
    input: CapturedPdf,
    cancel: Option<&CancellationToken>,
) -> PdfProcessOutcome {
    let output = async {
        if cancel.is_some_and(CancellationToken::is_cancelled) { return Err(PdfTextError::Cancelled); }
        let mut command = tokio::process::Command::new(&input.binary);
        codewhale_util::utils::suppress_tokio_console_window(&mut command);
        command.arg("-layout");
        if let Some((start,end))=input.page_range { command.arg("-f").arg(start.to_string()).arg("-l").arg(end.to_string()); }
        command.arg(&input.path).arg("-").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        codewhale_runtime::child_env::apply_to_tokio_command(&mut command,std::iter::empty::<(&str,&str)>());
        #[cfg(unix)] command.process_group(0);
        let mut child = command.spawn().map_err(|error| if error.kind()==std::io::ErrorKind::NotFound { PdfTextError::BinaryUnavailable } else { PdfTextError::Execution(format!("failed to launch pdftotext: {error}")) })?;
        let tree = codewhale_runtime::process_tree::ProcessTree::attach_tokio(&child).map_err(|error| PdfTextError::Execution(format!("failed to contain pdftotext: {error}")))?;
        let stdout=child.stdout.take().ok_or_else(||PdfTextError::Execution("failed to capture pdftotext stdout".into()))?;
        let stderr=child.stderr.take().ok_or_else(||PdfTextError::Execution("failed to capture pdftotext stderr".into()))?;
        let stdout_task=tokio::spawn(read_bounded(stdout,MAX_PDF_STDOUT_BYTES));
        let stderr_task=tokio::spawn(read_bounded(stderr,MAX_PDF_STDERR_BYTES));
        let status = tokio::select! {
            biased;
            ()=wait_for_cancellation(cancel)=>{
                let _=tree.kill(); terminate_child(&mut child).await;
                finish_capture_tasks(stdout_task,stderr_task).await?; return Err(PdfTextError::Cancelled);
            }
            ()=tokio::time::sleep(input.timeout)=>{
                let _=tree.kill(); terminate_child(&mut child).await;
                finish_capture_tasks(stdout_task,stderr_task).await?; return Err(PdfTextError::TimedOut);
            }
            status=child.wait()=>status.map_err(|error|PdfTextError::Execution(format!("failed to wait for pdftotext: {error}")))?,
        };
        // No parser descendant may retain the capture pipes after the primary exits.
        let _=tree.kill();
        let (stdout,stderr)=finish_capture_tasks(stdout_task,stderr_task).await?;
        Ok((stdout,stderr,status))
    }.await;
    PdfProcessOutcome { output }
}

async fn wait_for_cancellation(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(cancel) => cancel.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

async fn terminate_child(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    max_bytes: usize,
) -> std::io::Result<BoundedOutput> {
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut buffer = [0u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        let retained = read.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(BoundedOutput { bytes, truncated })
}

async fn finish_capture_tasks(
    mut stdout: tokio::task::JoinHandle<std::io::Result<BoundedOutput>>,
    mut stderr: tokio::task::JoinHandle<std::io::Result<BoundedOutput>>,
) -> Result<(BoundedOutput, BoundedOutput), PdfTextError> {
    let joined = tokio::time::timeout(PDF_PIPE_DRAIN_TIMEOUT, async {
        tokio::join!(&mut stdout, &mut stderr)
    })
    .await;
    let (stdout, stderr) = match joined {
        Ok(output) => output,
        Err(_) => {
            stdout.abort();
            stderr.abort();
            let _ = tokio::join!(stdout, stderr);
            return Err(PdfTextError::Execution(
                "pdftotext output pipes did not close after process termination".to_string(),
            ));
        }
    };
    let stdout = stdout
        .map_err(|error| PdfTextError::Execution(format!("stdout reader failed: {error}")))?
        .map_err(|error| PdfTextError::Execution(format!("stdout reader failed: {error}")))?;
    let stderr = stderr
        .map_err(|error| PdfTextError::Execution(format!("stderr reader failed: {error}")))?
        .map_err(|error| PdfTextError::Execution(format!("stderr reader failed: {error}")))?;
    Ok((stdout, stderr))
}

fn sanitized_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .map(|character| match character {
            '\n' | '\t' => character,
            character if character.is_control() => '\u{fffd}',
            character => character,
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
#[path = "pdf/host_tests.rs"]
mod host_tests;
