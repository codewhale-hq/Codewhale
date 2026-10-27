//! Approval option selection and modal state.
//!
//! This module owns approval-card interaction and event emission. Risk policy,
//! persistent rules, preview formatting, and sandbox elevation remain separate
//! authority boundaries.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use codewhale_config::ToolAskRule;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::tui::views::{ModalKind, ModalView, ViewAction, ViewEvent};
use crate::tui::widgets::{ApprovalWidget, Renderable};
use codewhale_execpolicy::canonical_action::canonical_action_alias;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_runtime::config::ApprovalDefaultSelection;

use super::previews::exact_edit_file_preview_lines;
use super::{ApprovalRequest, ReviewDecision};
#[cfg(test)]
use crate::core::authority::RiskLevel;

/// Indices into the option list shared by both variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOption {
    ApproveOnce,
    ApproveAlways,
    AllowExactRepo,
    Deny,
    Abort,
}

impl ApprovalOption {
    const ORDER: [ApprovalOption; 4] = [
        ApprovalOption::ApproveOnce,
        ApprovalOption::ApproveAlways,
        ApprovalOption::Deny,
        ApprovalOption::Abort,
    ];
    const ORDER_WITH_PERSISTENT_ALLOW: [ApprovalOption; 5] = [
        ApprovalOption::ApproveOnce,
        ApprovalOption::ApproveAlways,
        ApprovalOption::AllowExactRepo,
        ApprovalOption::Deny,
        ApprovalOption::Abort,
    ];

    /// A child agent's card (approvals C1): the person can hide it, never
    /// stop the parent's turn from it, so "Stop this turn" is not offered.
    const CHILD_ORDER: [ApprovalOption; 3] = [
        ApprovalOption::ApproveOnce,
        ApprovalOption::ApproveAlways,
        ApprovalOption::Deny,
    ];

    /// Workflow elevated-plan card (#4126): Approve / Edit plan / Cancel.
    const WORKFLOW_ORDER: [ApprovalOption; 3] = [
        ApprovalOption::ApproveOnce,
        ApprovalOption::Deny,
        ApprovalOption::Abort,
    ];

    fn order_for(request: &ApprovalRequest) -> &'static [ApprovalOption] {
        if request.tool_name == "workflow" {
            &Self::WORKFLOW_ORDER
        } else if request.owner.is_some() {
            &Self::CHILD_ORDER
        } else if request.can_save_allow_rule() {
            &Self::ORDER_WITH_PERSISTENT_ALLOW
        } else {
            &Self::ORDER
        }
    }

    fn from_index_for(request: &ApprovalRequest, idx: usize) -> ApprovalOption {
        Self::order_for(request)
            .get(idx)
            .copied()
            .unwrap_or(Self::Abort)
    }

    fn index_for(self, request: &ApprovalRequest) -> usize {
        Self::order_for(request)
            .iter()
            .position(|o| *o == self)
            .unwrap_or(Self::order_for(request).len().saturating_sub(1))
    }

    fn decision(self) -> ReviewDecision {
        match self {
            ApprovalOption::ApproveOnce => ReviewDecision::Approved,
            ApprovalOption::ApproveAlways => ReviewDecision::ApprovedForSession,
            ApprovalOption::AllowExactRepo => ReviewDecision::Approved,
            // Workflow maps Deny → "Edit plan" (model revises plan).
            ApprovalOption::Deny => ReviewDecision::Denied,
            ApprovalOption::Abort => ReviewDecision::Abort,
        }
    }
}

/// Approval overlay state managed by the modal view stack
#[derive(Debug, Clone)]
pub struct ApprovalView {
    request: ApprovalRequest,
    pub(super) selected: usize,
    pub(super) row_hitboxes: RefCell<Vec<Rect>>,
    /// Whether the last paint showed the persistent-rule save preview. The
    /// save offers (`[p]` and `s`) only work while it is on screen, so a
    /// person never saves a rule without seeing what it covers. Starts
    /// false: nothing is offered before the card has been painted.
    save_preview_shown: Cell<bool>,
    locale: Locale,
    pub(super) timeout: Option<Duration>,
    pub(super) requested_at: Instant,
    /// Whether the approval card is collapsed to a single-line banner.
    pub(crate) collapsed: bool,
}

impl ApprovalView {
    #[cfg(test)]
    pub fn new(request: ApprovalRequest) -> Self {
        Self::new_for_locale(request, Locale::En)
    }

    #[cfg(test)]
    pub fn new_for_locale(request: ApprovalRequest, locale: Locale) -> Self {
        Self::new_with_default_selection(request, locale, ApprovalDefaultSelection::default())
    }

