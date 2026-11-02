/// Selected frontend facts passed by the canonical CLI in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeControlFrontend {
    Stdio,
    Socket { path: Option<std::path::PathBuf> },
    LegacyHttp,
    Acp { model: String },
}

/// One bounded blocking boundary for held filesystem/process identity work.
/// The worker retains its permit and captured handles if the waiter cancels.
#[cfg(any(unix, windows))]
pub async fn owner_work<T, F>(work: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(anyhow::Error::from)?
}
