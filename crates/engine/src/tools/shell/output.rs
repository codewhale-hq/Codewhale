use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use encoding_rs::{CoderResult, Decoder, UTF_8};

const BOUNDED_OUTPUT_MAX_LINES: usize = 2_000;
const BOUNDED_OUTPUT_MAX_BYTES: usize = 50 * 1024;
const BOUNDED_OUTPUT_RETAIN_BYTES: usize = BOUNDED_OUTPUT_MAX_BYTES + 4;

#[derive(Debug)]
pub(super) struct BoundedOutputSnapshot {
    pub(super) content: String,
    pub(super) total_bytes: usize,
    pub(super) retained_bytes: usize,
    pub(super) truncated: bool,
}

/// One decoded, arrival-ordered stream: complete output goes to disk while
/// memory retains only enough tail bytes for the 2,000-line/50KiB result bound.
pub(super) struct BoundedOutputAccumulator {
    tail: VecDeque<u8>,
    tail_newlines: usize,
    total_bytes: usize,
    total_newlines: usize,
    current_line_bytes: usize,
    last_line_bytes: usize,
    front_clipped: bool,
    last_byte: Option<u8>,
    decoder: Decoder,
    stream_finished: bool,
    stream_error: Option<String>,
    temp: Option<tempfile::NamedTempFile>,
    full_output_path: Option<PathBuf>,
    /// Why the on-disk spill file could not be created (disk full, descriptor
    /// exhaustion, unwritable temp dir). The stream still runs and the bounded
    /// tail is still delivered; only "Full output: <path>" is unavailable.
    spill_unavailable: Option<String>,
}

