//! The full pet work surface. Replaces host-specific full-screen tank layout;
//! the host still owns sessions, roster receipts, responses and every action.

use crate::{
    DotWhale, MotionMode, Paint, PetStyle, Role, StatusMark, Subagent, SubagentIntent,
    SubagentView, SubagentViewState, SubagentViewWords, Theme, WhalePet, text,
    whale_motion::{Inputs, Presence, Stage, Tier},
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};
use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

/// Geometry shared by paint, response wrapping and host pointer routing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PetModeAreas {
    pub header: Rect,
    pub pet: Rect,
    pub agents: Rect,
    pub output: Rect,
    pub footer: Rect,
}

impl PetModeAreas {
    #[must_use]
    pub fn new(area: Rect, agents: bool, output: bool, focus_agents: bool) -> Self {
        let header = Rect {
            height: area.height.min(2),
            ..area
        };
        let footer_height = area.height.saturating_sub(header.height).min(2);
        let footer = Rect::new(
            area.x,
            area.bottom().saturating_sub(footer_height),
            area.width,
            footer_height,
        );
        let body = Rect::new(
            area.x,
            header.bottom(),
            area.width,
            area.height.saturating_sub(header.height + footer_height),
        );
        let mut plan = Self {
            header,
            footer,
            ..Self::default()
        };
        let wide = agents && body.width >= 100 && body.height >= 12;
        let main = if wide {
            let width = (body.width / 3).clamp(32, 44);
            plan.agents = Rect::new(body.right() - width, body.y, width, body.height);
            Rect {
                width: body.width.saturating_sub(width + 2),
                ..body
            }
        } else if agents && focus_agents {
            plan.agents = body;
            return plan;
        } else {
            body
        };
        if output && main.height >= 27 && main.width >= 16 {
            // Reading has priority over scenery, including at 80x24.
            let height = (u32::from(main.height) * 2 / 3).max(6) as u16;
            plan.output = Rect::new(main.x, main.bottom() - height, main.width, height);
            plan.pet = Rect {
                height: main.height - height,
                ..main
            };
        } else if output {
            plan.output = main;
        } else {
            plan.pet = main;
        }
        plan
    }
}

/// One canonical parent performance plus local selection/scroll. Never Clone.
#[derive(Default)]
pub struct PetModeState {
    stage: Stage,
    agents: SubagentViewState,
    pub output_scroll: usize,
    pub focus_agents: bool,
    output_detached: bool,
    output_end: usize,
    areas: PetModeAreas,
    session: Option<String>,
    visible: bool,
    motion: MotionMode,
    stage_animates: bool,
    style: PetStyle,
}

