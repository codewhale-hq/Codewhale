//! Quiet, action-triggered product guidance.
//!
//! These tips are deliberately event-driven rather than timer-driven. The
//! session gate keeps the TUI calm, while persisted impression counts prevent
//! a useful first-run hint from becoming permanent chrome.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::tui::app::{App, StatusToast, StatusToastKind, StatusToastLevel};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_runtime::settings::Settings;

const MAX_TIPS_PER_SESSION: u8 = 1;
const MAX_LIFETIME_IMPRESSIONS: u8 = 2;
const MAX_TRACKED_MANUAL_COMMANDS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BehavioralTip {
    BackgroundJobReceipt,
    ClearedInputRestore,
    McpValidation,
    RepeatedCommandHotbar,
    DurableStateWritten,
    #[expect(dead_code)]
    TodoWriteHint,
}

impl BehavioralTip {
    const fn key(self) -> &'static str {
        match self {
            Self::BackgroundJobReceipt => "background_job_receipt",
            Self::ClearedInputRestore => "cleared_input_restore",
            Self::McpValidation => "mcp_validation",
            Self::RepeatedCommandHotbar => "repeated_command_hotbar",
            Self::DurableStateWritten => "durable_state_written",
            Self::TodoWriteHint => "todo_write_hint",
        }
    }

    const fn message_id(self) -> MessageId {
        match self {
            Self::BackgroundJobReceipt => MessageId::BehavioralTipBackgroundReceipt,
            Self::ClearedInputRestore => MessageId::BehavioralTipClearedInput,
            Self::McpValidation => MessageId::BehavioralTipMcpValidation,
            Self::RepeatedCommandHotbar => MessageId::BehavioralTipRepeatedCommand,
            Self::DurableStateWritten => MessageId::BehavioralTipDurableStateWritten,
            Self::TodoWriteHint => MessageId::BehavioralTipTodoWrite,
        }
    }

    fn message(self, locale: Locale) -> String {
        let template = tr(locale, self.message_id());
        match self {
            Self::BackgroundJobReceipt => template.replace("{key}", "Enter"),
            Self::ClearedInputRestore => template.replace("{chord}", "Ctrl+Z"),
            Self::McpValidation => template.replace("{command}", "codewhale mcp validate"),
            Self::RepeatedCommandHotbar => template.replace("{command}", "/hotbar"),
            Self::DurableStateWritten => template.replace("{command}", "/memory"),
            Self::TodoWriteHint => template.replace("{command}", "todo_write"),
        }
    }
}

#[derive(Debug)]
pub struct BehavioralTipState {
    enabled: bool,
    shown_this_session: HashSet<BehavioralTip>,
    session_impressions: u8,
    manual_command_counts: HashMap<u64, u8>,
}

impl Default for BehavioralTipState {
    fn default() -> Self {
        Self::new(true)
    }
}

impl BehavioralTipState {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            shown_this_session: HashSet::new(),
            session_impressions: 0,
            manual_command_counts: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn guidance_available(&self) -> bool {
        self.enabled && self.session_impressions < MAX_TIPS_PER_SESSION
    }

    fn eligible_in_session(&self, tip: BehavioralTip) -> bool {
        self.guidance_available() && !self.shown_this_session.contains(&tip)
    }

    fn eligible(&self, tip: BehavioralTip, lifetime_impressions: u8) -> bool {
        self.eligible_in_session(tip) && lifetime_impressions < MAX_LIFETIME_IMPRESSIONS
    }

    fn record_impression(&mut self, tip: BehavioralTip) {
        self.shown_this_session.insert(tip);
        self.record_guidance_impression();
    }

    pub(crate) fn record_guidance_impression(&mut self) {
        self.session_impressions = self.session_impressions.saturating_add(1);
    }

    fn note_manual_command(&mut self, input: &str) -> bool {
        let Some(fingerprint) = manual_command_fingerprint(input) else {
            return false;
        };
        if self.manual_command_counts.len() >= MAX_TRACKED_MANUAL_COMMANDS
            && !self.manual_command_counts.contains_key(&fingerprint)
        {
            return false;
        }
        let count = self.manual_command_counts.entry(fingerprint).or_default();
        *count = count.saturating_add(1);
        *count == 3
    }
}

impl App {
    /// A preference change never acknowledges errors, approvals, or recovery
    /// notices. Keep impression caps intact when tips are enabled again.
    pub fn set_contextual_tips_enabled(&mut self, enabled: bool) {
        self.behavioral_tips.enabled = enabled;
        if !enabled {
            self.status_toasts.retain(|toast| {
                !matches!(
                    toast.kind,
                    StatusToastKind::BehavioralTip(_) | StatusToastKind::PluginSuggestion
                )
            });
            // One switch governs every plugin offer, the review row included.
            self.plugin_cta.phase = crate::tui::plugin_suggestions::PluginCtaPhase::Hidden;
        }
        self.needs_redraw = true;
    }

