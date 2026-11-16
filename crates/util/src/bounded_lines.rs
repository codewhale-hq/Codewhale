use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameRefusal {
    #[error("control frame exceeds the 8 MiB transport limit")]
    TooLarge,
    #[error("control pending input budget exhausted")]
    Busy,
    #[error("control frame is not UTF-8")]
    InvalidUtf8,
}

pub fn is_frame_refusal(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<FrameRefusal>())
}

/// Cancellation-safe, allocation-bounded newline framing shared by every
/// local control transport. Consumed partial bytes remain owned by the reader.
pub struct BoundedLines<R> {
    reader: R,
    pending: Vec<u8>,
    discarding: bool,
    budget: Option<Arc<Semaphore>>,
    pending_budget: Option<OwnedSemaphorePermit>,
    frame_budget: Option<OwnedSemaphorePermit>,
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            discarding: false,
            budget: None,
            pending_budget: None,
            frame_budget: None,
        }
    }

    pub fn with_budget(reader: R, budget: Arc<Semaphore>) -> Self {
        Self {
            budget: Some(budget),
            ..Self::new(reader)
        }
    }

    pub fn take_frame_budget(&mut self) -> Option<OwnedSemaphorePermit> {
        self.frame_budget.take()
    }

    fn finish_frame(&mut self) -> std::io::Result<Option<String>> {
        let bytes = std::mem::take(&mut self.pending);
        match String::from_utf8(bytes) {
            Ok(line) => {
                self.frame_budget = self.pending_budget.take();
                Ok(Some(line))
            }
            Err(_) => {
                self.pending_budget = None;
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    FrameRefusal::InvalidUtf8,
                ))
            }
        }
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        self.frame_budget = None;
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                self.discarding = false;
                if self.pending.is_empty() {
                    return Ok(None);
                }
                if self.pending.len() > MAX_FRAME_BYTES {
                    self.pending.clear();
                    self.pending_budget = None;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        FrameRefusal::TooLarge,
                    ));
                }
                return self.finish_frame();
            }
            let available = &available[..available.len().min(8192)];
            let newline = available.iter().position(|&byte| byte == b'\n');
            let count = newline.unwrap_or(available.len());
            if self.discarding {
                self.reader.consume(count + usize::from(newline.is_some()));
                if newline.is_some() {
                    self.discarding = false;
                } else {
                    tokio::task::yield_now().await;
                }
                continue;
            }
            if count > (MAX_FRAME_BYTES + 1).saturating_sub(self.pending.len()) {
                self.pending.clear();
                self.pending_budget = None;
                self.reader.consume(count + usize::from(newline.is_some()));
                self.discarding = newline.is_none();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    FrameRefusal::TooLarge,
                ));
            }
            if let Some(budget) = self.budget.as_ref() {
                match budget.clone().try_acquire_many_owned(count as u32) {
                    Ok(permit) => match self.pending_budget.as_mut() {
                        Some(held) => held.merge(permit),
                        None => self.pending_budget = Some(permit),
                    },
                    Err(_) => {
                        self.pending.clear();
                        self.pending_budget = None;
                        self.reader.consume(count + usize::from(newline.is_some()));
                        self.discarding = newline.is_none();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            FrameRefusal::Busy,
                        ));
                    }
                }
            }
            // Do not let Vec's geometric growth double an almost-full frame.
            self.pending.reserve_exact(count);
            self.pending.extend_from_slice(&available[..count]);
            self.reader.consume(count + usize::from(newline.is_some()));
            if self.pending.len() > MAX_FRAME_BYTES && self.pending.last() != Some(&b'\r') {
                self.pending.clear();
                self.pending_budget = None;
                self.discarding = newline.is_none();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    FrameRefusal::TooLarge,
                ));
            }
            if newline.is_some() {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                return self.finish_frame();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt as _, BufReader};

    #[tokio::test]
    async fn bounded_lines_refuses_oversize_before_retaining_extra_bytes() {
        let input = vec![b'x'; MAX_FRAME_BYTES + 4096];
        let mut reader = BoundedLines::new(BufReader::with_capacity(8192, input.as_slice()));
        assert_eq!(
            reader.next_line().await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(reader.pending.len() <= MAX_FRAME_BYTES + 1);
        assert!(reader.pending.capacity() <= MAX_FRAME_BYTES + 1);
    }

    #[tokio::test]
    async fn bounded_lines_keeps_consumed_prefix_when_read_future_is_cancelled() {
        let (mut input, output) = tokio::io::duplex(64);
        input.write_all(b"partial").await.unwrap();
        let mut reader = BoundedLines::new(BufReader::new(output));
        std::future::poll_fn(|cx| {
            let mut read = std::pin::pin!(reader.next_line());
            assert!(std::future::Future::poll(read.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(reader.pending, b"partial");
        input.write_all(b"-continued\n").await.unwrap();
        assert_eq!(
            reader.next_line().await.unwrap().as_deref(),
            Some("partial-continued")
        );
    }
    #[tokio::test]
    async fn bounded_lines_budget_covers_admitted_and_partial_frames_and_recovers_after_busy() {
        let budget = Arc::new(Semaphore::new(8));
        let mut reader = BoundedLines::with_budget(
            BufReader::new(b"first\nsecond\nlast\n".as_slice()),
            budget.clone(),
        );
        assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("first"));
        let admitted = reader.take_frame_budget().unwrap();
        assert_eq!(budget.available_permits(), 3);
        let refusal = reader.next_line().await.unwrap_err();
        assert!(matches!(
            refusal.get_ref().unwrap().downcast_ref::<FrameRefusal>(),
            Some(FrameRefusal::Busy)
        ));
        assert_eq!(budget.available_permits(), 3);
        drop(admitted);
        assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("last"));
        assert_eq!(budget.available_permits(), 4);
        drop(reader.take_frame_budget());
        assert_eq!(budget.available_permits(), 8);
    }

    #[tokio::test]
    async fn bounded_lines_budget_survives_read_cancellation_and_releases_on_drop_or_invalid_utf8()
    {
        let budget = Arc::new(Semaphore::new(8));
        let (mut input, output) = tokio::io::duplex(64);
        input.write_all(b"part").await.unwrap();
        let mut reader = BoundedLines::with_budget(BufReader::new(output), budget.clone());
        std::future::poll_fn(|cx| {
            let mut future = std::pin::pin!(reader.next_line());
            assert!(std::future::Future::poll(future.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(budget.available_permits(), 4);
        drop(reader);
        assert_eq!(budget.available_permits(), 8);
        let mut invalid =
            BoundedLines::with_budget(BufReader::new(b"\xff\n".as_slice()), budget.clone());
        assert!(invalid.next_line().await.is_err());
        assert_eq!(budget.available_permits(), 8);
    }
}
