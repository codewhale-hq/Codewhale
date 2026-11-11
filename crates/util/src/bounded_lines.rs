use tokio::io::{AsyncBufRead, AsyncBufReadExt as _};

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameRefusal {
    #[error("control frame exceeds the 8 MiB transport limit")]
    TooLarge,
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
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            discarding: false,
        }
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                self.discarding = false;
                if self.pending.is_empty() {
                    return Ok(None);
                }
                if self.pending.len() > MAX_FRAME_BYTES {
                    self.pending.clear();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        FrameRefusal::TooLarge,
                    ));
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes).map(Some).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, FrameRefusal::InvalidUtf8)
                });
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
                self.reader.consume(count + usize::from(newline.is_some()));
                self.discarding = newline.is_none();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    FrameRefusal::TooLarge,
                ));
            }
            // Do not let Vec's geometric growth double an almost-full frame.
            self.pending.reserve_exact(count);
            self.pending.extend_from_slice(&available[..count]);
            self.reader.consume(count + usize::from(newline.is_some()));
            if self.pending.len() > MAX_FRAME_BYTES && self.pending.last() != Some(&b'\r') {
                self.pending.clear();
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
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes).map(Some).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, FrameRefusal::InvalidUtf8)
                });
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
}
