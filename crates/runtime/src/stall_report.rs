use std::path::PathBuf;
use std::time::Duration;

/// One detected stall episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallReport {
    /// Which watchdog saw it (`engine`, `ui`, `client`).
    pub source: &'static str,
    pub phase: String,
    pub detail: Option<String>,
    pub turn_id: Option<String>,
    /// Provider response/request id when the stream reported one, else the
    /// route label the request went to.
    pub provider_request: Option<String>,
    pub since_progress: Duration,
    pub bound: Option<Duration>,
}

impl StallReport {
    /// One user-facing line: where it stalled and what to do.
    #[must_use]
    pub fn status_line(&self) -> String {
        let mut line = format!(
            "Turn stalled {} — no progress for {}s",
            self.phase,
            self.since_progress.as_secs()
        );
        if let Some(detail) = self.detail.as_deref().filter(|d| !d.is_empty()) {
            line.push_str(&format!(" ({detail})"));
        }
        line.push_str(". Press Esc to cancel and retry.");
        line
    }

    fn record_body(&self) -> String {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let bound = self.bound.map_or_else(
            || "none (parked)".to_string(),
            |b| format!("{}s", b.as_secs()),
        );
        format!(
            "Kind: turn-stall\nSource: {source}\nTimestamp: {timestamp}\nPhase: {phase}\n\
             Detail: {detail}\nTurn: {turn}\nProvider request: {request}\n\
             No progress for: {since}s\nPhase bound: {bound}\n",
            source = self.source,
            phase = self.phase,
            detail = self.detail.as_deref().unwrap_or("-"),
            turn = self.turn_id.as_deref().unwrap_or("-"),
            request = self.provider_request.as_deref().unwrap_or("-"),
            since = self.since_progress.as_secs(),
        )
    }
}

#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
thread_local! {
    static TEST_STALL_RECORD_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Route stall records for the current test thread into `dir` (tests never
/// write into the real `~/.codewhale/crashes`).
#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
pub fn set_test_stall_record_dir(dir: Option<PathBuf>) {
    TEST_STALL_RECORD_DIR.with(|slot| *slot.borrow_mut() = dir);
}

/// The selected profile's crash directory, shared with panic dumps.
pub fn stall_record_dir() -> Option<PathBuf> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        TEST_STALL_RECORD_DIR.with(|slot| slot.borrow().clone())
    }
    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    {
        codewhale_config::codewhale_home()
            .ok()
            .map(|home| home.join("crashes"))
    }
}

/// Log a stall and write its record to `crashes/<timestamp>-turn-stall-<source>.log`.
/// Best effort; returns the record path only when the write succeeds. The
/// write runs on its own short-lived thread so no caller (engine task or UI
/// event loop) blocks a runtime worker on unbounded disk I/O (#6149).
pub fn report_stall(report: &StallReport) -> Option<PathBuf> {
    let candidate = stall_record_dir().map(|dir| {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        dir.join(format!("{stamp}-turn-stall-{}.log", report.source))
    });
    let path = candidate.and_then(|candidate| {
        let body = report.record_body();
        let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<()>>(1);
        let writer_path = candidate.clone();
        let writer = std::thread::spawn(move || {
            let written = writer_path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&writer_path, body));
            if let Err(error) = &written {
                let msg = format!(
                    "stall record was not written to {}: {error}",
                    writer_path.display()
                );
                tracing::warn!(target: "turn_stall", path = %writer_path.display(), %error, "stall record was not written");
                crate::logging::warn(&msg);
            }
            let _ = tx.send(written);
        });
        // Tests read the record right after reporting; release caps the wait.
        #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
        let wrote = {
            let _ = writer.join();
            matches!(rx.recv(), Ok(Ok(())))
        };
        #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
        let wrote = {
            drop(writer);
            matches!(rx.recv_timeout(Duration::from_millis(50)), Ok(Ok(())))
        };
        wrote.then_some(candidate)
    });
    let message = format!(
        "turn stall ({source}): phase={phase} since_progress={since}s bound={bound:?} turn={turn} request={request} detail={detail} record={record}",
        source = report.source,
        phase = report.phase,
        since = report.since_progress.as_secs(),
        bound = report.bound.map(|b| b.as_secs()),
        turn = report.turn_id.as_deref().unwrap_or("-"),
        request = report.provider_request.as_deref().unwrap_or("-"),
        detail = report.detail.as_deref().unwrap_or("-"),
        record = path
            .as_deref()
            .map_or_else(|| "-".to_string(), |p| p.display().to_string()),
    );
    tracing::warn!(target: "turn_stall", "{message}");
    crate::logging::warn(&message);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirGuard;

    impl Drop for TestDirGuard {
        fn drop(&mut self) {
            set_test_stall_record_dir(None);
        }
    }

    fn sample_report() -> StallReport {
        StallReport {
            source: "engine",
            phase: "waiting for provider".to_string(),
            detail: Some("test detail".to_string()),
            turn_id: Some("turn-1".to_string()),
            provider_request: Some("req-1".to_string()),
            since_progress: Duration::from_secs(65),
            bound: Some(Duration::from_secs(60)),
        }
    }

    #[test]
    fn report_stall_returns_path_when_write_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = TestDirGuard;
        set_test_stall_record_dir(Some(dir.path().to_path_buf()));

        let path = report_stall(&sample_report()).expect("writable dir should return receipt");
        let body = std::fs::read_to_string(&path).expect("record should exist on disk");
        assert!(body.contains("Kind: turn-stall"));
        assert!(body.contains("Source: engine"));
    }

    #[test]
    fn report_stall_returns_none_when_write_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"file").expect("create blocking file");
        let _guard = TestDirGuard;
        set_test_stall_record_dir(Some(blocker));

        let receipt = report_stall(&sample_report());
        assert!(
            receipt.is_none(),
            "failed write must not return a path receipt"
        );
    }
}