impl BoundedOutputAccumulator {
    /// Build an accumulator whose complete-output spill file lives in
    /// `spill_dir` (`None` = process temp dir). Never fails: when the spill
    /// file cannot be created (disk full, `EMFILE`, missing temp dir) the
    /// command still runs and the bounded tail is still returned — the spill
    /// is a convenience, not a precondition for executing `echo ok`. Tests
    /// pass a nonexistent dir to fault-inject the failure.
    pub(super) fn new_in(spill_dir: Option<&std::path::Path>) -> Self {
        let mut builder = tempfile::Builder::new();
        builder.prefix("codewhale-bash-");
        let temp = match spill_dir {
            Some(dir) => builder.tempfile_in(dir),
            None => builder.tempfile(),
        };
        let (temp, spill_unavailable) = match temp {
            Ok(temp) => (Some(temp), None),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "shell output spill file unavailable; continuing with the in-memory tail only"
                );
                (None, Some(spill_unavailable_reason(&error)))
            }
        };
        Self {
            tail: VecDeque::with_capacity(BOUNDED_OUTPUT_RETAIN_BYTES),
            tail_newlines: 0,
            total_bytes: 0,
            total_newlines: 0,
            current_line_bytes: 0,
            last_line_bytes: 0,
            front_clipped: false,
            last_byte: None,
            decoder: UTF_8.new_decoder_without_bom_handling(),
            stream_finished: false,
            stream_error: None,
            temp,
            full_output_path: None,
            spill_unavailable,
        }
    }

    /// Why the complete output is not being persisted, if it is not.
    #[cfg(test)]
    pub(super) fn spill_unavailable(&self) -> Option<&str> {
        self.spill_unavailable.as_deref()
    }

    fn decode(&mut self, bytes: &[u8], last: bool) -> String {
        let capacity = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len().saturating_mul(3).saturating_add(3));
        let mut decoded = String::with_capacity(capacity);
        let mut offset = 0;
        loop {
            let (result, read, _) =
                self.decoder
                    .decode_to_string(&bytes[offset..], &mut decoded, last);
            offset += read;
            if result == CoderResult::InputEmpty {
                return decoded;
            }
            decoded.reserve(capacity.max(4));
        }
    }

    pub(super) fn append(&mut self, raw: &[u8]) -> io::Result<()> {
        if self.stream_finished {
            return Err(io::Error::other(
                "shell output arrived after the stream closed",
            ));
        }
        if let Some(temp) = self.temp.as_mut() {
            temp.write_all(raw)?;
        }
        let decoded = self.decode(raw, false);
        self.append_decoded(decoded.as_bytes());
        Ok(())
    }

    pub(super) fn finish(&mut self) -> io::Result<()> {
        if !self.stream_finished {
            let decoded = self.decode(&[], true);
            self.append_decoded(decoded.as_bytes());
            if let Some(temp) = self.temp.as_mut() {
                temp.flush()?;
            }
            self.stream_finished = true;
        }
        Ok(())
    }

    pub(super) fn record_error(&mut self, error: &io::Error) {
        self.stream_error = Some(error.to_string());
    }

    fn append_decoded(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        for &byte in bytes {
            self.tail.push_back(byte);
            if byte == b'\n' {
                self.tail_newlines += 1;
                self.total_newlines += 1;
                self.last_line_bytes = self.current_line_bytes;
                self.current_line_bytes = 0;
            } else {
                self.current_line_bytes += 1;
            }
            self.last_byte = Some(byte);
        }
        while self.tail.len() > BOUNDED_OUTPUT_RETAIN_BYTES {
            self.pop_front();
            self.front_clipped = true;
        }
        while self.tail_lines() > BOUNDED_OUTPUT_MAX_LINES {
            while let Some(byte) = self.tail.pop_front() {
                if byte == b'\n' {
                    self.tail_newlines -= 1;
                    break;
                }
            }
            self.front_clipped = false;
        }
    }

    fn pop_front(&mut self) {
        if self.tail.pop_front() == Some(b'\n') {
            self.tail_newlines -= 1;
        }
    }

    fn tail_lines(&self) -> usize {
        self.tail_newlines + usize::from(self.tail.back().is_some_and(|byte| *byte != b'\n'))
    }

    fn total_lines(&self) -> usize {
        self.total_newlines + usize::from(self.last_byte.is_some_and(|byte| byte != b'\n'))
    }

    fn selected(&self) -> (Vec<u8>, bool) {
        let mut bytes = self.tail.iter().copied().collect::<Vec<_>>();
        let recent_line_bytes = if self.last_byte == Some(b'\n') {
            self.last_line_bytes
        } else {
            self.current_line_bytes
        };
        let partial_line = recent_line_bytes > BOUNDED_OUTPUT_MAX_BYTES;
        if partial_line {
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            let floor = bytes.len().saturating_sub(BOUNDED_OUTPUT_MAX_BYTES);
            let start = (floor..bytes.len())
                .find(|index| std::str::from_utf8(&bytes[*index..]).is_ok())
                .unwrap_or(bytes.len());
            bytes.drain(..start);
        } else if self.front_clipped
            && let Some(newline) = bytes.iter().position(|byte| *byte == b'\n')
        {
            bytes.drain(..=newline);
        }
        (bytes, partial_line)
    }

    fn format_size(bytes: usize) -> String {
        if bytes < 1024 {
            format!("{bytes}B")
        } else if bytes < 1024 * 1024 {
            format!("{:.1}KB", bytes as f64 / 1024.0)
        } else {
            format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
        }
    }

    pub(super) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Decoded output appended after `cursor` (a previous `total_bytes`), for
    /// incremental readers such as `wait`. The second value counts bytes of
    /// that range the memory bound already discarded; the complete output is
    /// still in the spill file.
    pub(super) fn delta_since(&self, cursor: usize) -> io::Result<(String, usize)> {
        if let Some(error) = self.stream_error.as_ref() {
            return Err(io::Error::other(error.clone()));
        }
        let new_bytes = self.total_bytes.saturating_sub(cursor);
        let available = new_bytes.min(self.tail.len());
        let mut bytes = self
            .tail
            .range(self.tail.len() - available..)
            .copied()
            .collect::<Vec<_>>();
        // A cursor is always a character boundary; a clipped tail's front may
        // not be, so drop any continuation bytes it starts with.
        let partial = bytes
            .iter()
            .take_while(|byte| (**byte & 0xC0) == 0x80)
            .count();
        bytes.drain(..partial);
        let omitted = new_bytes - available + partial;
        Ok((String::from_utf8_lossy(&bytes).into_owned(), omitted))
    }

    /// First line of a delta that lost `omitted` bytes to the memory bound:
    /// how many, and where the complete output is (or why it is not kept).
    pub(super) fn omitted_notice(&self, omitted: usize) -> String {
        let location = self
            .full_output_path
            .as_deref()
            .or_else(|| self.temp.as_ref().map(tempfile::NamedTempFile::path));
        match (location, self.spill_unavailable.as_deref()) {
            (Some(path), _) => format!(
                "[{omitted} bytes of earlier output not retained in memory. Full output: {}]\n",
                path.display()
            ),
            (None, Some(reason)) => format!(
                "[{omitted} bytes of earlier output not retained in memory. Full output was not persisted: {reason}]\n"
            ),
            (None, None) => {
                format!("[{omitted} bytes of earlier output not retained in memory]\n")
            }
        }
    }

    pub(super) fn snapshot(&mut self, finalize: bool) -> io::Result<BoundedOutputSnapshot> {
        if let Some(error) = self.stream_error.as_ref() {
            return Err(io::Error::other(error.clone()));
        }
        let (selected, partial_line) = self.selected();
        let retained_bytes = selected.len();
        let truncated = retained_bytes < self.total_bytes;
        let total_lines = self.total_lines();
        let kept_lines = selected.iter().filter(|byte| **byte == b'\n').count()
            + usize::from(selected.last().is_some_and(|byte| *byte != b'\n'));
        let mut content = String::from_utf8(selected).expect("stream decoder emits valid UTF-8");

        if finalize && self.stream_finished && self.full_output_path.is_none() {
            if truncated {
                if let Some(mut temp) = self.temp.take() {
                    temp.flush()?;
                    let (_, path) = temp.keep().map_err(|error| error.error)?;
                    self.full_output_path = Some(path);
                }
            } else {
                self.temp.take();
            }
        }
        if truncated && finalize && self.full_output_path.is_none() {
            let reason = self.spill_unavailable.as_deref().unwrap_or(
                "the output stream did not close cleanly, so the spill file was not kept",
            );
            content.push_str(&format!(
                "\n\n[Showing the last {} of {} lines ({} limit). Full output was not persisted: {reason}]",
                Self::format_size(retained_bytes),
                total_lines,
                Self::format_size(BOUNDED_OUTPUT_MAX_BYTES),
            ));
        } else if truncated
            && finalize
            && let Some(path) = self.full_output_path.as_ref()
        {
            if partial_line {
                content.push_str(&format!(
                    "\n\n[Showing last {} of line {} (line is {}). Full output: {}]",
                    Self::format_size(retained_bytes),
                    total_lines,
                    Self::format_size(self.current_line_bytes),
                    path.display()
                ));
            } else {
                let start = total_lines.saturating_sub(kept_lines) + 1;
                let limit = if self.front_clipped {
                    format!(" ({} limit)", Self::format_size(BOUNDED_OUTPUT_MAX_BYTES))
                } else {
                    String::new()
                };
                content.push_str(&format!(
                    "\n\n[Showing lines {start}-{total_lines} of {total_lines}{limit}. Full output: {}]",
                    path.display()
                ));
            }
        }
        Ok(BoundedOutputSnapshot {
            content,
            total_bytes: self.total_bytes,
            retained_bytes,
            truncated,
        })
    }

    #[cfg(test)]
    pub(super) fn retained_memory_bytes(&self) -> usize {
        self.tail.len()
    }

    #[cfg(test)]
    pub(super) fn full_output_path(&self) -> Option<&std::path::Path> {
        self.full_output_path.as_deref()
    }
}

