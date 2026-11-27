//! Toasts: one line, a mark and a sentence, stacked at the bottom right.
//!
//! The rendering half of the engine's `StatusToast` (`crates/tui/src/tui/
//! app/status.rs`, `Hmbown/CodeWhale` `58b1dd3dd`), plus the clock the engine
//! keeps beside it: a [`Ttl`] and a `born` instant the caller supplies, so
//! nothing here reads the time. Painting is a pure function of what the host
//! passes in ([`Toasts::at`]); expiry is [`Toasts::retain_live`], called by
//! the host with its own `now`. Failures and anything that needs the person
//! stay until they have been seen ([`Ttl::UntilSeen`]); a stack caps at
//! [`Toasts::max_visible`] and says `+n more` with the real n.
//!
//! A toast arrives and leaves in steps of ink (`Dim`, `Hint`, `Muted`, then
//! `Foreground`), never by moving, and only under [`MotionMode::Full`]:
//! reduced and still motion show it at full ink at once.

use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{KeyHint, MotionMode, Paint, Role, State, StatusMark, Theme, text};

/// How long a toast lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ttl {
    /// This many seconds after it was born.
    Seconds(u16),
    /// Until the person has seen it ([`Toasts::mark_seen`]), then
    /// [`TOAST_SEEN_LINGER`] more: a failure must not vanish unread.
    UntilSeen,
}

impl Ttl {
    /// What [`Toast::new`] gives a state: failures, blocked states, and things
    /// that need the person stay until seen; everything else lasts five seconds.
    #[must_use]
    pub const fn for_state(state: State) -> Self {
        match state {
            State::Failed | State::NeedsYou | State::Blocked => Self::UntilSeen,
            _ => Self::Seconds(5),
        }
    }
}

/// How long a seen [`Ttl::UntilSeen`] toast stays before it goes.
pub const TOAST_SEEN_LINGER: Duration = Duration::from_secs(2);
/// How long a timed error toast ([`Toast::timed_error`]) lasts before expiring,
/// matching Desktop Wave 9 (`8000 ms`).
pub const TOAST_ERROR_SECONDS: u16 = 8;
/// A toast arrives over this long, in three steps of ink.
pub const TOAST_FADE_IN: Duration = Duration::from_millis(180);
/// A toast leaves over this long, in three steps of ink.
pub const TOAST_FADE_OUT: Duration = Duration::from_millis(300);

/// One notice: `✓ Theme set to Shoreline`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub state: State,
    pub text: Cow<'static, str>,
    /// Draws `→`: there is more to open.
    pub opens: bool,
    /// Optional right-aligned keyboard action hint.
    pub action: Option<KeyHint>,
    pub ttl: Ttl,
    /// When the host raised it. `None` never expires by time and never fades.
    pub born: Option<Instant>,
    /// When the person saw it, for [`Ttl::UntilSeen`].
    pub seen: Option<Instant>,
}