    /// `default_selection` is `[approval] default_selection` (#5293). Deny
    /// stays the default so a fresh card never turns a reflexive Enter into
    /// authorization; `allow_once` is a user opting out of that guard.
    pub fn new_with_default_selection(
        request: ApprovalRequest,
        locale: Locale,
        default_selection: ApprovalDefaultSelection,
    ) -> Self {
        // Resolve the semantic option because its numeric index differs for
        // persistent-allow and workflow approval cards.
        let selected = match default_selection {
            ApprovalDefaultSelection::Deny => ApprovalOption::Deny,
            ApprovalDefaultSelection::AllowOnce => ApprovalOption::ApproveOnce,
        }
        .index_for(&request);
        Self {
            request,
            selected,
            row_hitboxes: RefCell::new(Vec::new()),
            save_preview_shown: Cell::new(false),
            locale,
            timeout: None,
            requested_at: Instant::now(),
            collapsed: false,
        }
    }

    /// Bound how long this card may wait (#6101). `Some(timeout)` resolves
    /// the card to **deny** once the duration elapses (fail-closed); `None`
    /// waits indefinitely. A zero duration is treated as `None` so the
    /// config convention (`0` = wait forever) holds at this layer too.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout.filter(|timeout| !timeout.is_zero());
        self
    }

    pub(super) fn select_prev(&mut self) {
        self.step_selection(-1);
    }

    pub(super) fn select_next(&mut self) {
        self.step_selection(1);
    }

    /// Move the selection, skipping options the card is not offering.
    fn step_selection(&mut self, delta: isize) {
        let order = ApprovalOption::order_for(&self.request);
        let mut selected = self.selected;
        for _ in 0..order.len() {
            selected = crate::tui::list_nav::wrap_index(selected, order.len(), delta);
            if self.offers(order[selected]) {
                break;
            }
        }
        self.selected = selected;
    }

    /// Whether the card currently offers `option`. Saving a persistent allow
    /// rule is offered only while its save preview is on screen.
    pub(crate) fn offers(&self, option: ApprovalOption) -> bool {
        option != ApprovalOption::AllowExactRepo || self.save_preview_shown.get()
    }

    /// Record whether the paint that just ran showed the save preview.
    pub(crate) fn set_save_preview_shown(&self, shown: bool) {
        self.save_preview_shown.set(shown);
    }

    pub(super) fn current_option(&self) -> ApprovalOption {
        ApprovalOption::from_index_for(&self.request, self.selected)
    }

    /// The agent that owns this card, when it is a child's request.
    #[cfg(test)]
    #[must_use]
    pub fn owner(&self) -> Option<&super::ApprovalOwner> {
        self.request.owner.as_ref()
    }

    /// Whether this approval is the elevated Workflow plan card (#4126).
    #[must_use]
    pub fn is_workflow_plan_approval(&self) -> bool {
        self.request.tool_name == "workflow"
    }

    /// Test-only accessor for the selected option's decision.
    #[cfg(test)]
    pub(super) fn current_decision(&self) -> ReviewDecision {
        self.current_option().decision()
    }

    /// Selected option for the renderer (used by the widget tests too).
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn set_mouse_hitboxes(&self, hitboxes: Vec<Rect>) {
        *self.row_hitboxes.borrow_mut() = hitboxes;
    }

    /// Risk level for the renderer's accent picking.
    #[cfg(test)]
    pub fn risk(&self) -> RiskLevel {
        self.request.risk
    }

    pub(crate) fn locale(&self) -> Locale {
        self.locale
    }

    /// Commit the given option and close the approval modal.
    fn commit_option(&mut self, option: ApprovalOption) -> ViewAction {
        if !self.offers(option) {
            // Fail closed: a rule whose coverage is not on screen is never
            // saved, and nothing else is decided in its place.
            return ViewAction::None;
        }
        self.selected = option.index_for(&self.request);
        if option == ApprovalOption::AllowExactRepo && self.request.can_save_allow_rule() {
            self.emit_decision_with_rules(
                option.decision(),
                false,
                self.request.persistent_allow_rules.clone(),
            )
        } else {
            self.emit_decision(option.decision(), false)
        }
    }

    fn emit_decision(&self, decision: ReviewDecision, timed_out: bool) -> ViewAction {
        self.emit_decision_with_rules(decision, timed_out, Vec::new())
    }

    fn emit_decision_with_rules(
        &self,
        decision: ReviewDecision,
        timed_out: bool,
        persistent_rules: Vec<ToolAskRule>,
    ) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            tool_id: self.request.id.clone(),
            tool_name: self.request.tool_name.clone(),
            decision,
            timed_out,
            approval_key: self.request.approval_key.clone(),
            approval_grouping_key: self.request.approval_grouping_key.clone(),
            persistent_rules,
        })
    }

    fn emit_params_pager(&self) -> ViewAction {
        // The compact prompt keeps the about/impact dossier out of the
        // default band; the pager is where that context now lives.
        let locale = self.locale();
        let about_label = tr(locale, MessageId::ApprovalLabelAbout);
        let impact_label = tr(locale, MessageId::ApprovalLabelImpact);
        let mut content = String::new();
        content.push_str(&about_label);
        content.push_str(&self.request.description_for_locale(locale));
        content.push('\n');
        for impact in self.request.impacts_for_locale(locale) {
            content.push_str(&impact_label);
            content.push_str(&impact);
            content.push('\n');
        }
        content.push('\n');
        if canonical_action_alias(&self.request.tool_name, &self.request.params) == "edit_file"
            && let Some(preview_lines) = exact_edit_file_preview_lines(&self.request.params, locale)
        {
            content.push_str(&tr(locale, MessageId::ApprovalLabelPreview));
            content.push_str(":\n");
            for line in preview_lines {
                content.push_str(&line);
                content.push('\n');
            }
            content.push('\n');
        }
        content.push_str(
            &serde_json::to_string_pretty(&self.request.params)
                .unwrap_or_else(|_| self.request.params.to_string()),
        );
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: format!("Tool Params: {}", self.request.tool_name),
            content,
        })
    }

    fn is_timed_out(&self) -> bool {
        match self.timeout {
            Some(timeout) => self.requested_at.elapsed() >= timeout,
            None => false,
        }
    }
}

