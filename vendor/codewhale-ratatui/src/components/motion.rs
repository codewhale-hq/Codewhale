//! Motion: the few things that move, and the promise that nothing else does.
//!
//! Motion is allowed for a change of state or for spatial continuity, and
//! nothing else: no sweeps, dissolves, glitches or bounces. This module is
//! that layer in a few hundred lines, instead of a dependency.
//!
//! - A [`MotionStep`] eases a value from where it is to a target in `0..=1`,
//!   sampled at an instant the caller passes in. Nothing here reads a clock,
//!   spawns a thread or waits.
//! - [`MotionPolicy`] says what may move. `Full` animates; `Reduced` and
//!   `Still` jump straight to the end state, and every helper takes the
//!   policy so a component cannot forget it. Color steps run at truecolor
//!   only: at 256 and 16 colors the in-between values snap into visible
//!   flicker, so the step is skipped (§4.3).
//! - [`MotionSet::next_frame_at`] is the whole scheduler: the next instant a
//!   redraw is due, never sooner than the frame cap, and `None` once
//!   everything has settled, so an idle screen asks for zero redraws.
//!
//! The host owns the loop. After each draw it calls `next_frame_at(now,
//! policy)` and sleeps until that instant, or until an event, whichever comes
//! first. `None` means sleep until an event.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use crate::{
    MotionMode, Paint, Role, Theme,
    color::{ColorDepth, blend},
    glyphs, text, tokens,
};

/// A redraw is never due sooner than this after it was asked for: 60 frames
/// a second. A step lasts 120 to 340 ms, so that is 7 to 20 frames.
pub const MOTION_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// The fastest cap a [`MotionSet`] accepts (120 frames a second, the
/// engine's draw cap). A faster request is raised to this.
pub const MOTION_MIN_FRAME_INTERVAL: Duration = Duration::from_micros(8_333);

/// How a step eases. Nothing overshoots: every curve stays inside `0..=1`
/// and never reverses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MotionEasing {
    Linear,
    /// Fast, then settling: for things arriving.
    #[default]
    EaseOut,
    /// Slow, then leaving: for things exiting.
    EaseIn,
}

impl MotionEasing {
    /// The eased progress for `t` in `0..=1`. Outside the range clamps; a
    /// NaN counts as finished.
    #[must_use]
    pub fn apply(self, t: f32) -> f32 {
        let t = if t.is_nan() { 1.0 } else { t.clamp(0.0, 1.0) };
        match self {
            Self::Linear => t,
            Self::EaseOut => 1.0 - (1.0 - t).powi(3),
            Self::EaseIn => t * t * t,
        }
    }
}

/// What a step moves. A color step needs a concrete truecolor ground; a
/// spatial step moves cells and works at any depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MotionChannel {
    /// A column offset or a reveal width.
    Space,
    /// A role blend.
    Tint,
}

/// How long a step takes and how it eases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionTiming {
    pub duration: Duration,
    pub easing: MotionEasing,
    pub channel: MotionChannel,
}

impl MotionTiming {
    /// A row's state changed (`duration_state`, 120 ms, ease out).
    pub const STATE: Self = Self::tint(
        Duration::from_millis(tokens::MOTION_DURATION_STATE_MS as u64),
        MotionEasing::EaseOut,
    );
    /// Something arrived (`duration_arrive`, 180 ms, ease out).
    pub const ARRIVE: Self = Self::tint(
        Duration::from_millis(tokens::MOTION_DURATION_ARRIVE_MS as u64),
        MotionEasing::EaseOut,
    );
    /// Something left (`duration_state`, 120 ms, ease in).
    pub const EXIT: Self = Self::tint(
        Duration::from_millis(tokens::MOTION_DURATION_STATE_MS as u64),
        MotionEasing::EaseIn,
    );
    /// A panel's width or position settles (`duration_panel`, 340 ms, ease
    /// out).
    pub const PANEL: Self = Self::space(
        Duration::from_millis(tokens::MOTION_DURATION_PANEL_MS as u64),
        MotionEasing::EaseOut,
    );

    /// A role blend.
    #[must_use]
    pub const fn tint(duration: Duration, easing: MotionEasing) -> Self {
        Self {
            duration,
            easing,
            channel: MotionChannel::Tint,
        }
    }

