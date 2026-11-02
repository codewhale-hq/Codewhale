use anyhow::Result;
use codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES;

use crate::session_manager::SavedSession;

pub async fn history_owner_work<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    #[cfg(any(test, feature = "test-support"))]
    let ticket = crate::test_support::env_scope_ticket();
    codewhale_runtime::runtime_control::owner_work(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _membership = crate::test_support::join_env_scope(ticket);
        work()
    })
    .await
}

/// Enforce the canonical byte bound on a history, measured the way it is
/// carried between owners rather than the way it sits on disk.
///
/// A saved document repeats its own conversation while it carries a journal:
/// `messages` and the journal's active branch hold the same turns, so the
/// stored file is up to twice the history it holds. The reader that opens such
/// a document already judges it by [`SavedSession::transport_form`]
/// (`SessionManager::load_session_snapshot_with_limit`), so a writer that
/// judged the stored shape against the same bound refused to write back a
/// conversation the runtime was still opening, saving and running turns on.
/// That is exactly how a fork — and with it `/undo`, retry and
/// `fork-at-turn` — came to fail on every conversation whose stored file
/// passed 8 MiB while its history was still half that.
///
/// Every writer of a complete history calls this instead of measuring the
/// session itself, so the writers cannot drift from each other or from the
/// reader.
pub fn ensure_history_transport_fits(session: &SavedSession) -> Result<()> {
    let mut size = HistorySizeBound(MAX_CANONICAL_HISTORY_BYTES);
    serde_json::to_writer(&mut size, &session.transport_form())?;
    Ok(())
}

pub fn saved_document_digest(session: &SavedSession) -> Result<String> {
    ensure_history_transport_fits(session)?;
    Ok(codewhale_runtime::hashing::sha256_hex(
        codewhale_runtime::hashing::canonical_json(&serde_json::to_value(session)?).as_bytes(),
    ))
}

pub struct HistorySizeBound(pub usize);
impl std::io::Write for HistorySizeBound {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "full history exceeds byte transport bound; source retained",
            ));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