/// Human-readable, actionable reason for a failed spill-file creation.
pub(super) fn spill_unavailable_reason(error: &io::Error) -> String {
    match resource_exhaustion_hint(error) {
        Some(hint) => format!("{error} ({hint})"),
        None => error.to_string(),
    }
}

/// When an I/O error looks like host resource exhaustion, name the likely
/// cause and the remedy. Returns `None` for ordinary errors.
pub(super) fn resource_exhaustion_hint(error: &io::Error) -> Option<&'static str> {
    use io::ErrorKind;
    match error.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => {
            return Some("the disk holding the temp dir is full; free space and retry");
        }
        ErrorKind::OutOfMemory => {
            return Some("the host is out of memory; close heavy processes and retry");
        }
        _ => {}
    }
    let code = error.raw_os_error()?;
    // ENOSPC / EDQUOT / EMFILE / ENFILE / ENOMEM / EAGAIN — the codes fork(2),
    // pipe(2), and open(2) return when the machine is thrashing.
    #[cfg(unix)]
    {
        if code == libc::ENOSPC || code == libc::EDQUOT {
            return Some("the disk holding the temp dir is full; free space and retry");
        }
        if code == libc::EMFILE || code == libc::ENFILE {
            return Some(
                "the process or host has run out of file descriptors; close background jobs or raise `ulimit -n` and retry",
            );
        }
        if code == libc::ENOMEM {
            return Some("the host is out of memory; close heavy processes and retry");
        }
        if code == libc::EAGAIN {
            return Some(
                "the host refused to create a process, thread, or pipe (resource limit reached); close heavy processes and retry",
            );
        }
    }
    #[cfg(windows)]
    {
        // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL, ERROR_NOT_ENOUGH_MEMORY, ERROR_TOO_MANY_OPEN_FILES
        if code == 112 || code == 39 {
            return Some("the disk holding the temp dir is full; free space and retry");
        }
        if code == 8 {
            return Some("the host is out of memory; close heavy processes and retry");
        }
        if code == 4 {
            return Some(
                "the process has run out of file handles; close background jobs and retry",
            );
        }
    }
    let _ = code;
    None
}