    /// Show a behavioral tip when both the quiet session cap and the persisted
    /// lifetime cap allow it. Persistence is best-effort: a read-only home
    /// must not make a useful in-session hint fail closed.
    pub fn maybe_show_behavioral_tip(&mut self, tip: BehavioralTip) -> bool {
        // Clear-input hooks are hot paths. Once the in-memory session gate is
        // closed, avoid touching the settings file for every later keypress.
        if !self.behavioral_tips.eligible_in_session(tip) {
            return false;
        }
        // Tests never touch the settings file here, so the eligibility read uses
        // in-memory defaults. Outside tests the read and the increment are one
        // transaction: a read-modify-write on an impression counter is exactly
        // what another whole-file writer would otherwise revert.
        if cfg!(test) {
            // No settings file is read or written, so the lifetime count is
            // whatever `Settings::default()` carries: nothing.
            if !self.behavioral_tips.eligible(tip, 0) {
                return false;
            }
            self.behavioral_tips.record_impression(tip);
        } else {
            let eligible = Settings::transact_opt(|settings| {
                let lifetime_impressions = settings
                    .behavioral_tip_impressions
                    .get(tip.key())
                    .copied()
                    .unwrap_or(0);
                if !self.behavioral_tips.eligible(tip, lifetime_impressions) {
                    return Ok(None);
                }
                settings.behavioral_tip_impressions.insert(
                    tip.key().to_string(),
                    lifetime_impressions.saturating_add(1),
                );
                Ok(Some(()))
            });
            match eligible {
                Ok(None) => return false,
                Ok(Some(())) => {}
                Err(err) => {
                    tracing::warn!(tip = tip.key(), error = %err, "behavioral tip impression was not persisted");
                }
            }
            self.behavioral_tips.record_impression(tip);
        }
        let mut toast = StatusToast::new(
            tip.message(self.ui_locale),
            StatusToastLevel::Info,
            Some(8_000),
        );
        toast.kind = StatusToastKind::BehavioralTip(tip);
        self.push_status_toast_record(toast);
        true
    }

    pub fn note_manual_command_for_tip(&mut self, input: &str) -> bool {
        self.behavioral_tips.enabled
            && self.behavioral_tips.note_manual_command(input)
            && self.maybe_show_behavioral_tip(BehavioralTip::RepeatedCommandHotbar)
    }
}

fn manual_command_fingerprint(input: &str) -> Option<u64> {
    let parts = input.split_whitespace().collect::<Vec<_>>();
    let command = parts.first()?;
    if !command.starts_with('/') || command.eq_ignore_ascii_case("/hotbar") {
        return None;
    }
    let normalized = parts.join(" ");
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_lifetime_caps_keep_tips_quiet() {
        let mut state = BehavioralTipState::default();
        assert!(state.eligible(BehavioralTip::McpValidation, 0));
        state.record_impression(BehavioralTip::McpValidation);
        assert!(!state.eligible(BehavioralTip::McpValidation, 0));
        assert!(!state.eligible(BehavioralTip::BackgroundJobReceipt, 0));

        let fresh_session = BehavioralTipState::default();
        assert!(fresh_session.eligible(BehavioralTip::McpValidation, 1));
        assert!(!fresh_session.eligible(BehavioralTip::McpValidation, MAX_LIFETIME_IMPRESSIONS));
    }

    #[test]
    fn third_matching_manual_command_triggers_once() {
        let mut state = BehavioralTipState::default();
        assert!(!state.note_manual_command("/model one"));
        assert!(!state.note_manual_command("/model two"));
        assert!(!state.note_manual_command(" /model   one "));
        assert!(state.note_manual_command("/model one"));
        assert!(!state.note_manual_command("/model one"));
        assert!(!state.note_manual_command("/hotbar"));
        assert!(!state.note_manual_command("ordinary prompt"));
    }

    #[test]
    fn every_complete_locale_renders_tips_with_code_owned_controls() {
        let tips = [
            BehavioralTip::BackgroundJobReceipt,
            BehavioralTip::ClearedInputRestore,
            BehavioralTip::McpValidation,
            BehavioralTip::RepeatedCommandHotbar,
            BehavioralTip::DurableStateWritten,
        ];
        for locale in Locale::shipped_complete() {
            for tip in tips {
                let message = tip.message(*locale);
                assert!(!message.contains('{'), "unexpanded placeholder: {message}");
            }
        }

        assert_eq!(
            BehavioralTip::BackgroundJobReceipt.message(Locale::En),
            "Receipts live in the Work panel — Enter opens the inspector"
        );
        assert_eq!(
            BehavioralTip::ClearedInputRestore.message(Locale::En),
            "Cleared · Ctrl+Z restores"
        );
        assert_eq!(
            BehavioralTip::McpValidation.message(Locale::En),
            "codewhale mcp validate starts servers and shows why"
        );
        assert_eq!(
            BehavioralTip::RepeatedCommandHotbar.message(Locale::En),
            "/hotbar can pin this"
        );
        assert_eq!(
            BehavioralTip::DurableStateWritten.message(Locale::En),
            "Saved · /memory to inspect"
        );
    }
}