impl ModalView for ApprovalView {
    fn kind(&self) -> ModalKind {
        ModalKind::Approval
    }

    fn approval_request_id(&self) -> Option<&str> {
        Some(&self.request.id)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if key.kind != KeyEventKind::Press {
            return ViewAction::None;
        }
        // Details is the intentional modified-key action; it cannot grant
        // authority. Editing/mode chords must never answer this card.
        if crate::tui::shell_key_routing::is_tool_details_shortcut(&key) {
            return self.emit_params_pager();
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Tab => {
                self.collapsed = !self.collapsed;
                ViewAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                ViewAction::None
            }
            KeyCode::Enter => self.commit_option(self.current_option()),
            // Direct shortcuts; '1' / '2' map to the first two options
            // so a numeric pad still works for approve flows.
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('1') => {
                self.commit_option(ApprovalOption::ApproveOnce)
            }
            KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('2')
                if !self.is_workflow_plan_approval() =>
            {
                self.commit_option(ApprovalOption::ApproveAlways)
            }
            KeyCode::Char('p') | KeyCode::Char('P')
                if self.request.can_save_allow_rule() && self.save_preview_shown.get() =>
            {
                self.commit_option(ApprovalOption::AllowExactRepo)
            }
            // Workflow plan card (#4126): [2/e] Edit plan, [3/n/d] Cancel.
            KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Char('2')
                if self.is_workflow_plan_approval() =>
            {
                self.commit_option(ApprovalOption::Deny)
            }
            KeyCode::Char('s') | KeyCode::Char('S')
                if self.request.can_save_ask_rule() && self.save_preview_shown.get() =>
            {
                self.emit_decision_with_rules(
                    ReviewDecision::Approved,
                    false,
                    self.request.persistent_ask_rules.clone(),
                )
            }
            KeyCode::Char('n')
            | KeyCode::Char('N')
            | KeyCode::Char('d')
            | KeyCode::Char('D')
            | KeyCode::Char('3') => {
                if self.is_workflow_plan_approval() {
                    // Cancel (abort turn) rather than session-deny.
                    self.commit_option(ApprovalOption::Abort)
                } else {
                    self.commit_option(ApprovalOption::Deny)
                }
            }
            // A child's card hides on Esc: the request stays pending (footer
            // row, `/agents`) and the parent's turn is never cancelled.
            KeyCode::Esc if self.request.owner.is_some() => ViewAction::Close,
            KeyCode::Esc => self.emit_decision(ReviewDecision::Abort, false),
            KeyCode::Char('g') | KeyCode::Char('G') => match self.request.owner.as_ref() {
                Some(owner) => ViewAction::Emit(ViewEvent::OpenAgentTranscript {
                    agent_id: owner.agent_id.clone(),
                }),
                None => ViewAction::None,
            },
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.select_prev();
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self.row_hitboxes.borrow().iter().position(|rect| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                });
                if let Some(index) = clicked {
                    return self
                        .commit_option(ApprovalOption::from_index_for(&self.request, index));
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let approval_widget = ApprovalWidget::new(&self.request, self);
        approval_widget.render(area, buf);
    }

    fn occupied_region(&self, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
        // The approval is an inline, bottom-anchored prompt: it only occupies
        // a band at the bottom of the frame so the backdrop dims that band and
        // the transcript above stays visible. Must match what `render` paints.
        ApprovalWidget::new(&self.request, self).inline_region(area)
    }

    fn tick(&mut self) -> ViewAction {
        if self.is_timed_out() {
            return self.emit_decision(ReviewDecision::Denied, true);
        }
        ViewAction::None
    }
}