/// Hard in-flight ceiling for one raw shell stream held in memory (#5472).
/// Past this the oldest bytes are dropped — counted, never silently lost — so
/// one chatty command (`cargo build -v`, `git log -p`) cannot grow the process
/// by its entire output. Deliberately far above every consumer of these bytes:
/// the 30 KB tool-result truncation (`shell_output::MAX_OUTPUT_SIZE`), the
/// 1,200-char job-panel tail and the 1 KiB completion tail all fit with three
/// orders of magnitude to spare. The only surface a clip can reach is the
/// durable completion artifact, which records the omission explicitly.
pub(super) const RAW_STREAM_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Extra headroom before a front-drop, so the O(len) compaction runs once per
/// `cap / 4` bytes appended instead of once per chunk.
const RAW_STREAM_DROP_SLACK: usize = RAW_STREAM_MAX_BYTES / 4;

/// Tail retained once a job's output has been *delivered* — the foreground
/// result is already the tool result, or the completion evidence is already
/// written to its session artifact. Everything past this is dead weight for
/// the up-to-1 h the finished record stays listed (#5472 finding 1).
pub(super) const RAW_STREAM_SETTLED_TAIL_BYTES: usize = 64 * 1024;

/// One raw (undecoded) shell stream retained in memory for a live job.
///
/// Bounded two independent ways, which is the whole point of the type:
/// `append` enforces `cap` while the command runs, and `release_to_tail`
/// collapses the buffer the moment its bytes have been delivered. Both record
/// how many leading bytes were discarded so `total_len` — and therefore every
/// `stdout_len` / `byte_length` the model and the artifact see — stays honest.
pub(super) struct RawOutputBuffer {
    data: Vec<u8>,
    dropped: usize,
    cap: usize,
    abandoned: bool,
}

impl RawOutputBuffer {
    pub(super) fn new() -> Self {
        Self::with_cap(RAW_STREAM_MAX_BYTES)
    }

    pub(super) fn with_cap(cap: usize) -> Self {
        Self {
            data: Vec::new(),
            dropped: 0,
            cap: cap.max(1),
            abandoned: false,
        }
    }

    /// Append, returning `false` once nobody will ever read this stream again.
    ///
    /// The reader thread uses that as its exit condition, which is the only way
    /// out when a descendant has escaped the process group and holds the pipe
    /// write-end open: `read()` will never see EOF, so without this the thread
    /// runs — and retains its buffer — for the life of the process (#5472
    /// finding 2).
    pub(super) fn append(&mut self, bytes: &[u8]) -> bool {
        if self.abandoned {
            // Keep the total honest even though the bytes are discarded.
            self.dropped = self.dropped.saturating_add(bytes.len());
            return false;
        }
        self.data.extend_from_slice(bytes);
        if self.data.len() > self.cap.saturating_add(RAW_STREAM_DROP_SLACK.min(self.cap)) {
            self.drop_front_to(self.cap);
        }
        true
    }

    /// Give up on this stream: release everything held and stop accepting more.
    ///
    /// Called when the bounded reader join times out. The shell is already
    /// terminal and its result already delivered, so nothing can consume these
    /// bytes; holding them until the writer eventually closes is pure residency.
    pub(super) fn abandon(&mut self) {
        self.abandoned = true;
        self.dropped = self.dropped.saturating_add(self.data.len());
        self.data = Vec::new();
    }

    /// Total bytes this stream has produced, including bytes no longer held.
    pub(super) fn total_len(&self) -> usize {
        self.dropped.saturating_add(self.data.len())
    }

    /// Leading bytes discarded by the in-flight cap or by `release_to_tail`.
    pub(super) fn dropped(&self) -> usize {
        self.dropped
    }

    pub(super) fn retained(&self) -> &[u8] {
        &self.data
    }

    /// Collapse to at most `keep` trailing bytes and give the allocation back.
    /// Called once a job is terminal *and* its output has been delivered.
    pub(super) fn release_to_tail(&mut self, keep: usize) {
        if self.data.len() <= keep {
            return;
        }
        self.drop_front_to(keep);
        self.data.shrink_to_fit();
    }

