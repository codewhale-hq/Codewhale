use std::path::{Path, PathBuf};

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
pub(crate) fn open_private_append_sync(path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let mut dirs = std::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dirs.mode(0o700);
        }
        dirs.create(parent)
            .with_context(|| format!("failed to create sink directory {}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("failed to open sink file {}", path.display()))
}

pub(crate) fn open_private_lock_file_sync(outbox_path: &Path) -> Result<std::fs::File> {
    let mut lock_name = outbox_path
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_else(|| std::ffi::OsString::from("outbox"));
    lock_name.push(".lock");
    let lock_path = match outbox_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            let mut dirs = std::fs::DirBuilder::new();
            dirs.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                dirs.mode(0o700);
            }
            dirs.create(parent)
                .with_context(|| format!("failed to create sink directory {}", parent.display()))?;
            parent.join(lock_name)
        }
        _ => PathBuf::from(lock_name),
    };
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
        .trim_end_matches('.');
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn validate_webhook_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("webhook URL is not a valid URL")?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&parsed) => Ok(()),
        "http" => anyhow::bail!(
            "refusing cleartext http:// webhook URL for non-loopback host; use https://"
        ),
        scheme => anyhow::bail!("unsupported webhook URL scheme '{scheme}'; use https://"),
    }
}

/// Header carrying the stable per-event idempotency key across webhook retries.
pub const WEBHOOK_IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";

/// HTTP transport for lifecycle outbox webhooks: POSTs a JSON payload to a
/// remote endpoint. Failed requests are retried up to 2 times with exponential
/// back-off (200 ms, 400 ms). After exhausting retries the error is propagated.
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
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| {
                    codewhale_release::tls::reqwest_client_builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(std::time::Duration::from_secs(10))
                        .build()
                        .expect("non-redirecting webhook HTTP client")
                }),
        }
    }

    /// POST an arbitrary JSON payload to the configured endpoint with a stable
    /// `Idempotency-Key` header reused across retries.
    ///
    /// This is the delivery path behind the lifecycle outbox fan-out.
    pub async fn post_payload(
        &self,
        payload: serde_json::Value,
        idempotency_key: &str,
    ) -> Result<()> {
        validate_webhook_url(&self.url)?;
        let mut retries = 0usize;
        loop {
            let mut request = self
                .client
                .post(&self.url)
                .header(WEBHOOK_IDEMPOTENCY_KEY_HEADER, idempotency_key)
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
        open_private_append_sync(&path).expect("open");
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

    #[test]
    fn validate_webhook_url_enforces_https_except_loopback() {
        assert!(validate_webhook_url("https://collector.example.com/hook").is_ok());
        assert!(validate_webhook_url("http://127.0.0.1:8080/hook").is_ok());
        assert!(validate_webhook_url("http://[::1]:8080/hook").is_ok());
        assert!(validate_webhook_url("http://localhost:8080/hook").is_ok());
        assert!(validate_webhook_url("http://collector.internal:8080/hook").is_err());
        assert!(validate_webhook_url("http://192.168.1.10:8080/hook").is_err());
        assert!(validate_webhook_url("ftp://collector.example.com/hook").is_err());
        assert!(validate_webhook_url("not-a-url").is_err());
    }

    #[tokio::test]
    async fn webhook_rejects_cleartext_non_loopback_http_with_bearer_token() {
        let sink = WebhookHookSink::new_with_token(
            "http://collector.internal:8080/hook".to_string(),
            Some("secret-token".to_string()),
        );
        let err = sink
            .post_payload(serde_json::json!({"event": "test"}), "test-idem-key")
            .await
            .expect_err("cleartext non-loopback http webhook must be rejected");
        assert!(err.to_string().contains("https://"), "{err}");
    }
}