impl PetModeState {
    /// The owner supplies current facts and one clock before painting.
    pub fn update(
        &mut self,
        session: Option<&str>,
        inputs: Inputs,
        agents: &[Subagent<'_>],
        now: Instant,
        motion: MotionMode,
    ) {
        if self.session.as_deref() != session {
            self.agents = SubagentViewState::default();
            self.agents.set_visible(self.visible);
            self.reset_output();
            self.focus_agents = false;
            self.session = session.map(str::to_owned);
        } else if self.stage.director().context.turn_id != inputs.context.turn_id {
            self.reset_output();
        }
        let settled = inputs.presence == Presence::Done
            || (matches!(
                inputs.presence,
                Presence::Idle | Presence::Listening | Presence::Offline
            ) && matches!(
                inputs.context.status.as_deref(),
                Some("completed" | "failed" | "error" | "cancelled" | "canceled" | "interrupted")
            ));
        self.motion = motion;
        self.stage.set_visible(self.visible);
        self.agents.set_visible(self.visible);
        self.stage
            .observe(session, inputs.clone(), !motion.animates());
        self.stage.advance(now);
        self.stage_animates = !settled || self.stage.director().completing();
        if !self.stage_animates {
            self.stage.observe(session, inputs, true);
        }
        self.agents.update(agents, now, motion);
        if agents.is_empty() {
            self.focus_agents = false;
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        self.stage.set_visible(visible);
        self.agents.set_visible(visible);
    }

    #[must_use]
    pub fn next_frame_in(&self, agents: &[Subagent<'_>], theme: &Theme) -> Option<Duration> {
        if !self.visible || !self.motion.animates() {
            return None;
        }
        let art = WhalePet::art_area(self.areas.pet);
        let pet = (!theme.ascii() && art.width >= 16 && art.height >= 8 && self.stage_animates)
            .then(|| {
                self.stage
                    .cadence(if theme.paints_grounds() && self.style == PetStyle::Color {
                        Tier::Hero
                    } else {
                        Tier::Terminal
                    })
            })
            .flatten();
        [pet, self.agents.next_frame_in(agents, theme)]
            .into_iter()
            .flatten()
            .min()
    }

    /// Painted selection only; hosts validate the ID again before opening.
    #[must_use]
    pub fn selected_agent_id(&self) -> Option<&str> {
        (self.visible && self.focus_agents)
            .then(|| self.agents.selected_id())
            .flatten()
    }

    pub fn toggle_agents(&mut self, agents: &[Subagent<'_>]) {
        if !agents.is_empty() {
            self.focus_agents = !self.focus_agents;
        }
    }

    /// Follow the next response from the host's authoritative turn boundary.
    pub fn reset_output(&mut self) {
        self.output_scroll = 0;
        self.output_end = 0;
        self.output_detached = false;
    }

    fn scroll_output(&mut self, lines: i16) {
        self.output_scroll = self
            .output_scroll
            .saturating_add_signed(isize::from(lines))
            .min(self.output_end);
        self.output_detached = self.output_scroll < self.output_end;
    }

    /// Result scrolling or roster navigation, depending on the visible focus.
    pub fn scroll(&mut self, agents: &[Subagent<'_>], lines: i16) {
        if lines == 0 {
            return;
        }
        if self.focus_agents {
            let index = self
                .agents
                .selected_id()
                .and_then(|id| agents.iter().position(|agent| agent.id == id))
                .unwrap_or(0);
            let target = index
                .saturating_add_signed(isize::from(lines))
                .min(agents.len().saturating_sub(1));
            if let Some(agent) = agents.get(target) {
                self.agents.select(agents, &agent.id);
            }
        } else {
            self.scroll_output(lines);
        }
    }

    /// Home/End route to the focused roster or the complete response.
    pub fn scroll_end(&mut self, agents: &[Subagent<'_>], end: bool) {
        if self.focus_agents {
            self.agents.handle_key(
                agents,
                KeyEvent::from(if end { KeyCode::End } else { KeyCode::Home }),
            );
        } else {
            self.output_scroll = if end { usize::MAX } else { 0 };
            self.output_detached = !end;
        }
    }

    pub fn open_agent(&mut self, agents: &[Subagent<'_>]) -> Option<SubagentIntent> {
        if !self.focus_agents {
            return None;
        }
        self.agents
            .handle_key(agents, KeyEvent::from(KeyCode::Enter))
    }

    /// Tab switches the composed pane. Focused roster keys return the same
    /// capability-gated intents as SubagentView; the host owns every action.
    pub fn handle_key(&mut self, agents: &[Subagent<'_>], key: KeyEvent) -> Option<SubagentIntent> {
        if !self.visible
            || key.kind == KeyEventKind::Release
            || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            return None;
        }
        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            self.toggle_agents(agents);
            return None;
        }
        if self.focus_agents {
            return self.agents.handle_key(agents, key);
        }
        match key.code {
            KeyCode::Up => self.scroll(agents, -1),
            KeyCode::Down => self.scroll(agents, 1),
            KeyCode::PageUp => self.scroll(agents, -10),
            KeyCode::PageDown => self.scroll(agents, 10),
            KeyCode::Home => self.scroll_end(agents, false),
            KeyCode::End => self.scroll_end(agents, true),
            _ => {}
        }
        None
    }

    /// A click/wheel on the roster uses its recorded IDs; a wheel on the
    /// response scrolls it. No pointer input executes a worker action.
    pub fn handle_mouse(&mut self, agents: &[Subagent<'_>], mouse: MouseEvent) -> bool {
        if !self.visible {
            return false;
        }
        let point = (mouse.column, mouse.row).into();
        let cove_point = WhalePet::point(self.areas.pet, mouse.column, mouse.row);
        if cove_point.is_none() {
            self.stage.cove_leave();
        }
        if self.areas.agents.contains(point) && self.agents.handle_mouse(agents, mouse) {
            self.focus_agents = true;
            return true;
        }
        if self.areas.output.contains(point) {
            match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_output(-3),
                MouseEventKind::ScrollDown => self.scroll_output(3),
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {}
                _ => return false,
            }
            self.focus_agents = false;
            return true;
        }
        if let Some((x, y)) = cove_point {
            self.stage.cove_observe(x, y);
            if matches!(
                mouse.kind,
                MouseEventKind::Down(crossterm::event::MouseButton::Left)
            ) {
                self.stage.cove_tap(x, y);
            }
            return true;
        }
        false
    }
}

/// Full pet-mode composition. Response lines are already wrapped by the host's
/// existing Markdown/transcript renderer at [`PetModeAreas::output`] width.
/// Pass only actual roster facts and localized words. Paint never advances time.
pub struct PetMode<'a> {
    pub theme: &'a Theme,
    pub title: Cow<'a, str>,
    pub status: StatusMark,
    pub dot_whale: Option<DotWhale<'a>>,
    pub style: PetStyle,
    pub cove: bool,
    /// The host's existing notice, using its semantic severity ink.
    pub notice: Option<Line<'a>>,
    pub agents: &'a [Subagent<'a>],
    pub agent_words: SubagentViewWords<'a>,
    pub output: &'a [Line<'a>],
    pub output_title: Cow<'a, str>,
    /// Optional second footer row for the focused pane's controls.
    pub pane_hints: Cow<'a, str>,
    pub hints: Cow<'a, str>,
}

impl<'a> PetMode<'a> {
    #[must_use]
    pub fn new(theme: &'a Theme, status: StatusMark) -> Self {
        Self {
            theme,
            title: "Codewhale".into(),
            status,
            dot_whale: None,
            style: PetStyle::Color,
            cove: true,
            notice: None,
            agents: &[],
            agent_words: SubagentViewWords::default(),
            output: &[],
            output_title: "Response".into(),
            pane_hints: "".into(),
            hints: "Esc back".into(),
        }
    }
}

impl StatefulWidget for PetMode<'_> {
    type State = PetModeState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let area = area.intersection(buf.area);
        state.areas = PetModeAreas::default();
        state.style = self.style;
        if !state.visible || area.is_empty() {
            return;
        }
        buf.set_style(
            area,
            self.theme
                .bg(Role::Background)
                .patch(self.theme.fg(Role::Foreground)),
        );
        let plan = PetModeAreas::new(
            area,
            !self.agents.is_empty(),
            !self.output.is_empty(),
            state.focus_agents,
        );
        state.areas = plan;
        if !plan.header.is_empty() {
            let status = self.status.spans(self.theme);
            let status_width: usize = status.iter().map(|span| text::width(&span.content)).sum();
            let title = text::display_safe(&self.title);
            let mut spans = Vec::new();
            // Shed identity before phase; narrow/ASCII views still name the
            // activity even when a caller supplies a long title.
            if text::width(&title)
                .saturating_add(3)
                .saturating_add(status_width)
                <= usize::from(plan.header.width)
            {
                spans.push(Span::styled(
                    title.into_owned(),
                    self.theme.fg(Role::Accent),
                ));
                spans.push(Span::raw("   "));
            }
            spans.extend(status);
            Paragraph::new(Line::from(spans)).render(
                Rect {
                    height: 1,
                    ..plan.header
                },
                buf,
            );
        }
        if plan.header.height > 1
            && let Some(line) = self.notice
        {
            let safe = Line::from(
                line.spans
                    .iter()
                    .map(|span| {
                        Span::styled(text::display_safe(&span.content).into_owned(), span.style)
                    })
                    .collect::<Vec<_>>(),
            )
            .style(line.style);
            Paragraph::new(safe).render(
                Rect::new(plan.header.x, plan.header.y + 1, plan.header.width, 1),
                buf,
            );
        }
        if !plan.pet.is_empty() {
            if let Some(whale) = self.dot_whale {
                whale.paint(plan.pet, buf, self.theme);
            } else {
                WhalePet::new(self.theme)
                    .words("")
                    .style(self.style)
                    .cove(self.cove)
                    .paint(plan.pet, buf, &mut state.stage);
            }
        }
        let marker = if self.theme.ascii() { "> " } else { "› " };
        let mut agent_words = self.agent_words;
        if state.focus_agents {
            agent_words.title = format!("{marker}{}", agent_words.title).into();
        }
        if !plan.agents.is_empty() && plan.agents.x > area.x {
            // The shared layout already reserves this gap; no reading space
            // or pointer geometry is consumed by the divider.
            for y in plan.agents.y..plan.agents.bottom() {
                buf[(plan.agents.x - 1, y)]
                    .set_symbol(if self.theme.ascii() { "|" } else { "│" })
                    .set_style(self.theme.fg(Role::Muted));
            }
        }
        // Render even the hidden roster's empty area to clear stale hitboxes.
        SubagentView::new(self.agents, self.theme)
            .words(agent_words)
            .render(plan.agents, buf, &mut state.agents);
        if !plan.output.is_empty() {
            let body = Rect::new(
                plan.output.x,
                plan.output.y + 1,
                plan.output.width,
                plan.output.height.saturating_sub(1),
            );
            state.output_end = self.output.len().saturating_sub(usize::from(body.height));
            state.output_scroll = if state.output_detached {
                state.output_scroll.min(state.output_end)
            } else {
                state.output_end
            };
            let heading = Rect {
                height: 1,
                ..plan.output
            };
            let mut title_area = heading;
            if body.height > 0 && state.output_end > 0 {
                let range = format!(
                    "{}-{} / {}",
                    state.output_scroll + 1,
                    (state.output_scroll + usize::from(body.height)).min(self.output.len()),
                    self.output.len()
                );
                let width = text::width(&range) as u16;
                if heading.width >= width.saturating_add(8) {
                    title_area.width -= width + 1;
                    Paragraph::new(range)
                        .alignment(Alignment::Right)
                        .style(self.theme.fg(Role::Muted))
                        .render(heading, buf);
                }
            }
            let title = if state.focus_agents {
                self.output_title.into_owned()
            } else {
                format!("{marker}{}", self.output_title)
            };
            Paragraph::new(text::display_safe(&title).into_owned())
                .style(if state.focus_agents {
                    self.theme.fg(Role::Muted)
                } else {
                    self.theme.fg(Role::Accent).add_modifier(Modifier::BOLD)
                })
                .render(title_area, buf);
            // usize selection preserves arbitrarily long already-wrapped output.
            for (y, line) in self
                .output
                .iter()
                .skip(state.output_scroll)
                .take(usize::from(body.height))
                .enumerate()
            {
                let safe = Line::from(
                    line.spans
                        .iter()
                        .map(|span| {
                            Span::styled(text::display_safe(&span.content).into_owned(), span.style)
                        })
                        .collect::<Vec<_>>(),
                )
                .style(line.style);
                Paragraph::new(safe)
                    .render(Rect::new(body.x, body.y + y as u16, body.width, 1), buf);
            }
        }
        if !plan.footer.is_empty() {
            let split = plan.footer.height > 1 && !self.pane_hints.is_empty();
            let hints = text::display_safe(&self.hints);
            let hints = if split {
                text::truncate(&hints, usize::from(plan.footer.width), self.theme.ascii())
                    .into_owned()
            } else {
                hints.into_owned()
            };
            Paragraph::new(hints)
                .wrap(Wrap { trim: true })
                .style(self.theme.fg(Role::Muted))
                .render(
                    Rect {
                        height: if split { 1 } else { plan.footer.height },
                        ..plan.footer
                    },
                    buf,
                );
            if split {
                Paragraph::new(
                    text::truncate(
                        &text::display_safe(&self.pane_hints),
                        usize::from(plan.footer.width),
                        self.theme.ascii(),
                    )
                    .into_owned(),
                )
                .style(self.theme.fg(Role::Muted))
                .render(
                    Rect::new(plan.footer.x, plan.footer.y + 1, plan.footer.width, 1),
                    buf,
                );
            }
        }
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;
    #[test]
    fn default_hidden_updates_do_not_advance_the_parent_clock() {
        let mut state = PetModeState::default();
        let now = Instant::now();
        let inputs = Inputs {
            presence: Presence::Working,
            activity: None,
            context: Default::default(),
        };
        state.update(Some("session"), inputs.clone(), &[], now, MotionMode::Full);
        state.update(
            Some("session"),
            inputs.clone(),
            &[],
            now + Duration::from_secs(30),
            MotionMode::Full,
        );
        assert!(!state.stage.is_visible());
        assert_eq!(state.stage.director().f, 0.);
        state.set_visible(true);
        state.update(
            Some("session"),
            inputs,
            &[],
            now + Duration::from_secs(30),
            MotionMode::Full,
        );
        assert_eq!(state.stage.director().f, 0.);
    }
}