    /// A column offset or reveal width.
    #[must_use]
    pub const fn space(duration: Duration, easing: MotionEasing) -> Self {
        Self {
            duration,
            easing,
            channel: MotionChannel::Space,
        }
    }
}

/// What may move, from the person's setting and what the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionPolicy {
    mode: MotionMode,
    tints: bool,
}

impl MotionPolicy {
    /// `mode` on the terminal `theme` describes.
    #[must_use]
    pub fn new(mode: MotionMode, theme: &Theme) -> Self {
        Self {
            mode,
            tints: can_blend(theme),
        }
    }

    #[must_use]
    pub const fn mode(self) -> MotionMode {
        self.mode
    }

    /// Whether anything moves at all: `Full` only.
    #[must_use]
    pub const fn animates(self) -> bool {
        self.mode.animates()
    }

    /// Whether a step on `channel` runs, or jumps to its end state.
    #[must_use]
    pub const fn allows(self, channel: MotionChannel) -> bool {
        self.animates()
            && match channel {
                MotionChannel::Space => true,
                MotionChannel::Tint => self.tints,
            }
    }
}

/// Colors blend only at truecolor on a known ground: the in-between values
/// of a 256-color blend are other roles, and an unknown ground has nothing
/// to blend with.
fn can_blend(theme: &Theme) -> bool {
    theme.paints_grounds() && theme.depth() == ColorDepth::TrueColor
}

/// One value easing from where it is to a target, both in `0..=1`.
///
/// `0.0` is the start state (the `from` role, the home column, nothing
/// revealed) and `1.0` the end state. A new step starts at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionStep {
    from: f32,
    to: f32,
    start: Option<Instant>,
    duration: Duration,
    easing: MotionEasing,
}

impl Default for MotionStep {
    fn default() -> Self {
        Self::at_rest(0.0)
    }
}

impl MotionStep {
    /// A step that is not moving, at `value` (clamped to `0..=1`).
    #[must_use]
    pub fn at_rest(value: f32) -> Self {
        let value = unit(value);
        Self {
            from: value,
            to: value,
            start: None,
            duration: Duration::ZERO,
            easing: MotionEasing::Linear,
        }
    }