impl Toast {
    #[must_use]
    pub fn new(state: State, text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            state,
            text: text.into(),
            opens: false,
            action: None,
            ttl: Ttl::for_state(state),
            born: None,
            seen: None,
        }
    }

    /// A completed-action toast (`State::Done`, 5-second TTL).
    #[must_use]
    pub fn success(text: impl Into<Cow<'static, str>>) -> Self {
        Self::new(State::Done, text)
    }

    /// An attention-needed toast (`State::NeedsYou`, [`Ttl::UntilSeen`]).
    #[must_use]
    pub fn attention(text: impl Into<Cow<'static, str>>) -> Self {
        Self::new(State::NeedsYou, text)
    }

    /// A failure toast (`State::Failed`, [`Ttl::UntilSeen`]).
    #[must_use]
    pub fn error(text: impl Into<Cow<'static, str>>) -> Self {
        Self::new(State::Failed, text)
    }

    /// A timed failure toast (`State::Failed`, [`TOAST_ERROR_SECONDS`] seconds).
    #[must_use]
    pub fn timed_error(text: impl Into<Cow<'static, str>>) -> Self {
        Self::new(State::Failed, text).ttl(Ttl::Seconds(TOAST_ERROR_SECONDS))
    }

    /// A quiet informational toast (`State::Ready`, 5-second TTL).
    #[must_use]
    pub fn info(text: impl Into<Cow<'static, str>>) -> Self {
        Self::new(State::Ready, text)
    }

    #[must_use]
    pub fn opens(mut self) -> Self {
        self.opens = true;
        self
    }

    /// Attach an optional right-aligned keyboard action hint to this toast.
    #[must_use]
    pub fn action(mut self, action: KeyHint) -> Self {
        self.action = Some(action);
        self
    }

    #[must_use]
    pub fn ttl(mut self, ttl: Ttl) -> Self {
        self.ttl = ttl;
        self
    }

    /// When the host raised this toast. Painting never reads a clock; the
    /// host passes the instant it measured.
    #[must_use]
    pub fn born(mut self, born: Instant) -> Self {
        self.born = Some(born);
        self
    }

    /// When this toast goes, if it is known yet: `born` plus the seconds, or
    /// the moment it was seen plus [`TOAST_SEEN_LINGER`]. `None` means not yet
    /// (unborn, or not seen).
    #[must_use]
    pub fn expires_at(&self) -> Option<Instant> {
        match self.ttl {
            Ttl::Seconds(s) => self.born.map(|b| b + Duration::from_secs(u64::from(s))),
            Ttl::UntilSeen => self.seen.map(|s| s + TOAST_SEEN_LINGER),
        }
    }

    /// Whether the toast still shows at `now`.
    #[must_use]
    pub fn is_live(&self, now: Instant) -> bool {
        self.expires_at().is_none_or(|end| now < end)
    }

    /// The ink the sentence takes at `now`: `Foreground` once settled, and
    /// `Dim`, `Hint` or `Muted` while it arrives or leaves. `Foreground`
    /// always unless `motion` is [`MotionMode::Full`] and the toast has a
    /// `born`.
    #[must_use]
    pub fn ink(&self, now: Instant, motion: MotionMode) -> Role {
        const STEPS: [Role; 3] = [Role::Dim, Role::Hint, Role::Muted];
        if !motion.animates() {
            return Role::Foreground;
        }
        let mut step = 3usize;
        if let Some(born) = self.born {
            let age = now.saturating_duration_since(born);
            if age < TOAST_FADE_IN {
                step = step.min((age.as_millis() / 60) as usize);
            }
        }
        if let Some(end) = self.expires_at()
            && let Some(left) = end.checked_duration_since(now)
            && left < TOAST_FADE_OUT
        {
            step = step.min((left.as_millis() / 100) as usize);
        }
        STEPS.get(step).copied().unwrap_or(Role::Foreground)
    }

    fn line(&self, max: usize, ink: Role, theme: &Theme) -> Line<'static> {
        let mark = StatusMark::new(self.state);
        let glyph = mark.glyph(theme);
        let arrow = if self.opens {
            if theme.ascii() { " >" } else { " →" }
        } else {
            ""
        };
        let fixed = 1 + text::width(glyph) + 1 + text::width(arrow) + 1;
        let safe_body = text::display_safe(&self.text);
        let min_body = text::width(&safe_body).min(4);
        let mut action_spans: Option<(Vec<Span<'static>>, usize)> = None;
        if let Some(hint) = &self.action {
            let keys = text::display_safe(&hint.keys).into_owned();
            let verb = text::display_safe(&hint.verb).into_owned();
            let keys_w = text::width(&keys);
            let verb_w = text::width(&verb);
            let avail_for_action = max.saturating_sub(fixed + min_body + 2);
            if keys_w > 0 && avail_for_action >= keys_w {
                let (key_style, verb_style) = if hint.enabled {
                    (
                        theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
                        theme.fg(Role::Muted),
                    )
                } else {
                    (
                        theme.fg(Role::Muted),
                        theme.fg(Role::Muted).add_modifier(Modifier::DIM),
                    )
                };
                if verb_w > 0 && avail_for_action >= keys_w + 1 + verb_w {
                    action_spans = Some((
                        vec![
                            Span::raw("  "),
                            Span::styled(keys, key_style),
                            Span::raw(" "),
                            Span::styled(verb, verb_style),
                        ],
                        2 + keys_w + 1 + verb_w,
                    ));
                } else if verb_w > 0 && avail_for_action >= keys_w + 2 {
                    let cut = text::truncate(&verb, avail_for_action - keys_w - 1, theme.ascii())
                        .into_owned();
                    let cut_w = text::width(&cut);
                    if cut_w > 0 {
                        action_spans = Some((
                            vec![
                                Span::raw("  "),
                                Span::styled(keys, key_style),
                                Span::raw(" "),
                                Span::styled(cut, verb_style),
                            ],
                            2 + keys_w + 1 + cut_w,
                        ));
                    } else {
                        action_spans = Some((
                            vec![Span::raw("  "), Span::styled(keys, key_style)],
                            2 + keys_w,
                        ));
                    }
                } else {
                    action_spans = Some((
                        vec![Span::raw("  "), Span::styled(keys, key_style)],
                        2 + keys_w,
                    ));
                }
            }
        }
        let action_w = action_spans.as_ref().map_or(0, |(_, w)| *w);
        let body = text::truncate_words(
            &safe_body,
            max.saturating_sub(fixed + action_w),
            theme.ascii(),
        )
        .into_owned();
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(glyph, theme.fg(self.state.role())),
            Span::raw(" "),
            Span::styled(body, theme.fg(ink)),
        ];
        if let Some((extra, _)) = action_spans {
            spans.extend(extra);
        }
        if !arrow.is_empty() {
            spans.push(Span::styled(arrow, theme.fg(Role::Muted)));
        }
        spans.push(Span::raw(" "));
        Line::from(spans)
    }
}

