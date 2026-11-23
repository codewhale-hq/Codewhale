use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

mod lifecycle_outbox;

pub use lifecycle_outbox::{
    LifecycleEvent, LifecycleOutbox, OUTBOX_DETAIL_MAX_CHARS, OUTBOX_HEADLINE_MAX_CHARS,
    OUTBOX_PREVIEW_MAX_CHARS, OUTBOX_TRUNCATION_MARKER, bounded_text,
};

/// Expand a leading `~` in a configured sink path, reusing the one expansion
/// rule global path overrides use. Documented examples write
/// `~/.codewhale/...`; taken literally that created a directory named `~` in
/// the working directory. A path without `~` (absolute or relative) is
/// returned unchanged, as is `~` when no home resolves.
pub(crate) fn expand_home(path: PathBuf) -> PathBuf {
    codewhale_paths::validate_absolute_path("hook sink path", path.clone()).unwrap_or(path)
}

/// Open a JSONL sink for append, creating it privately: on Unix new parent
/// directories are 0700 and a new file 0600 (existing modes are left alone).
/// Hook and lifecycle events carry tool payloads, prompts, and paths; under
/// an ordinary umask the old defaults were readable by every local user.
pub(crate) async fn open_private_append(path: &Path) -> Result<tokio::fs::File> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let mut dirs = tokio::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        dirs.mode(0o700);
        dirs.create(parent)
            .await
            .with_context(|| format!("failed to create sink directory {}", parent.display()))?;
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true).create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .await
        .with_context(|| format!("failed to open sink file {}", path.display()))
}

pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

static FALLBACK_IDEMPOTENCY_COUNTER: AtomicU64 = AtomicU64::new(1);

/// HTTP transport for lifecycle outbox webhooks: POSTs a JSON payload to a
/// remote endpoint. Failed requests are retried up to 2 times with exponential
/// back-off (200 ms, 400 ms). Every attempt carries a stable `Idempotency-Key`
/// header so receivers can deduplicate retries (or deduplicate on `event.seq`).
/// After exhausting retries the error is propagated.
#[derive(Clone)]
pub struct WebhookHookSink {
    url: String,
    /// Optional bearer token sent as `Authorization: Bearer <token>`.
    bearer_token: Option<String>,
    client: reqwest::Client,
}

impl WebhookHookSink {
    /// Create a new sink that sends events to the given `url`, attaching
    /// `Authorization: Bearer <token>` when a token is provided.
    pub fn new_with_token(url: String, bearer_token: Option<String>) -> Self {
        Self {
            url,
            bearer_token,
            client: codewhale_release::platform_http_client_builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| {
                    codewhale_release::platform_http_client_builder()
                        .build()
                        .unwrap_or_else(|_| codewhale_release::tls::reqwest_client())
                }),
        }
    }

    fn default_idempotency_key(&self, payload: &serde_json::Value) -> String {
        let sink_hash = fnv1a_64(self.url.as_bytes());
        if let Some(seq) = payload
            .get("event")
            .and_then(|event| event.get("seq"))
            .and_then(|seq| seq.as_u64())
        {
            return format!("webhook-{sink_hash:016x}:{seq}");
        }
        let seq = FALLBACK_IDEMPOTENCY_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("webhook-{sink_hash:016x}:{}:{seq}", std::process::id())
    }

    /// POST an arbitrary JSON payload to the configured endpoint with a stable
    /// `Idempotency-Key` header reused across retries.
    pub async fn post_payload(&self, payload: serde_json::Value) -> Result<()> {
        let key = self.default_idempotency_key(&payload);
        self.post_payload_with_idempotency_key(payload, &key).await
    }

    /// POST a JSON payload with a caller-supplied `Idempotency-Key` reused on
    /// every retry attempt.
    pub async fn post_payload_with_idempotency_key(
        &self,
        payload: serde_json::Value,
        idempotency_key: &str,
    ) -> Result<()> {
        let mut retries = 0usize;
        loop {
            let mut request = self
                .client
                .post(&self.url)
                .header("Idempotency-Key", idempotency_key)
                .json(&payload);
            if let Some(token) = self.bearer_token.as_deref().filter(|t| !t.is_empty()) {
                request = request.bearer_auth(token);
            }
            let resp = request.send().await;
            match resp {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    if retries >= 2 {
                        anyhow::bail!("webhook returned non-success status {}", response.status());
                    }
                }
                Err(err) => {
                    if retries >= 2 {
                        return Err(err).context("webhook request failed");
                    }
                }
            }
            retries += 1;
            tokio::time::sleep(std::time::Duration::from_millis(200 * retries as u64)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_home_expands_tilde_and_leaves_other_paths() {
        let home = codewhale_paths::user_home().expect("test home");
        assert_eq!(
            expand_home(PathBuf::from("~/.codewhale/events.jsonl")),
            home.join(".codewhale/events.jsonl")
        );
        assert_eq!(
            expand_home(PathBuf::from("relative/events.jsonl")),
            PathBuf::from("relative/events.jsonl")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_private_append_creates_private_files() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "deepseek-hooks-private-{}-{nanos}",
            std::process::id()
        ));
        let path = root.join("nested").join("events.jsonl");
        open_private_append(&path).await.expect("open");
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn webhook_sink_new_does_not_panic() {
        let sink =
            WebhookHookSink::new_with_token("https://example.invalid/webhook".to_string(), None);
        let _ = sink;
    }

    #[tokio::test]
    async fn webhook_retry_reuses_identical_idempotency_key() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let server = wiremock::MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/hook"))
            .respond_with(move |_req: &wiremock::Request| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    wiremock::ResponseTemplate::new(500)
                } else {
                    wiremock::ResponseTemplate::new(200)
                }
            })
            .mount(&server)
            .await;

        let sink = WebhookHookSink::new_with_token(format!("{}/hook", server.uri()), None);
        sink.post_payload(serde_json::json!({
            "at": "2026-11-23T00:00:00Z",
            "event": { "seq": 7, "kind": "turn.started" }
        }))
        .await
        .expect("retry should succeed on second attempt");

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2);
        let first_key = requests[0]
            .headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .expect("first attempt must carry Idempotency-Key");
        let second_key = requests[1]
            .headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .expect("retry must carry Idempotency-Key");
        assert!(!first_key.is_empty());
        assert_eq!(first_key, second_key);
    }
}