    fn drop_front_to(&mut self, keep: usize) {
        let mut start = self.data.len().saturating_sub(keep);
        // Snap forward off a UTF-8 continuation byte so the retained slice
        // never begins mid-character (the leading-U+FFFD bug guarded against
        // in `tail_from_buffer`).
        while start < self.data.len() && (self.data[start] & 0xC0) == 0x80 {
            start += 1;
        }
        self.data.drain(..start);
        self.dropped = self.dropped.saturating_add(start);
    }
}

impl Default for RawOutputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) type SharedRawOutput = Arc<Mutex<RawOutputBuffer>>;

pub(super) fn new_shared_raw_output() -> SharedRawOutput {
    Arc::new(Mutex::new(RawOutputBuffer::new()))
}

pub(super) fn take_delta_from_buffer(
    buffer: &SharedRawOutput,
    cursor: &mut usize,
) -> (Vec<u8>, usize) {
    let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let total = guard.total_len();
    // The cursor is an absolute offset into the stream. Bytes the bound already
    // discarded can never be delivered as a delta, so skip forward over them
    // rather than re-sending the retained tail as if it were new.
    let start_abs = (*cursor).max(guard.dropped()).min(total);
    let start = start_abs - guard.dropped();
    let retained = guard.retained();
    // Clone only the unread portion (the delta), not the entire accumulated buffer.
    // Long-running processes can produce megabytes of output; cloning the full
    // buffer on every poll held the ShellManager mutex for O(total_bytes) time.
    let unread = &retained[start..];
    // A poll can land mid-character: the caller decodes this delta as UTF-8, so
    // handing back a truncated multibyte sequence renders it as replacement
    // glyphs and corrupts the next delta's leading byte too (the streaming-client
    // bug from #1675, in the shell preview path). Leave an incomplete trailing
    // sequence in the buffer for the next poll. Bytes that are genuinely invalid
    // rather than merely unfinished still pass through, so binary output cannot
    // stall the cursor, and the final result is read from the whole buffer.
    let consumed = match std::str::from_utf8(unread) {
        Ok(_) => unread.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => unread.len(),
    };
    let delta = unread[..consumed].to_vec();
    *cursor = start_abs + consumed;
    (delta, total)
}

/// Read only the tail of a byte buffer and return (total_len, tail_string).
///
/// Avoids cloning the full buffer when only a trailing excerpt is needed
/// (e.g. for the job-panel display). `max_tail_chars` is in Unicode scalar
/// values; we read at most `max_tail_chars * 4` bytes from the end to account
/// for multi-byte UTF-8 sequences.
pub(super) fn tail_from_buffer(buffer: &SharedRawOutput, max_tail_chars: usize) -> (usize, String) {
    let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    // The reported length is the stream's total, not what is still held: a
    // released or clipped buffer must not make the model believe the command
    // printed less than it did.
    let total = guard.total_len();
    let retained = guard.retained();
    let retained_len = retained.len();
    // Over-estimate byte count (4 bytes per char worst case for UTF-8).
    let mut tail_start = retained_len.saturating_sub(max_tail_chars.saturating_mul(4));
    // Snap forward to the next valid UTF-8 codepoint boundary so we don't
    // pass a slice beginning with continuation bytes (0x80-0xBF) to
    // from_utf8_lossy, which would emit a leading U+FFFD replacement char.
    while tail_start < retained_len && (retained[tail_start] & 0xC0) == 0x80 {
        tail_start += 1;
    }
    let tail_str = String::from_utf8_lossy(&retained[tail_start..]).into_owned();
    (total, tail_text(&tail_str, max_tail_chars))
}

pub(super) fn tail_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let tail = text
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}

#[cfg(test)]
mod tests {
    use super::{
        BOUNDED_OUTPUT_MAX_BYTES, BOUNDED_OUTPUT_MAX_LINES, BOUNDED_OUTPUT_RETAIN_BYTES,
        BoundedOutputAccumulator, RAW_STREAM_MAX_BYTES, RawOutputBuffer, SharedRawOutput,
        tail_from_buffer, take_delta_from_buffer,
    };
    use std::sync::{Arc, Mutex};

    fn raw(bytes: &[u8]) -> SharedRawOutput {
        let mut buffer = RawOutputBuffer::new();
        buffer.append(bytes);
        Arc::new(Mutex::new(buffer))
    }