/// Words a toast stack prints. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastWords {
    /// `+2 more`.
    pub more: Cow<'static, str>,
}

impl Default for ToastWords {
    fn default() -> Self {
        Self {
            more: Cow::Borrowed("more"),
        }
    }
}

/// How many toasts a stack shows before it says `+n more`.
pub const TOAST_MAX_VISIBLE: usize = 3;

/// A stack of toasts anchored to the bottom right of an area, newest last
/// (nearest the horizon). At most [`Toasts::max_visible`] show, and the rows
/// available cap that further; older ones fold into a `+n more` line above
/// the stack, with n the real number hidden.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toasts {
    pub items: Vec<Toast>,
    pub max_visible: usize,
    /// The host's clock for this frame; with [`MotionMode::Full`] it lets
    /// toasts fade. `None` paints every toast settled.
    pub now: Option<Instant>,
    pub motion: MotionMode,
    pub words: ToastWords,
}

impl Default for Toasts {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Toasts {
    #[must_use]
    pub fn new(items: Vec<Toast>) -> Self {
        Self {
            items,
            max_visible: TOAST_MAX_VISIBLE,
            now: None,
            motion: MotionMode::Full,
            words: ToastWords::default(),
        }
    }

    #[must_use]
    pub fn max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = max_visible;
        self
    }

    /// Paint as of `now`, the host's clock for this frame.
    #[must_use]
    pub fn at(mut self, now: Instant) -> Self {
        self.now = Some(now);
        self
    }

    #[must_use]
    pub fn motion(mut self, motion: MotionMode) -> Self {
        self.motion = motion;
        self
    }

    #[must_use]
    pub fn words(mut self, words: ToastWords) -> Self {
        self.words = words;
        self
    }

