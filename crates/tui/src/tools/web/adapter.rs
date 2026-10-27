//! The one pure web adapter bridge and provenance for retry decisions.
//! ToolError remains the execution outcome; this private wrapper has no state
//! or authority and is removed only after the last retry consumer decides.
use crate::tools::spec::{ToolContext, ToolError};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

// One per-attempt diagnostic bit, never an execution/approval authority. The
// chain owns it while polling a backend so its outer timeout distinguishes a
// pending Host RPC from a Core transport timeout. Cancellation leaves the bit
// set until that caller reads it; there is no global or cached origin.
tokio::task_local! {
    pub(super) static HOST_PENDING: std::sync::Arc<std::sync::atomic::AtomicBool>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureOrigin {
    ContentOrProvider,
    Host,
    CaptureGuard,
}
#[derive(Debug)]
pub(crate) struct AdapterFailure {
    pub(crate) origin: FailureOrigin,
    pub(crate) error: ToolError,
}
pub(crate) type AdapterResult<T> = Result<T, AdapterFailure>;
impl AdapterFailure {
    pub(crate) fn host(error: ToolError) -> Self {
        Self {
            origin: FailureOrigin::Host,
            error,
        }
    }
    pub(crate) fn capture(error: ToolError) -> Self {
        Self {
            origin: FailureOrigin::CaptureGuard,
            error,
        }
    }
    pub(crate) fn content(&self) -> bool {
        self.origin == FailureOrigin::ContentOrProvider
    }
}
impl From<ToolError> for AdapterFailure {
    fn from(error: ToolError) -> Self {
        Self {
            origin: FailureOrigin::ContentOrProvider,
            error,
        }
    }
}
impl From<AdapterFailure> for ToolError {
    fn from(error: AdapterFailure) -> Self {
        error.error
    }
}
impl std::fmt::Display for AdapterFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for AdapterFailure {}

pub(crate) async fn transform<T: DeserializeOwned>(
    operation: crate::extension_host::StockOperation,
    input: Value,
    context: &ToolContext,
    budget: Duration,
) -> AdapterResult<T> {
    // Preserve the existing stock cap before broker admission. No partial body
    // or automatic backend fallback is a substitute for a full captured value.
    let size = serde_json::to_vec(&input)
        .map_err(|_| {
            AdapterFailure::capture(ToolError::execution_failed(
                "Web adapter capture is not JSON",
            ))
        })?
        .len();
    if size > 1024 * 1024 {
        return Err(AdapterFailure::capture(ToolError::execution_failed(
            "Web adapter capture exceeds the 1 MiB safety limit; no Host fallback was attempted",
        )));
    }
    let _ =
        HOST_PENDING.try_with(|pending| pending.store(true, std::sync::atomic::Ordering::SeqCst));
    let result = crate::extension_host::manager()
        .execute_stock(operation, input, context, budget)
        .await;
    let _ =
        HOST_PENDING.try_with(|pending| pending.store(false, std::sync::atomic::Ordering::SeqCst));
    let result = result.map_err(AdapterFailure::host)?;
    if !result.success {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host returned an invalid transform result; no fallback was attempted",
        )));
    }
    serde_json::from_value(result.metadata.ok_or_else(|| {
        AdapterFailure::host(ToolError::execution_failed(
            "Web Host omitted its transform result",
        ))
    })?)
    .map_err(|_| {
        AdapterFailure::host(ToolError::execution_failed(
            "Web Host returned a malformed transform proposal; no fallback was attempted",
        ))
    })
}

pub(crate) fn search_selected(context: &ToolContext) -> bool {
    context
        .features
        .enabled(codewhale_runtime::features::Feature::WebSearchHost)
}

/// Read the complete provider body under the selected Host capture cap. A cap
/// refusal is never an empty result and never earns another backend request.
pub(crate) async fn read_response(
    response: reqwest::Response,
    context: &ToolContext,
) -> AdapterResult<String> {
    read_response_with_limit(response, context, 1024 * 1024).await
}

pub(crate) async fn read_response_with_limit(
    mut response: reqwest::Response,
    context: &ToolContext,
    maximum: usize,
) -> AdapterResult<String> {
    if !search_selected(context) {
        return response.text().await.map_err(|error| {
            ToolError::execution_failed(format!("Failed to read search response: {error}")).into()
        });
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let read = async {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            ToolError::execution_failed(format!("Failed to read search response: {error}"))
        })? {
            if chunk.len() > maximum.saturating_sub(bytes.len()) {
                return Err(AdapterFailure::capture(ToolError::execution_failed(
                    format!(
                        "Web provider response exceeds the {maximum} byte Host capture limit; no fallback was attempted"
                    ),
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        // Reuse the single bounded charset/NUL decoder. The selected Host path
        // refuses binary provider JSON instead of shipping a partial capture.
        super::extract::decode_response_body(&bytes, content_type.as_deref(), false)
            .map_err(AdapterFailure::capture)
    };
    tokio::pin!(read);
    let cancel = async {
        match context.cancel_token.as_ref() {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let deadline = async {
        match context.turn_deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        biased;
        () = cancel => Err(ToolError::cancelled("web provider capture").into()),
        () = deadline => Err(ToolError::Timeout { seconds: 0 }.into()),
        result = &mut read => result,
    }
}