    fn append(buffer: &SharedRawOutput, bytes: &[u8]) {
        buffer.lock().unwrap().append(bytes);
    }

    #[test]
    fn delta_holds_back_an_incomplete_trailing_utf8_sequence() {
        // "宽" is three bytes; deliver two of them, then the rest.
        let wide = "宽".as_bytes();
        let buffer = raw(b"ok ");
        append(&buffer, &wide[..2]);
        let mut cursor = 0usize;

        let (delta, total) = take_delta_from_buffer(&buffer, &mut cursor);
        assert_eq!(
            String::from_utf8(delta).expect("delta must be whole characters"),
            "ok "
        );
        assert_eq!(total, 5, "total still reports every buffered byte");
        assert_eq!(cursor, 3, "the split character stays unread");

        append(&buffer, &wide[2..]);
        let (delta, _) = take_delta_from_buffer(&buffer, &mut cursor);
        assert_eq!(
            String::from_utf8(delta).expect("delta must be whole characters"),
            "宽"
        );
    }

    #[test]
    fn delta_does_not_stall_on_genuinely_invalid_bytes() {
        // A lone 0xFF is never a valid start byte: passing it through keeps
        // binary output flowing instead of parking the cursor forever.
        let buffer = raw(&[b'a', 0xFF, b'b']);
        let mut cursor = 0usize;
        let (delta, total) = take_delta_from_buffer(&buffer, &mut cursor);
        assert_eq!(delta, vec![b'a', 0xFF, b'b']);
        assert_eq!(cursor, total);
    }

    // === #5472: in-memory retention bounds for the raw `Bash` streams ===

    #[test]
    fn raw_buffer_caps_in_flight_bytes_and_keeps_the_total_honest() {
        let mut buffer = RawOutputBuffer::with_cap(1_024);
        // 4 MiB through a 1 KiB cap: the analogue of `cargo build -v` through
        // the 16 MiB production ceiling.
        for _ in 0..1_024 {
            buffer.append(&[b'x'; 4_096]);
        }
        let produced = 1_024 * 4_096;
        assert_eq!(
            buffer.total_len(),
            produced,
            "the stream's length must survive the bound"
        );
        assert_eq!(buffer.dropped(), produced - buffer.retained().len());
        assert!(
            buffer.retained().len() <= 1_024 + 1_024 / 4,
            "retained {} exceeded cap + slack",
            buffer.retained().len()
        );
    }

    #[test]
    fn raw_buffer_release_collapses_to_a_tail_and_reports_the_omission() {
        let mut buffer = RawOutputBuffer::new();
        buffer.append(&[b'y'; 200_000]);
        assert_eq!(buffer.dropped(), 0, "200 KB is under the in-flight ceiling");

        buffer.release_to_tail(1_000);
        assert_eq!(buffer.retained().len(), 1_000);
        assert_eq!(buffer.dropped(), 199_000);
        assert_eq!(
            buffer.total_len(),
            200_000,
            "releasing memory must not rewrite how much the command printed"
        );
    }

    #[test]
    fn raw_buffer_never_retains_a_split_character() {
        let mut buffer = RawOutputBuffer::with_cap(8);
        // Each "宽" is 3 bytes, so a byte-exact tail would land mid-character.
        for _ in 0..64 {
            buffer.append("宽".as_bytes());
        }
        assert!(
            std::str::from_utf8(buffer.retained()).is_ok(),
            "front-drop must snap off continuation bytes"
        );

        let mut released = RawOutputBuffer::new();
        for _ in 0..64 {
            released.append("宽".as_bytes());
        }
        released.release_to_tail(10);
        assert!(std::str::from_utf8(released.retained()).is_ok());
    }

    #[test]
    fn delta_skips_bytes_the_bound_already_discarded() {
        // A consumer that stops reading while output keeps arriving must be
        // moved forward, not handed the retained tail as if it were new bytes.
        let buffer = Arc::new(Mutex::new(RawOutputBuffer::with_cap(16)));
        append(&buffer, b"first-chunk-that-will-be-dropped-entirely");
        let mut cursor = 0usize;
        let (delta, total) = take_delta_from_buffer(&buffer, &mut cursor);
        let dropped = buffer.lock().unwrap().dropped();
        assert!(dropped > 0, "the cap must have clipped the front");
        assert_eq!(cursor, total, "cursor lands at the stream's true position");
        assert_eq!(
            delta.len(),
            total - dropped,
            "only bytes still held can be delivered"
        );

        append(&buffer, b"tail");
        let (delta, _) = take_delta_from_buffer(&buffer, &mut cursor);
        assert_eq!(
            delta,
            b"tail".to_vec(),
            "subsequent deltas continue from the corrected cursor"
        );
    }