    /// Where the step is going.
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.to
    }

    /// The value at `now`, in `0..=1`. Before the start it is the start
    /// value; at and after the end it is the target.
    #[must_use]
    pub fn at(&self, now: Instant) -> f32 {
        let Some(start) = self.start else {
            return self.to;
        };
        let elapsed = now.saturating_duration_since(start);
        if self.duration.is_zero() || elapsed >= self.duration {
            return self.to;
        }
        let t = elapsed.as_secs_f32() / self.duration.as_secs_f32();
        unit(self.from + (self.to - self.from) * self.easing.apply(t))
    }

    /// Whether the step has reached its target at `now`.
    #[must_use]
    pub fn is_settled(&self, now: Instant) -> bool {
        self.start.is_none_or(|start| {
            self.duration.is_zero() || now.saturating_duration_since(start) >= self.duration
        })
    }

    /// Whether this step still needs frames: it is moving, and `policy`
    /// lets it move.
    #[must_use]
    pub fn is_live(&self, now: Instant, policy: MotionPolicy) -> bool {
        policy.animates() && !self.is_settled(now)
    }

    /// Head for `target` (clamped to `0..=1`). Mid-flight it starts from
    /// where the step is now, so nothing jumps. Asking for the target the
    /// step already has changes nothing and does not restart it, so a
    /// component may call this on every event. Returns whether anything
    /// changed.
    ///
    /// Where the policy does not let `timing.channel` move, the step jumps
    /// to `target` and is settled.
    pub fn go(
        &mut self,
        target: f32,
        now: Instant,
        timing: MotionTiming,
        policy: MotionPolicy,
    ) -> bool {
        if !target.is_finite() {
            return false;
        }
        let target = unit(target);
        if target == self.to {
            return false;
        }
        *self = if policy.allows(timing.channel) && !timing.duration.is_zero() {
            Self {
                from: self.at(now),
                to: target,
                start: Some(now),
                duration: timing.duration,
                easing: timing.easing,
            }
        } else {
            Self::at_rest(target)
        };
        true
    }

    /// Stop where the step is going, at once. Input wins: a key press
    /// settles every running step.
    pub fn settle(&mut self) {
        *self = Self::at_rest(self.to);
    }

    /// The redraw this step wants after `now`, or `None` once it is settled
    /// (or the policy does not animate).
    #[must_use]
    pub fn next_frame_at(&self, now: Instant, policy: MotionPolicy) -> Option<Instant> {
        self.is_live(now, policy)
            .then(|| now.checked_add(MOTION_FRAME_INTERVAL))
            .flatten()
    }

    /// The value to show: eased where `policy` lets `channel` move, the
    /// target where it does not. The one place the policy is applied at
    /// paint time, so a mode changed mid-flight cannot leave a ghost.
    fn shown(&self, now: Instant, policy: MotionPolicy, channel: MotionChannel) -> f32 {
        if policy.allows(channel) {
            self.at(now)
        } else {
            self.to
        }
    }

    /// A column between `from` and `to`: slide a marker, nudge a panel.
    #[must_use]
    pub fn offset(&self, now: Instant, policy: MotionPolicy, from: u16, to: u16) -> u16 {
        let v = self.shown(now, policy, MotionChannel::Space);
        let (a, b) = (f32::from(from), f32::from(to));
        (a + (b - a) * v).round().clamp(0.0, f32::from(u16::MAX)) as u16
    }

    /// How many of `width` columns are shown: `0` before, `width` after.
    #[must_use]
    pub fn reveal(&self, now: Instant, policy: MotionPolicy, width: u16) -> u16 {
        self.offset(now, policy, 0, width)
    }

    /// Ink: [`Theme::fg`] blended from `from` toward `to`. At the ends it is
    /// exactly `theme.fg(from)` and `theme.fg(to)`; between, a blend at
    /// truecolor, and a jump to the nearer role anywhere else.
    #[must_use]
    pub fn fg(
        &self,
        now: Instant,
        policy: MotionPolicy,
        theme: &Theme,
        from: Role,
        to: Role,
    ) -> Style {
        let v = self.shown(now, policy, MotionChannel::Tint);
        match ends(v, from, to) {
            Some(role) => theme.fg(role),
            None if !can_blend(theme) => theme.fg(nearer(v, from, to)),
            None => match (theme.fg(from).fg, theme.fg(to).fg) {
                (Some(a), Some(b)) => Style::default().fg(blend(b, a, v)),
                _ => theme.fg(nearer(v, from, to)),
            },
        }
    }

    /// A ground: [`Theme::bg`] blended from `from` toward `to`. A ground the
    /// terminal keeps (unpainted, or `NO_COLOR`) is not blended through: the
    /// step jumps to the nearer role.
    #[must_use]
    pub fn bg(
        &self,
        now: Instant,
        policy: MotionPolicy,
        theme: &Theme,
        from: Role,
        to: Role,
    ) -> Style {
        let v = self.shown(now, policy, MotionChannel::Tint);
        match ends(v, from, to) {
            Some(role) => theme.bg(role),
            None if !can_blend(theme) => theme.bg(nearer(v, from, to)),
            None => match (theme.bg(from).bg, theme.bg(to).bg) {
                (Some(a), Some(b)) => Style::default().bg(blend(b, a, v)),
                _ => theme.bg(nearer(v, from, to)),
            },
        }
    }
}

