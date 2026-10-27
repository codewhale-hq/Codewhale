//! Status surface state: toast queue, sticky status, and the
//! `status_message` -> toast synchronization helpers.
//!
//! `StatusToast` / `StatusToastLevel` live here with the `impl App`
//! extension block that owns toast/status/message behavior; state fields
//! (`status_toasts`, `sticky_status`, `status_message`,
//! `last_status_message_seen`) remain on `App` in `app.rs`.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl StatusToastLevel {
    /// Resolve every toast surface through the same semantic theme slots.
    pub(crate) fn ink(self) -> codewhale_palette::ChromeInk {
        use codewhale_palette::ChromeInk;
        match self {
            Self::Info => ChromeInk::Info,
            Self::Success => ChromeInk::Outcome,
            Self::Warning => ChromeInk::Attention,
            Self::Error => ChromeInk::Failure,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusToast {
    pub text: String,
    pub level: StatusToastLevel,
    pub created_at: Instant,
    pub ttl_ms: Option<u64>,
    pub(crate) kind: StatusToastKind,
    event_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusToastKind {
    Ordinary,
    /// An explicit command result stays visible over a completed turn.
    CommandReceipt,
    ActionRequired,
    RedactionGate(RedactionGateNotice),
    BehavioralTip(crate::tui::behavioral_tips::BehavioralTip),
    PluginSuggestion,
    ContextPressure(codewhale_runtime::context_budget::PressureLevel),
    /// A condition that holds until its owner retires it by event id (a
    /// session save that keeps failing). It never times out: expiring it
    /// would report recovery that has not happened.
    Standing,
}

impl StatusToast {
    #[must_use]
    pub fn new(text: impl Into<String>, level: StatusToastLevel, ttl_ms: Option<u64>) -> Self {
        Self {
            text: text.into(),
            level,
            created_at: Instant::now(),
            ttl_ms,
            kind: StatusToastKind::Ordinary,
            event_id: None,
        }
    }

    pub(crate) fn for_event(mut self, event_id: impl Into<String>) -> Self {
        self.event_id = Some(event_id.into());
        self
    }

    /// A notice for a standing condition, withdrawn only through
    /// [`App::retire_event_notices`]`(event_id)` once the condition clears.
    #[must_use]
    pub(crate) fn standing(
        text: impl Into<String>,
        level: StatusToastLevel,
        event_id: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into(),
            level,
            created_at: Instant::now(),
            ttl_ms: None,
            kind: StatusToastKind::Standing,
            event_id: Some(event_id.into()),
        }
    }

    pub(crate) fn for_action(mut self, request_id: impl Into<String>) -> Self {
        self.kind = StatusToastKind::ActionRequired;
        self.event_id = Some(request_id.into());
        self
    }

    pub(crate) fn for_command_receipt(mut self) -> Self {
        self.kind = StatusToastKind::CommandReceipt;
        self
    }

    pub(crate) fn for_redaction_gate(mut self, notice: RedactionGateNotice) -> Self {
        self.kind = StatusToastKind::RedactionGate(notice);
        self
    }

    #[must_use]
    pub(crate) fn context_pressure(
        text: impl Into<String>,
        level: codewhale_runtime::context_budget::PressureLevel,
    ) -> Self {
        Self {
            text: text.into(),
            level: StatusToastLevel::Warning,
            created_at: Instant::now(),
            ttl_ms: None,
            kind: StatusToastKind::ContextPressure(level),
            event_id: None,
        }
    }

    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        self.ttl_ms.is_some_and(|ttl| {
            now.saturating_duration_since(self.created_at).as_millis() >= u128::from(ttl)
        })
    }
}

impl App {
    pub fn push_status_toast(
        &mut self,
        text: impl Into<String>,
        level: StatusToastLevel,
        ttl_ms: Option<u64>,
    ) {
        self.push_status_toast_record(StatusToast::new(text, level, ttl_ms));
    }

    /// Coalesce a still-visible duplicate without renewing its first expiry.
    /// Decision identities keep otherwise identical requests independent.
    pub(crate) fn push_status_toast_record(&mut self, mut toast: StatusToast) {
        // An omitted lifetime used to leave routine notices in the queue
        // forever, resurfacing after newer notices expired. Pending actions
        // and safety gates have explicit kinds and keep their own lifecycle.
        if toast.kind == StatusToastKind::Ordinary && toast.ttl_ms.is_none() {
            toast.ttl_ms = Some(Self::STICKY_ERROR_TTL_MS);
        }
        self.prune_expired_status_toasts(toast.created_at);
        if self.status_toasts.iter().any(|existing| {
            existing.level == toast.level
                && existing.text == toast.text
                && existing.kind == toast.kind
                && existing.event_id == toast.event_id
        }) {
            return;
        }
        self.status_toasts.push_back(toast);
        // Overflow drops the oldest notice that is not a standing condition;
        // those leave only when their condition clears.
        while self.status_toasts.len() > 24 {
            let oldest = self
                .status_toasts
                .iter()
                .position(|toast| toast.kind != StatusToastKind::Standing)
                .unwrap_or(0);
            self.status_toasts.remove(oldest);
        }
        self.needs_redraw = true;
    }

    /// Retire requests, never their denial/error outcomes. `None` settles
    /// requests for the whole finished/cancelled turn.
    pub(crate) fn retire_action_notices(&mut self, request_id: Option<&str>) {
        let before = self.status_toasts.len();
        self.status_toasts.retain(|toast| {
            toast.kind != StatusToastKind::ActionRequired
                || request_id.is_some_and(|id| toast.event_id.as_deref() != Some(id))
        });
        self.needs_redraw |= self.status_toasts.len() != before;
    }

    /// Retire the notices pushed with [`StatusToast::for_event`]`(event_id)`.
    pub(crate) fn retire_event_notices(&mut self, event_id: &str) {
        let before = self.status_toasts.len();
        self.status_toasts
            .retain(|toast| toast.event_id.as_deref() != Some(event_id));
        self.needs_redraw |= self.status_toasts.len() != before;
    }

    /// Gate transitions retire their own guidance or failed-write receipt,
    /// independently of translated text and unrelated requests/errors.
    pub(crate) fn retire_redaction_gate_notice(&mut self, notice: RedactionGateNotice) {
        let before = self.status_toasts.len();
        self.status_toasts
            .retain(|toast| toast.kind != StatusToastKind::RedactionGate(notice));
        self.needs_redraw |= self.status_toasts.len() != before;
    }

    /// Default lifetime for sticky error toasts. Long enough to read, short
    /// enough that a failed workflow does not permanently occupy footer chrome.
    pub const STICKY_ERROR_TTL_MS: u64 = 8_000;

    pub fn set_sticky_status(
        &mut self,
        text: impl Into<String>,
        level: StatusToastLevel,
        ttl_ms: Option<u64>,
    ) {
        let text = text.into();
        if self.sticky_status.as_ref().is_some_and(|existing| {
            existing.text == text
                && existing.level == level
                && existing.kind == StatusToastKind::Ordinary
                && !existing.is_expired(Instant::now())
        }) {
            return;
        }
        // Cap sticky errors so a missing TTL never becomes permanent chrome.
        // Explicit shorter TTLs still win; longer/None fall back to the default.
        let ttl_ms = match level {
            StatusToastLevel::Error => Some(
                ttl_ms
                    .unwrap_or(Self::STICKY_ERROR_TTL_MS)
                    .min(Self::STICKY_ERROR_TTL_MS),
            ),
            _ => ttl_ms.or(Some(Self::STICKY_ERROR_TTL_MS)),
        };
        self.sticky_status = Some(StatusToast::new(text, level, ttl_ms));
        self.needs_redraw = true;
    }

    pub fn clear_sticky_status(&mut self) {
        if self.sticky_status.take().is_some() {
            self.needs_redraw = true;
        }
    }

    /// Dismiss the persistent context-pressure warning without dismissing
    /// unrelated error/status chrome. Returns whether anything was cleared.
    pub fn dismiss_context_pressure_warning(&mut self) -> bool {
        let is_context_pressure = self
            .sticky_status
            .as_ref()
            .is_some_and(|status| matches!(status.kind, StatusToastKind::ContextPressure(_)));
        if is_context_pressure {
            if self.status_message.as_ref() == self.sticky_status.as_ref().map(|toast| &toast.text)
            {
                self.status_message = None;
                self.last_status_message_seen = None;
            }
            if let Some(StatusToastKind::ContextPressure(level)) =
                self.sticky_status.as_ref().map(|status| status.kind)
            {
                self.context_pressure_warning_dismissed = Some(level);
            }
            self.clear_sticky_status();
            return true;
        }
        false
    }

    /// Drop sticky error chrome when the user resumes typing so a prior
    /// workflow/provider failure does not linger over the next draft.
    pub fn acknowledge_sticky_on_composer_activity(&mut self) {
        if self
            .sticky_status
            .as_ref()
            .is_some_and(|toast| matches!(toast.level, StatusToastLevel::Error))
        {
            self.clear_sticky_status();
        }
    }

    pub(super) fn classify_status_text(text: &str) -> (StatusToastLevel, Option<u64>, bool) {
        let lower = text.to_ascii_lowercase();
        let has = |needle: &str| lower.contains(needle);

        if has("offline mode") || has("context critical") {
            return (StatusToastLevel::Warning, None, true);
        }
        if has("error")
            || has("failed")
            || has("denied")
            || has("timeout")
            || has("aborted")
            || has("critical")
        {
            return (
                StatusToastLevel::Error,
                Some(Self::STICKY_ERROR_TTL_MS),
                true,
            );
        }
        // A success keyword under a negation ("not saved", "no longer
        // found", "could not enable") is a failure the coarse keyword match
        // would otherwise paint green. Guard it: negated success degrades to
        // a neutral Info toast rather than a misleading Success.
        let negated = has("not ")
            || has("no longer")
            || has("no ")
            || has("could not")
            || has("couldn't")
            || has("cannot")
            || has("can't")
            || has("unable");
        if !negated
            && (has("saved")
                || has("loaded")
                || has("queued")
                || has("found")
                || has("enabled")
                || has("completed"))
        {
            return (StatusToastLevel::Success, Some(5_000), false);
        }
        if has("cancelled") || has("canceled") || has("warning") {
            return (StatusToastLevel::Warning, Some(5_000), false);
        }
        (StatusToastLevel::Info, Some(4_000), false)
    }

    fn is_mode_switch_status_message(message: &str) -> bool {
        message.starts_with("Switched to ") && message.ends_with(" mode")
    }

    pub fn sync_status_message_to_toasts(&mut self) {
        let current = self.status_message.clone();
        if self.last_status_message_seen == current {
            return;
        }
        self.last_status_message_seen = current.clone();

        let Some(message) = current else {
            return;
        };
        if message.trim().is_empty() {
            return;
        }
        if Self::is_mode_switch_status_message(&message) {
            return;
        }
        let now = Instant::now();
        // A typed producer already owns this notice. The legacy adapter must
        // not reclassify tool text or create a second sticky/queued copy.
        if self
            .status_toasts
            .iter()
            .chain(self.sticky_status.iter())
            .any(|toast| toast.text == message && !toast.is_expired(now))
        {
            return;
        }

        let (level, ttl_ms, sticky) = Self::classify_status_text(&message);
        if sticky {
            self.set_sticky_status(message, level, ttl_ms);
        } else {
            // A routine success ("Auto-compaction enabled") must not clear
            // the one error that says no model is connected: nothing but
            // connecting a provider resolves it (U1).
            if matches!(level, StatusToastLevel::Success)
                && !self.onboarding_needs_api_key
                && self
                    .sticky_status
                    .as_ref()
                    .is_some_and(|toast| matches!(toast.level, StatusToastLevel::Error))
            {
                self.clear_sticky_status();
            }
            self.push_status_toast(message, level, ttl_ms);
        }
    }

    fn prune_expired_status_toasts(&mut self, now: Instant) {
        let queued_before = self.status_toasts.len();
        self.status_toasts.retain(|toast| !toast.is_expired(now));
        let queued_removed = self.status_toasts.len() != queued_before;
        let sticky_removed = self
            .sticky_status
            .as_ref()
            .is_some_and(|toast| toast.is_expired(now));
        if sticky_removed {
            self.sticky_status = None;
        }
        if queued_removed || sticky_removed {
            self.needs_redraw = true;
        }
    }

    pub fn active_status_toast(
        &mut self,
        phase: crate::tui::underwater::ShellPhase,
    ) -> Option<StatusToast> {
        self.sync_status_message_to_toasts();
        let now = Instant::now();
        self.prune_expired_status_toasts(now);

        let eligible = |toast: &&StatusToast| {
            phase != crate::tui::underwater::ShellPhase::Done
                || toast.kind == StatusToastKind::CommandReceipt
                || matches!(
                    toast.level,
                    StatusToastLevel::Warning | StatusToastLevel::Error
                )
        };
        let sticky = self.sticky_status.as_ref().filter(eligible).cloned();
        let latest = self.status_toasts.iter().rev().find(eligible).cloned();
        match (sticky, latest) {
            (Some(sticky), Some(latest))
                if matches!(
                    sticky.kind,
                    StatusToastKind::ContextPressure(
                        codewhale_runtime::context_budget::PressureLevel::High
                    ) | StatusToastKind::ContextPressure(
                        codewhale_runtime::context_budget::PressureLevel::Medium
                    )
                ) =>
            {
                Some(latest)
            }
            (Some(sticky), _) => Some(sticky),
            (None, latest) => latest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn app() -> App {
        App::new(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
            &codewhale_runtime::config::Config::default(),
        )
    }

    #[test]
    fn live_toast_duplicates_keep_first_expiry_and_distinct_decisions() {
        let mut app = app();
        let now = Instant::now();
        let record = |id: &str, at: Instant| {
            let mut toast = StatusToast::new(
                "Review this request",
                StatusToastLevel::Warning,
                Some(2_000),
            )
            .for_action(id);
            toast.created_at = at;
            toast
        };
        app.push_status_toast_record(record("a", now));
        app.push_status_toast_record(record("a", now + Duration::from_millis(1_000)));
        assert_eq!(app.status_toasts.len(), 1);
        assert_eq!(app.status_toasts[0].created_at, now);
        app.push_status_toast_record(record("b", now + Duration::from_millis(1_000)));
        assert_eq!(app.status_toasts.len(), 2);
        app.push_status_toast_record(record("a", now + Duration::from_millis(2_000)));
        assert_eq!(
            app.status_toasts.len(),
            2,
            "expired a is replaced; independent b survives"
        );
        assert_eq!(
            app.status_toasts.back().unwrap().created_at,
            now + Duration::from_millis(2_000)
        );
    }

    #[test]
    fn routine_notices_expire_without_dismissing_pending_actions_or_safety_gates() {
        let mut app = app();
        let now = Instant::now();
        app.push_status_toast("Temporary warning", StatusToastLevel::Warning, None);
        app.push_status_toast("Copied", StatusToastLevel::Info, None);
        app.set_sticky_status("Offline mode", StatusToastLevel::Warning, None);
        app.push_status_toast_record(
            StatusToast::new("Review request", StatusToastLevel::Warning, None)
                .for_action("pending"),
        );
        app.push_status_toast_record(
            StatusToast::new("Review redaction", StatusToastLevel::Warning, None)
                .for_redaction_gate(RedactionGateNotice::WriteFailure),
        );
        app.prune_expired_status_toasts(now + Duration::from_secs(10));
        assert!(app.sticky_status.is_none());
        assert_eq!(app.status_toasts.len(), 2);
        assert_eq!(app.status_toasts[0].kind, StatusToastKind::ActionRequired);
        assert_eq!(
            app.status_toasts[1].kind,
            StatusToastKind::RedactionGate(RedactionGateNotice::WriteFailure)
        );
    }

    #[test]
    fn legacy_status_does_not_reclassify_a_typed_notice_or_repeat_a_live_notice() {
        let mut app = app();
        let text = "承認してください · failed-tool";
        app.push_status_toast(text, StatusToastLevel::Warning, Some(12_000));
        let first = app.status_toasts[0].created_at;
        for message in [text, "another status", text] {
            app.status_message = Some(message.into());
            app.sync_status_message_to_toasts();
        }
        assert_eq!(app.status_toasts.len(), 2);
        assert_eq!(app.status_toasts[0].level, StatusToastLevel::Warning);
        assert_eq!(app.status_toasts[0].created_at, first);
        assert!(
            app.sticky_status.is_none(),
            "tool data must not create an inferred error"
        );
    }

    #[test]
    fn repeated_sticky_error_does_not_renew_its_expiry() {
        let mut app = app();
        app.set_sticky_status("same failure", StatusToastLevel::Error, None);
        let created_at = app.sticky_status.as_ref().unwrap().created_at;
        app.set_sticky_status("same failure", StatusToastLevel::Error, None);
        assert_eq!(app.sticky_status.as_ref().unwrap().created_at, created_at);
        app.sticky_status.as_mut().unwrap().created_at = created_at - Duration::from_secs(10);
        app.set_sticky_status("same failure", StatusToastLevel::Error, None);
        assert!(app.sticky_status.as_ref().unwrap().created_at >= created_at);
    }

    #[test]
    fn retiring_action_notices_preserves_other_requests_and_outcome_receipts() {
        let mut app = app();
        for id in ["a", "b"] {
            app.push_status_toast_record(
                StatusToast::new("Review", StatusToastLevel::Warning, None).for_action(id),
            );
        }
        app.push_status_toast_record(
            StatusToast::new("Denied", StatusToastLevel::Warning, None).for_event("a"),
        );
        app.retire_action_notices(Some("a"));
        assert_eq!(app.status_toasts.len(), 2);
        assert_eq!(app.status_toasts[0].event_id.as_deref(), Some("b"));
        app.retire_action_notices(None);
        assert_eq!(app.status_toasts.len(), 1);
        assert_eq!(app.status_toasts[0].text, "Denied");
    }

    #[test]
    fn redaction_gate_cleanup_preserves_failed_writes_until_settled_and_unrelated_notices() {
        let mut app = app();
        // Deliberately equal translated text: identity must own cleanup.
        for notice in [
            RedactionGateNotice::EnterGuidance,
            RedactionGateNotice::WriteFailure,
        ] {
            app.push_status_toast_record(
                StatusToast::new("確認してください", StatusToastLevel::Warning, None)
                    .for_redaction_gate(notice),
            );
        }
        app.push_status_toast("確認してください", StatusToastLevel::Warning, None);
        app.push_status_toast_record(
            StatusToast::new("Review", StatusToastLevel::Warning, None).for_action("request"),
        );
        app.set_sticky_status("Unrelated failure", StatusToastLevel::Error, None);
        app.needs_redraw = false;
        app.retire_redaction_gate_notice(RedactionGateNotice::EnterGuidance);
        assert!(app.needs_redraw);
        assert_eq!(app.status_toasts.len(), 3);
        assert_eq!(
            app.status_toasts[0].kind,
            StatusToastKind::RedactionGate(RedactionGateNotice::WriteFailure)
        );
        app.retire_redaction_gate_notice(RedactionGateNotice::WriteFailure);
        assert_eq!(app.status_toasts.len(), 2);
        assert_eq!(app.status_toasts[0].kind, StatusToastKind::Ordinary);
        assert_eq!(app.status_toasts[1].kind, StatusToastKind::ActionRequired);
        assert_eq!(
            app.sticky_status.as_ref().unwrap().text,
            "Unrelated failure"
        );
    }
}