    #[test]
    fn tail_reports_the_stream_total_not_the_retained_length() {
        let buffer = Arc::new(Mutex::new(RawOutputBuffer::new()));
        append(&buffer, b"abcdefghij");
        buffer.lock().unwrap().release_to_tail(4);
        let (total, tail) = tail_from_buffer(&buffer, 100);
        assert_eq!(total, 10, "stdout_len must not shrink when memory is freed");
        assert_eq!(tail, "ghij");
    }

    #[test]
    fn abandoning_a_stream_releases_it_and_stops_the_reader() {
        let mut buffer = RawOutputBuffer::new();
        assert!(buffer.append(&[b'a'; 5_000]), "a live stream keeps reading");
        buffer.abandon();

        assert_eq!(buffer.retained().len(), 0, "held bytes are released");
        assert_eq!(
            buffer.total_len(),
            5_000,
            "the stream's length survives the release"
        );
        assert!(
            !buffer.append(&[b'b'; 100]),
            "an abandoned stream tells the reader thread to exit"
        );
        assert_eq!(buffer.retained().len(), 0, "and retains nothing further");
        assert_eq!(
            buffer.total_len(),
            5_100,
            "bytes that arrive after the give-up are still counted, not hidden"
        );
    }

    #[test]
    fn raw_stream_ceiling_clears_every_downstream_bound() {
        // The clip must be unreachable by the model-visible surfaces: the 30 KB
        // result truncation, the 1,200-char job tail, the 1 KiB completion tail.
        const { assert!(RAW_STREAM_MAX_BYTES > 30_000 * 100) };
        const { assert!(super::RAW_STREAM_SETTLED_TAIL_BYTES > 30_000) };
    }

    #[test]
    fn bounded_output_keeps_last_two_thousand_complete_lines() {
        let source = (0..=BOUNDED_OUTPUT_MAX_LINES)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = BoundedOutputAccumulator::new_in(None);
        output.append(source.as_bytes()).expect("append");
        output.finish().expect("finish");
        let snapshot = output.snapshot(true).expect("snapshot");
        assert!(snapshot.truncated);
        assert!(snapshot.content.starts_with("line-1\n"));
        assert!(snapshot.content.contains("Showing lines 2-2001 of 2001"));
    }

    #[test]
    fn bounded_output_streams_raw_full_output_and_bounds_decoded_tail() {
        let raw = vec![0xFF; 2 * 1024 * 1024];
        let mut output = BoundedOutputAccumulator::new_in(None);
        for chunk in raw.chunks(4_096) {
            output.append(chunk).expect("append");
            assert!(output.retained_memory_bytes() <= BOUNDED_OUTPUT_MAX_BYTES + 4);
        }
        output.finish().expect("finish");
        let snapshot = output.snapshot(true).expect("snapshot");
        assert!(snapshot.truncated);
        assert!(snapshot.retained_bytes <= BOUNDED_OUTPUT_MAX_BYTES);
        assert!(snapshot.content.contains('\u{FFFD}'));
        let path = output
            .full_output_path()
            .expect("full output")
            .to_path_buf();
        assert_eq!(std::fs::read(&path).expect("read full output"), raw);
        drop(output);
        std::fs::remove_file(path).expect("remove full output");
    }

    #[test]
    fn bounded_output_delta_since_returns_only_new_bytes() {
        let mut output = BoundedOutputAccumulator::new_in(None);
        output.append(b"first\n").expect("append");
        let cursor = output.total_bytes();
        assert_eq!(output.delta_since(0).expect("delta").0, "first\n");
        output.append(b"second\n").expect("append");
        assert_eq!(
            output.delta_since(cursor).expect("delta"),
            ("second\n".to_string(), 0)
        );
        assert_eq!(
            output.delta_since(output.total_bytes()).expect("delta"),
            (String::new(), 0)
        );

        // Past the memory bound: the dropped span is counted, and a clipped
        // multi-byte character at the front is not emitted as garbage.
        let mut output = BoundedOutputAccumulator::new_in(None);
        // One ASCII byte first puts the retained tail's front mid-character.
        output.append(b"a").expect("append");
        output
            .append("é".repeat(BOUNDED_OUTPUT_MAX_BYTES).as_bytes())
            .expect("append");
        let total = output.total_bytes();
        let (delta, omitted) = output.delta_since(0).expect("delta");
        assert!(!delta.contains('\u{FFFD}'));
        assert_eq!(delta.len() + omitted, total);
        assert!(delta.len() <= BOUNDED_OUTPUT_RETAIN_BYTES);
    }