fn unit(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

fn ends(v: f32, from: Role, to: Role) -> Option<Role> {
    if v <= 0.0 {
        Some(from)
    } else if v >= 1.0 {
        Some(to)
    } else {
        None
    }
}

fn nearer(v: f32, from: Role, to: Role) -> Role {
    if v >= 0.5 { to } else { from }
}

/// A component's few named motions, and the schedule for all of them.
#[derive(Clone, Debug)]
pub struct MotionSet {
    steps: Vec<(&'static str, MotionStep)>,
    interval: Duration,
}

impl Default for MotionSet {
    fn default() -> Self {
        Self::new()
    }
}

impl MotionSet {
    #[must_use]
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            interval: MOTION_FRAME_INTERVAL,
        }
    }

    /// Cap redraws at one per `interval` (at least
    /// [`MOTION_MIN_FRAME_INTERVAL`]): 33 ms holds a slow terminal to 30
    /// frames a second.
    #[must_use]
    pub fn with_frame_interval(mut self, interval: Duration) -> Self {
        self.interval = interval.max(MOTION_MIN_FRAME_INTERVAL);
        self
    }

    /// Head the motion called `name` for `target`; see [`MotionStep::go`].
    /// A name seen for the first time starts at rest at `0.0`.
    pub fn go(
        &mut self,
        name: &'static str,
        target: f32,
        now: Instant,
        timing: MotionTiming,
        policy: MotionPolicy,
    ) -> bool {
        let at = match self.steps.iter().position(|(n, _)| *n == name) {
            Some(at) => at,
            None => {
                self.steps.push((name, MotionStep::default()));
                self.steps.len() - 1
            }
        };
        self.steps[at].1.go(target, now, timing, policy)
    }

    /// The motion called `name`, or one at rest at `0.0`.
    #[must_use]
    pub fn step(&self, name: &str) -> MotionStep {
        self.steps
            .iter()
            .find(|(n, _)| *n == name)
            .map_or_else(MotionStep::default, |(_, step)| *step)
    }

    /// Settle every motion at its target now. Input wins.
    pub fn settle_all(&mut self) {
        for (_, step) in &mut self.steps {
            step.settle();
        }
    }

    /// Whether every motion has reached its target at `now`.
    #[must_use]
    pub fn is_settled(&self, now: Instant) -> bool {
        self.steps.iter().all(|(_, step)| step.is_settled(now))
    }

    /// When the next redraw is due: one frame interval after `now` while any
    /// motion is live, and `None` once everything is settled or the policy
    /// does not animate. Ask right after a draw, with the instant it was
    /// drawn at, so the interval is the cap between draws.
    #[must_use]
    pub fn next_frame_at(&self, now: Instant, policy: MotionPolicy) -> Option<Instant> {
        self.steps
            .iter()
            .any(|(_, step)| step.is_live(now, policy))
            .then(|| now.checked_add(self.interval))
            .flatten()
    }
}

/// The earliest of several components' [`MotionSet::next_frame_at`]
/// answers; `None` when none of them wants a frame.
#[must_use]
pub fn soonest_frame(due: impl IntoIterator<Item = Option<Instant>>) -> Option<Instant> {
    due.into_iter().flatten().min()
}

/// What one frame may spend, and the earliest redraw anyone asked for.
///
/// Kept from the first draft of this package. The host makes one per frame,
/// each component claims what it draws, and the host reads
/// [`FrameBudget::next_frame_in`] after the draw. Only the first
/// [`claim_spinner`](Self::claim_spinner) and
/// [`claim_horizon`](Self::claim_horizon) return `true`: a second spinner is
/// drawn as the still `●` plus its word (hiding it would hide real work), and
/// a second horizon is not drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameBudget {
    next: Option<Duration>,
    spinners: usize,
    horizons: usize,
}

impl FrameBudget {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A redraw wanted `after` from now (`None` asks for nothing), as
    /// [`spin::next_frame_in`](crate::spin::next_frame_in) answers.
    pub fn request(&mut self, after: Option<Duration>) {
        if let Some(after) = after {
            self.next = Some(self.next.map_or(after, |current| current.min(after)));
        }
    }

    /// A redraw due at `at`, as [`MotionSet::next_frame_at`] answers. An
    /// instant already past asks for a frame now.
    pub fn request_at(&mut self, now: Instant, at: Option<Instant>) {
        self.request(at.map(|at| at.saturating_duration_since(now)));
    }

    /// The soonest redraw anyone asked for; `None` when nothing moves, so an
    /// idle screen sleeps until an event.
    #[must_use]
    pub fn next_frame_in(&self) -> Option<Duration> {
        self.next
    }

    /// Whether this is the frame's first spinner.
    pub fn claim_spinner(&mut self) -> bool {
        self.spinners += 1;
        self.spinners == 1
    }

