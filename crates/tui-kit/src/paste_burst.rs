//! Paste-burst detection for terminals without reliable bracketed paste.

use std::time::{Duration, Instant};

const PASTE_BURST_MIN_CHARS: u16 = 3;
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
#[cfg(not(windows))]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Default)]
pub struct PasteBurst {
    last_plain_char_time: Option<Instant>,
    consecutive_plain_char_burst: u16,
    burst_window_until: Option<Instant>,
    buffer: String,
    active: bool,
    pending_first_char: Option<(char, Instant)>,
}

pub enum CharDecision {
    BeginBuffer,
    BufferAppend,
    RetainFirstChar,
    BeginBufferFromPending,
}

pub enum FlushResult {
    Paste(String),
    Typed(char),
    /// Enter can submit again even though the composer text has not changed.
    SuppressionExpired,
    None,
}

impl PasteBurst {
    #[cfg(any(test, feature = "test-support"))]
    pub fn recommended_flush_delay() -> Duration {
        PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(1)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn recommended_active_flush_delay() -> Duration {
        PASTE_BURST_ACTIVE_IDLE_TIMEOUT + Duration::from_millis(1)
    }

    pub fn on_plain_char(&mut self, ch: char, now: Instant) -> CharDecision {
        self.note_plain_char(now);

        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BufferAppend;
        }

        if let Some((held, held_at)) = self.pending_first_char
            && now.duration_since(held_at) <= PASTE_BURST_CHAR_INTERVAL
        {
            self.active = true;
            let _ = self.pending_first_char.take();
            self.buffer.push(held);
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BeginBufferFromPending;
        }

        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return CharDecision::BeginBuffer;
        }