    /// The notice for dropped delta bytes says where the full output is.
    #[test]
    fn bounded_output_omitted_notice_points_at_the_spill_file() {
        let output = BoundedOutputAccumulator::new_in(None);
        let spill = output
            .temp
            .as_ref()
            .expect("spill file")
            .path()
            .display()
            .to_string();
        let notice = output.omitted_notice(42);
        assert!(notice.starts_with("[42 bytes"), "{notice}");
        assert!(notice.contains(&spill), "{notice}");

        let missing = tempfile::tempdir().expect("tempdir").path().join("gone");
        let unspilled = BoundedOutputAccumulator::new_in(Some(&missing));
        let notice = unspilled.omitted_notice(7);
        assert!(notice.contains("not persisted"), "{notice}");
    }

    #[test]
    fn bounded_output_huge_terminal_line_matches_upstream_notice() {
        let mut source = vec![b'x'; BOUNDED_OUTPUT_MAX_BYTES + 1_024];
        source.push(b'\n');
        let mut output = BoundedOutputAccumulator::new_in(None);
        output.append(&source).expect("append");
        output.finish().expect("finish");
        let snapshot = output.snapshot(true).expect("snapshot");
        assert!(snapshot.content.contains("Showing last 50.0KB of line 1"));
        assert!(snapshot.content.contains("line is 0B"));
        let path = output
            .full_output_path()
            .expect("full output")
            .to_path_buf();
        drop(output);
        std::fs::remove_file(path).expect("remove full output");
    }

    #[test]
    fn spill_failure_is_soft_and_names_the_reason() {
        // A missing spill dir simulates a full or broken temp volume: the
        // stream still runs, the tail is still delivered, and the notice says
        // why "Full output: <path>" is absent instead of failing the command.
        let missing =
            std::env::temp_dir().join(format!("codewhale-missing-spill-{}", std::process::id()));
        let mut output = BoundedOutputAccumulator::new_in(Some(&missing));
        let reason = output.spill_unavailable().expect("spill unavailable");
        assert!(!reason.is_empty(), "reason must name the io error");

        output.append(b"ok\n").expect("append works without spill");
        output.finish().expect("finish works without spill");
        let short = output.snapshot(true).expect("snapshot");
        assert_eq!(short.content, "ok\n");
        assert!(!short.truncated);
        assert!(output.full_output_path().is_none());

        let source = (0..=BOUNDED_OUTPUT_MAX_LINES)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = BoundedOutputAccumulator::new_in(Some(&missing));
        output.append(source.as_bytes()).expect("append");
        output.finish().expect("finish");
        let snapshot = output.snapshot(true).expect("snapshot");
        assert!(snapshot.truncated);
        assert!(snapshot.content.starts_with("line-1\n"));
        assert!(
            snapshot.content.contains("Full output was not persisted:"),
            "{}",
            snapshot.content
        );
        assert!(!snapshot.content.contains("Full output: "));
        assert!(output.full_output_path().is_none());
    }

    #[test]
    fn resource_exhaustion_hint_names_disk_descriptors_and_memory() {
        use std::io::{Error, ErrorKind};
        assert!(
            super::resource_exhaustion_hint(&Error::from(ErrorKind::StorageFull))
                .expect("storage full")
                .contains("disk")
        );
        assert!(
            super::resource_exhaustion_hint(&Error::from(ErrorKind::OutOfMemory))
                .expect("oom")
                .contains("memory")
        );
        #[cfg(unix)]
        {
            assert!(
                super::resource_exhaustion_hint(&Error::from_raw_os_error(libc::ENOSPC))
                    .expect("enospc")
                    .contains("disk")
            );
            assert!(
                super::resource_exhaustion_hint(&Error::from_raw_os_error(libc::EMFILE))
                    .expect("emfile")
                    .contains("file descriptors")
            );
            assert!(
                super::resource_exhaustion_hint(&Error::from_raw_os_error(libc::EAGAIN))
                    .expect("eagain")
                    .contains("retry")
            );
        }
        assert!(super::resource_exhaustion_hint(&Error::from(ErrorKind::NotFound)).is_none());
        assert!(
            super::resource_exhaustion_hint(&Error::from(ErrorKind::PermissionDenied)).is_none()
        );
    }
}