    /// `(shown, hidden, more_row)` for a stack `rows` tall: the newest
    /// `shown` toasts draw, `hidden` older ones are folded, and `more_row`
    /// says whether the `+n more` line has a row. With one row the newest
    /// toast gets it and the count goes unspoken.
    #[must_use]
    pub fn window(&self, rows: u16) -> (usize, usize, bool) {
        let rows = usize::from(rows);
        let len = self.items.len();
        if len == 0 || rows == 0 {
            return (0, 0, false);
        }
        if len <= self.max_visible.min(rows) {
            return (len, 0, false);
        }
        if rows == 1 {
            return (1, len - 1, false);
        }
        let shown = self.max_visible.min(rows - 1);
        (shown, len - shown, true)
    }

    /// Drop the toasts that have expired at `now`, and say how many went.
    /// Failures stay until [`Toasts::mark_seen`] has run for them.
    pub fn retain_live(&mut self, now: Instant) -> usize {
        let before = self.items.len();
        self.items.retain(|t| t.is_live(now));
        before - self.items.len()
    }

    /// Record that the person saw the toasts a stack `rows` tall shows, so
    /// their [`Ttl::UntilSeen`] clocks start. Call it when the stack was
    /// really on screen.
    pub fn mark_seen(&mut self, now: Instant, rows: u16) {
        let (shown, _, _) = self.window(rows);
        let first = self.items.len() - shown;
        for toast in &mut self.items[first..] {
            toast.seen.get_or_insert(now);
        }
    }

    /// How long until anything about the stack changes (a toast expires or
    /// moves a step of ink), or `None` when nothing will: the host schedules
    /// its next redraw from this instead of polling.
    #[must_use]
    pub fn next_change_in(&self, now: Instant) -> Option<Duration> {
        let mut soonest: Option<Duration> = None;
        let mut consider = |at: Instant| {
            if let Some(after) = at.checked_duration_since(now)
                && !after.is_zero()
            {
                soonest = Some(soonest.map_or(after, |s| s.min(after)));
            }
        };
        for toast in &self.items {
            let end = toast.expires_at();
            if let Some(end) = end {
                consider(end);
            }
            if self.motion.animates() {
                if let Some(born) = toast.born {
                    for ms in [60, 120, 180] {
                        consider(born + Duration::from_millis(ms));
                    }
                }
                if let Some(end) = end {
                    for ms in [300, 200, 100] {
                        if let Some(at) = end.checked_sub(Duration::from_millis(ms)) {
                            consider(at);
                        }
                    }
                }
            }
        }
        soonest
    }
}

impl Paint for Toasts {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let max = usize::from(area.width);
        let (shown, hidden, more_row) = self.window(area.height);
        let first = self.items.len() - shown;
        let top = area.bottom() - shown as u16 - u16::from(more_row);
        if more_row {
            let more = format!(" +{hidden} {} ", text::display_safe(&self.words.more));
            let more = text::truncate(&more, max, theme.ascii()).into_owned();
            let w = u16::try_from(text::width(&more))
                .unwrap_or(u16::MAX)
                .min(area.width);
            let rect = Rect {
                x: area.right() - w,
                y: top,
                width: w,
                height: 1,
            };
            buf.set_style(rect, theme.bg(Role::Surface));
            Line::from(Span::styled(more, theme.fg(Role::Muted))).render(rect, buf);
        }
        for (row, toast) in self.items[first..].iter().enumerate() {
            let ink = match self.now {
                Some(now) => toast.ink(now, self.motion),
                None => Role::Foreground,
            };
            let line = toast.line(max, ink, theme);
            let w = u16::try_from(line.width())
                .unwrap_or(u16::MAX)
                .min(area.width);
            let rect = Rect {
                x: area.right() - w,
                y: top + u16::from(more_row) + row as u16,
                width: w,
                height: 1,
            };
            buf.set_style(rect, theme.bg(Role::Surface));
            line.render(rect, buf);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        let len = self.items.len();
        let rows = if len <= self.max_visible {
            len
        } else {
            self.max_visible + 1
        };
        u16::try_from(rows).unwrap_or(u16::MAX)
    }
}