        self.pending_first_char = Some((ch, now));
        CharDecision::RetainFirstChar
    }

    pub fn note_plain_char(&mut self, now: Instant) -> u16 {
        match self.last_plain_char_time {
            Some(prev) if now.duration_since(prev) <= PASTE_BURST_CHAR_INTERVAL => {
                self.consecutive_plain_char_burst =
                    self.consecutive_plain_char_burst.saturating_add(1);
            }
            _ => self.consecutive_plain_char_burst = 1,
        }
        self.last_plain_char_time = Some(now);
        self.consecutive_plain_char_burst
    }

    pub fn flush_if_due(&mut self, now: Instant) -> FlushResult {
        let suppression_expired = self.burst_window_until.is_some_and(|until| now > until);
        if suppression_expired {
            self.burst_window_until = None;
        }
        let unchanged = if suppression_expired {
            FlushResult::SuppressionExpired
        } else {
            FlushResult::None
        };
        let timeout = if self.is_active_internal() {
            PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        } else {
            PASTE_BURST_CHAR_INTERVAL
        };
        let timed_out = self
            .last_plain_char_time
            .is_some_and(|t| now.duration_since(t) > timeout);

        if timed_out && self.is_active_internal() {
            self.active = false;
            let out = std::mem::take(&mut self.buffer);
            // `burst_window_until` intentionally survives the flush: the idle
            // timeout is only 8ms, and a paste's trailing newline can land
            // just after it over a laggy link (SSH/tmux). Dropping the window
            // here would let that pasted newline submit a partial paste
            // (#1073). The window stays *bounded* instead: absorbing an Enter
            // outside an active burst no longer re-arms it, so suppression
            // always ends `PASTE_ENTER_SUPPRESS_WINDOW` after the last real
            // keystroke.
            FlushResult::Paste(out)
        } else if timed_out {
            if let Some((ch, _)) = self.pending_first_char.take() {
                FlushResult::Typed(ch)
            } else {
                unchanged
            }
        } else {
            unchanged
        }
    }

    /// Wake for pending input or the one redraw that re-enables submission.
    /// Once both are settled, typing must not leave a zero-delay poll loop.
    #[must_use]
    pub fn next_flush_delay(&self, now: Instant) -> Option<Duration> {
        if self.is_active() {
            let last = self.last_plain_char_time?;
            let timeout = if self.is_active_internal() {
                PASTE_BURST_ACTIVE_IDLE_TIMEOUT
            } else {
                PASTE_BURST_CHAR_INTERVAL
            };
            return Some(timeout.saturating_sub(now.duration_since(last)));
        }
        self.burst_window_until
            .map(|until| (until + Duration::from_millis(1)).saturating_duration_since(now))
    }

    pub fn append_newline_if_active(&mut self, now: Instant) -> bool {
        if self.is_active() {
            // A held first char precedes this newline in arrival order; move
            // it into the burst first or it would land after the newline
            // ("a\nb" became "\nab").
            if let Some((held, _)) = self.pending_first_char.take() {
                self.buffer.push(held);
                self.active = true;
            }
            self.buffer.push('\n');
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            true
        } else {
            false
        }
    }

    pub fn newline_should_insert_instead_of_submit(&self, now: Instant) -> bool {
        let in_burst_window = self.burst_window_until.is_some_and(|until| now <= until);
        self.is_active() || in_burst_window
    }

    pub fn extend_window(&mut self, now: Instant) {
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    /// Begin buffering from the current char, leaving already-typed text
    /// untouched. The burst never rewrites the composer (Y-7): chars before
    /// the burst stay exactly as the user typed them.
    pub fn begin_buffer_from_now(&mut self, ch: char, now: Instant) {
        self.buffer.push(ch);
        self.active = true;
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    /// The text the heuristic currently holds on the user's behalf: the
    /// held first char plus the burst buffer, in arrival order. Command
    /// context must be judged on this — while a burst is held the composer
    /// can be empty even though the user is mid-command (Y-7).
    pub fn held_text(&self) -> String {
        let mut out = String::new();
        if let Some((ch, _)) = self.pending_first_char {
            out.push(ch);
        }
        out.push_str(&self.buffer);
        out
    }

    pub fn append_char_to_buffer(&mut self, ch: char, now: Instant) {
        self.buffer.push(ch);
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    pub fn flush_before_modified_input(&mut self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        self.active = false;
        let mut out = std::mem::take(&mut self.buffer);
        if let Some((ch, _)) = self.pending_first_char.take() {
            out.push(ch);
        }
        Some(out)
    }

    /// Reset burst-accumulation state without clearing the suppression window.
    ///
    /// Used when a non-char key (Tab, etc.) arrives during an active burst as
    /// part of table-data paste. The buffer was flushed upstream; only the
    /// active state is reset so `burst_window_until` stays alive and a trailing
    /// Enter is still absorbed as a newline (#2134).
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `buffer` is non-empty — the caller must flush
    /// via `flush_before_modified_input` first.
    pub fn deactivate_keep_window(&mut self) {
        debug_assert!(
            self.buffer.is_empty(),
            "buffer must be flushed before deactivating"
        );
        self.consecutive_plain_char_burst = 0;
        self.last_plain_char_time = None;
        self.active = false;
        self.pending_first_char = None;
        // burst_window_until intentionally NOT cleared
    }

    pub fn is_active(&self) -> bool {
        self.is_active_internal() || self.pending_first_char.is_some()
    }

    fn is_active_internal(&self) -> bool {
        self.active || !self.buffer.is_empty()
    }

    pub fn clear_after_explicit_paste(&mut self) {
        self.last_plain_char_time = None;
        self.consecutive_plain_char_burst = 0;
        self.burst_window_until = None;
        self.active = false;
        self.buffer.clear();
        self.pending_first_char = None;
    }

    /// Arm the Enter-suppression window for a non-ASCII character that was
    /// inserted straight into the composer instead of being buffered (the
    /// IME / raw-CJK path in `tui::paste`).
    ///
    /// `rapid_chars` is the run length reported by [`Self::note_plain_char`].
    ///
    /// A *lone* commit only earns a burst-interval window. An IME candidate
    /// commit is ordinary typing: the user may press Enter to send a
    /// message ending in a CJK character tens of milliseconds later, and the
    /// full 120ms window turned that Enter into a stray newline. A real raw
    /// paste delivers its trailing newline within microseconds of the last
    /// character, so the short window still absorbs it — including the
    /// single-character first line of a CJK paste (#1302).
    ///
    /// Two or more characters at paste speed mean the stream *is* a paste,
    /// so the full window applies and later lines stay absorbed.
    pub fn arm_window_for_direct_char(&mut self, now: Instant, rapid_chars: u16) {
        if rapid_chars >= 2 {
            self.extend_window(now);
        } else {
            self.burst_window_until = Some(now + PASTE_BURST_CHAR_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_first_char_is_held_then_flushes_as_typed() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();
        assert!(matches!(
            burst.on_plain_char('a', t0),
            CharDecision::RetainFirstChar
        ));

        let t1 = t0 + PasteBurst::recommended_flush_delay() + Duration::from_millis(1);
        assert!(matches!(burst.flush_if_due(t1), FlushResult::Typed('a')));
        assert!(!burst.is_active());
    }

    #[test]
    fn ascii_two_fast_chars_start_buffer_from_pending_and_flush_as_paste() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();
        assert!(matches!(
            burst.on_plain_char('a', t0),
            CharDecision::RetainFirstChar
        ));

        let t1 = t0 + Duration::from_millis(1);
        assert!(matches!(
            burst.on_plain_char('b', t1),
            CharDecision::BeginBufferFromPending
        ));
        burst.append_char_to_buffer('b', t1);

        let t2 = t1 + PasteBurst::recommended_active_flush_delay() + Duration::from_millis(1);
        assert!(matches!(
            burst.flush_if_due(t2),
            FlushResult::Paste(ref s) if s == "ab"
        ));
    }

    #[test]
    fn newline_after_held_first_char_keeps_arrival_order() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();
        assert!(matches!(
            burst.on_plain_char('1', t0),
            CharDecision::RetainFirstChar
        ));
        let t1 = t0 + Duration::from_millis(1);
        assert!(burst.append_newline_if_active(t1));
        let t2 = t1 + Duration::from_millis(1);
        let decision = burst.on_plain_char('2', t2);
        assert!(matches!(
            decision,
            CharDecision::BufferAppend | CharDecision::BeginBufferFromPending
        ));
        burst.append_char_to_buffer('2', t2);
        let t3 = t2 + PasteBurst::recommended_active_flush_delay() + Duration::from_millis(1);
        assert!(matches!(
            burst.flush_if_due(t3),
            FlushResult::Paste(ref s) if s == "1\n2"
        ));
    }

    #[test]
    fn flush_before_modified_input_includes_pending_first_char() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();
        assert!(matches!(
            burst.on_plain_char('a', t0),
            CharDecision::RetainFirstChar
        ));

        assert_eq!(burst.flush_before_modified_input(), Some("a".to_string()));
        assert!(!burst.is_active());
    }

    #[test]
    fn settled_input_stops_polling_and_expiry_requests_one_redraw() {
        let mut burst = PasteBurst::default();
        let now = Instant::now();
        let _ = burst.on_plain_char('a', now);
        assert!(matches!(
            burst.flush_if_due(now + Duration::from_millis(20)),
            FlushResult::Typed('a')
        ));
        assert_eq!(
            burst.next_flush_delay(now + Duration::from_millis(20)),
            None
        );

        burst.extend_window(now);
        let inside = now + Duration::from_millis(100);
        assert!(burst.newline_should_insert_instead_of_submit(inside));
        assert_eq!(
            burst.next_flush_delay(inside),
            Some(Duration::from_millis(21))
        );
        let expired = now + Duration::from_millis(121);
        assert!(matches!(
            burst.flush_if_due(expired),
            FlushResult::SuppressionExpired
        ));
        assert!(!burst.newline_should_insert_instead_of_submit(expired));
        assert_eq!(burst.next_flush_delay(expired), None);
        assert!(matches!(burst.flush_if_due(expired), FlushResult::None));
    }

    #[test]
    fn next_flush_delay_counts_down_to_zero() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();
        let _ = burst.on_plain_char('a', t0);

        let almost_due = t0 + Duration::from_millis(7);
        let remaining = burst
            .next_flush_delay(almost_due)
            .expect("delay should exist");
        assert!(remaining <= Duration::from_millis(1));

        let due = t0 + Duration::from_millis(20);
        assert_eq!(burst.next_flush_delay(due), Some(Duration::ZERO));
    }

    /// Simulate #2134: when a non-char key (Tab) arrives during table-data
    /// paste, `deactivate_keep_window` resets accumulation state but
    /// preserves the Enter-suppression window so a trailing newline is still
    /// absorbed instead of submitting the partial input.
    #[test]
    fn deactivate_keep_window_preserves_enter_suppression_window() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        assert!(matches!(
            burst.on_plain_char('a', t0),
            CharDecision::RetainFirstChar
        ));
        let t1 = t0 + Duration::from_millis(1);
        assert!(matches!(
            burst.on_plain_char('b', t1),
            CharDecision::BeginBufferFromPending
        ));
        burst.append_char_to_buffer('b', t1);
        assert!(burst.is_active());
        assert!(burst.newline_should_insert_instead_of_submit(t1));

        let flushed = burst.flush_before_modified_input();
        assert!(flushed.is_some());
        assert!(!burst.is_active());

        burst.deactivate_keep_window();

        assert!(!burst.is_active());

        let t_tab = t1 + Duration::from_millis(2);
        assert!(
            burst.newline_should_insert_instead_of_submit(t_tab),
            "Enter within suppression window should insert newline, not submit"
        );

        let t_expired = t_tab + PASTE_ENTER_SUPPRESS_WINDOW + Duration::from_millis(1);
        assert!(
            !burst.newline_should_insert_instead_of_submit(t_expired),
            "Enter after suppression window expires should submit"
        );
    }

    /// The idle flush must NOT drop the Enter-suppression window. The active
    /// idle timeout is only 8ms, so a paste's trailing newline can easily
    /// land just after the flush on a laggy link — dropping the window there
    /// would submit a partial paste (#1073).
    #[test]
    fn idle_flush_keeps_enter_suppression_window_alive() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        let _ = burst.on_plain_char('a', t0);
        let t1 = t0 + Duration::from_millis(1);
        assert!(matches!(
            burst.on_plain_char('b', t1),
            CharDecision::BeginBufferFromPending
        ));
        burst.append_char_to_buffer('b', t1);

        let t_flush = t1 + PasteBurst::recommended_active_flush_delay();
        assert!(matches!(
            burst.flush_if_due(t_flush),
            FlushResult::Paste(ref s) if s == "ab"
        ));
        assert!(!burst.is_active());
        assert!(
            burst.newline_should_insert_instead_of_submit(t_flush),
            "a trailing pasted newline arriving right after the idle flush \
             must still be absorbed instead of submitting"
        );
    }

    /// …but the window is *bounded*: it expires 120ms after the last real
    /// keystroke and nothing about the flush re-arms it, so the user's next
    /// Enter submits.
    #[test]
    fn enter_suppression_window_expires_after_the_last_keystroke() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        let _ = burst.on_plain_char('a', t0);
        let t1 = t0 + Duration::from_millis(1);
        let _ = burst.on_plain_char('b', t1);
        burst.append_char_to_buffer('b', t1);
        let t_flush = t1 + PasteBurst::recommended_active_flush_delay();
        let _ = burst.flush_if_due(t_flush);

        let t_late = t1 + PASTE_ENTER_SUPPRESS_WINDOW + Duration::from_millis(1);
        assert!(
            !burst.newline_should_insert_instead_of_submit(t_late),
            "Enter more than the suppression window after the paste must submit"
        );
    }

    /// A lone IME candidate commit is ordinary typing: it may only hold Enter
    /// for one burst interval, so a user finishing a CJK sentence and
    /// pressing Enter actually sends.
    #[test]
    fn lone_non_ascii_commit_arms_only_a_burst_interval_window() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        let rapid = burst.note_plain_char(t0);
        assert_eq!(rapid, 1, "an isolated commit is a run of one");
        burst.arm_window_for_direct_char(t0, rapid);

        assert!(
            burst.newline_should_insert_instead_of_submit(t0 + PASTE_BURST_CHAR_INTERVAL),
            "a raw paste delivers its trailing newline within the burst \
             interval and must still be absorbed (#1302)"
        );
        assert!(
            !burst.newline_should_insert_instead_of_submit(
                t0 + PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(1)
            ),
            "an IME commit must not swallow the Enter a human presses \
             tens of milliseconds later"
        );
    }

    /// Two non-ASCII characters at paste speed mean the stream is a paste,
    /// so the full suppression window applies to later lines.
    #[test]
    fn rapid_non_ascii_run_arms_the_full_suppression_window() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        let rapid = burst.note_plain_char(t0);
        burst.arm_window_for_direct_char(t0, rapid);
        let t1 = t0 + Duration::from_millis(1);
        let rapid = burst.note_plain_char(t1);
        assert_eq!(rapid, 2);
        burst.arm_window_for_direct_char(t1, rapid);

        assert!(
            burst.newline_should_insert_instead_of_submit(t1 + PASTE_ENTER_SUPPRESS_WINDOW),
            "a raw CJK paste must keep absorbing its embedded newlines"
        );
        assert!(
            !burst.newline_should_insert_instead_of_submit(
                t1 + PASTE_ENTER_SUPPRESS_WINDOW + Duration::from_millis(1)
            ),
            "even a paste-speed run releases Enter once the window lapses"
        );
    }

    /// A slow IME sequence never accumulates a rapid run, so every commit
    /// re-arms only the short window and Enter stays available throughout.
    #[test]
    fn slow_ime_sequence_never_holds_enter() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        // "你好世界" committed one character at a time with human gaps.
        for i in 0..4u64 {
            let now = t0 + Duration::from_millis(50 * i);
            let rapid = burst.note_plain_char(now);
            assert_eq!(rapid, 1, "50ms gaps are never a paste-speed run");
            burst.arm_window_for_direct_char(now, rapid);
        }

        let last = t0 + Duration::from_millis(150);
        assert!(
            !burst.newline_should_insert_instead_of_submit(last + Duration::from_millis(30)),
            "Enter after an IME-typed CJK message must submit"
        );
    }
}