    /// Whether this is the frame's first horizon rule.
    pub fn claim_horizon(&mut self) -> bool {
        self.horizons += 1;
        self.horizons == 1
    }
}

/// The words [`MotionDemo`] shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionDemoWords {
    pub working: Cow<'static, str>,
    pub done: Cow<'static, str>,
    pub slide: Cow<'static, str>,
    pub detail: Cow<'static, str>,
}

impl Default for MotionDemoWords {
    fn default() -> Self {
        Self {
            working: Cow::Borrowed("Working"),
            done: Cow::Borrowed("Done"),
            slide: Cow::Borrowed("Selected"),
            detail: Cow::Borrowed("patch applied to 3 files"),
        }
    }
}

/// The reference component for this module: one state change, painted at
/// one instant.
///
/// Three rows. The first is a state mark and word whose ink eases from
/// `Muted` to `Live` (a tint). The mark and the word always say the real
/// state, so a frame caught mid-step is never ambiguous. The second slides a
/// selection marker across, and the third reveals a line of detail. Where
/// the policy does not animate, every row is its end state.
#[derive(Clone, Debug)]
pub struct MotionDemo<'a> {
    motions: &'a MotionSet,
    now: Instant,
    policy: MotionPolicy,
    done: bool,
    words: MotionDemoWords,
}

impl<'a> MotionDemo<'a> {
    #[must_use]
    pub fn new(motions: &'a MotionSet, now: Instant, policy: MotionPolicy, done: bool) -> Self {
        Self {
            motions,
            now,
            policy,
            done,
            words: MotionDemoWords::default(),
        }
    }

    #[must_use]
    pub fn with_words(mut self, words: MotionDemoWords) -> Self {
        self.words = words;
        self
    }

    /// Start the three motions toward `done` (or back to working).
    pub fn start(motions: &mut MotionSet, done: bool, now: Instant, policy: MotionPolicy) {
        let target = if done { 1.0 } else { 0.0 };
        motions.go("ink", target, now, MotionTiming::ARRIVE, policy);
        motions.go(
            "slide",
            target,
            now,
            MotionTiming::space(MotionTiming::ARRIVE.duration, MotionEasing::EaseOut),
            policy,
        );
        motions.go("reveal", target, now, MotionTiming::PANEL, policy);
    }
}

impl Paint for MotionDemo<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let (now, policy) = (self.now, self.policy);
        let row = |i: u16| (area.height > i).then_some(area.y + i);
        let put = |buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, max: u16| {
            if x < area.right() {
                buf.set_stringn(x, y, s, usize::from(max.min(area.right() - x)), style);
            }
        };

        if let Some(y) = row(0) {
            let ink = self
                .motions
                .step("ink")
                .fg(now, policy, theme, Role::Muted, Role::Live);
            let (mark, word) = if self.done {
                (glyphs::DONE, &self.words.done)
            } else {
                (glyphs::CURRENT, &self.words.working)
            };
            let mark = glyphs::pick(mark, theme.ascii());
            put(buf, area.x, y, mark, ink, area.width);
            let x = area.x.saturating_add(text::width(mark) as u16 + 1);
            put(
                buf,
                x,
                y,
                &text::display_safe(word),
                theme.fg(Role::Foreground),
                area.width,
            );
        }

        if let Some(y) = row(1) {
            let marker = glyphs::pick(glyphs::SELECTION, theme.ascii());
            let label = text::display_safe(&self.words.slide);
            let used = text::width(marker) + 1 + text::width(&label);
            let span = area.width.saturating_sub(used as u16).min(24);
            let x = area
                .x
                .saturating_add(self.motions.step("slide").offset(now, policy, 0, span));
            put(buf, x, y, marker, theme.fg(Role::Accent), area.width);
            let lx = x.saturating_add(text::width(marker) as u16 + 1);
            put(buf, lx, y, &label, theme.fg(Role::Foreground), area.width);
        }

        if let Some(y) = row(2) {
            let detail = text::display_safe(&self.words.detail);
            let full = text::width(&detail).min(usize::from(area.width)) as u16;
            let shown = self.motions.step("reveal").reveal(now, policy, full);
            put(buf, area.x, y, &detail, theme.fg(Role::Muted), shown);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        3
    }
}
