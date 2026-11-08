/// Selected frontend facts passed by the canonical CLI in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeControlFrontend {
    Stdio,
    Socket { path: Option<std::path::PathBuf> },
    Http,
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
    #[cfg(any(test, feature = "test-support"))]
    let env_ticket = crate::test_support::env_scope_ticket();
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    const PERMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
    let slots = SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone();
    if slots.available_permits() == 0 {
        tracing::warn!(
            target: "turn",
            "owner_work is waiting: all 4 blocking slots are held by earlier jobs"
        );
    }
    let permit = tokio::time::timeout(PERMIT_WAIT, slots.acquire_owned())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "blocking work is queued behind earlier jobs that have not finished within {}s",
                PERMIT_WAIT.as_secs()
            )
        })??;
    tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let _permit = permit;
        work()
    })
    .await
    .map_err(anyhow::Error::from)?
}
