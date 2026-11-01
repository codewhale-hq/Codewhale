//! In-context plugin reminders: the send-time toast, the model-requested
//! review row, and idle catalog polling.
//!
//! 0.10.1 plugin offering policy ("helpful, not pushy"):
//! - The only unprompted surface is the send-time toast. There is no live
//!   as-you-type matching.
//! - The review row appears only when the model calls `request_plugin_install`
//!   (once per session), and only while contextual tips are on and the shared
//!   per-session guidance budget has room.
//! - The row offers Review. Only that button opens the existing Extensions
//!   inventory: installed bundles on Plugins, other suggestions on Marketplace.
//!   Install, exact-content trust, and enable remain explicit row actions.
//! - Esc hides the row for this session only. "Don't suggest again" is the
//!   explicit, persisted dismissal.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::{App, StatusToast, StatusToastKind, StatusToastLevel};
use codewhale_localization::{MessageId, tr};
use codewhale_plugins::recommend::{
    PluginNextStep, load_marketplace_candidates, match_plugin_for_draft,
};

const CATALOG_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCtaPhase {
    Hidden,
    Matched { name: String },
}

impl PluginCtaPhase {
    #[must_use]
    pub fn is_visible(&self) -> bool {
        matches!(self, Self::Matched { .. })
    }

    #[must_use]
    pub fn matched_name(&self) -> Option<&str> {
        match self {
            Self::Hidden => None,
            Self::Matched { name, .. } => Some(name.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginCtaState {
    pub phase: PluginCtaPhase,
    /// Lowercased names hidden from every proactive path: persisted "Don't
    /// suggest again" choices plus this session's Esc dismissals.
    pub dismissed: BTreeSet<String>,
}

impl Default for PluginCtaState {
    fn default() -> Self {
        Self {
            phase: PluginCtaPhase::Hidden,
            dismissed: BTreeSet::new(),
        }
    }
}

impl PluginCtaState {
    pub(crate) fn from_settings(settings: &codewhale_runtime::settings::Settings) -> Self {
        Self {
            dismissed: settings
                .dismissed_plugin_suggestions
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
            ..Self::default()
        }
    }
}

impl App {
    /// When the user sends a task that matches an installed-but-idle plugin
    /// or a locally added marketplace candidate, toast the next review step
    /// once. Never installs, trusts, or enables anything.
    pub fn maybe_nudge_plugin_for_prompt(&mut self, input: &str) -> bool {
        if !self.behavioral_tips.guidance_available() {
            return false;
        }
        let marketplace = load_marketplace_candidates(self.plugin_registry.state_path());
        let Some(recommendation) = match_plugin_for_draft(
            input,
            self.plugin_registry.as_ref(),
            &marketplace,
            &self.plugin_cta.dismissed,
        ) else {
            return false;
        };
        let message_id = match recommendation.next_step {
            PluginNextStep::Trust => MessageId::PluginPromptSuggestTrust,
            PluginNextStep::Enable => MessageId::PluginPromptSuggestEnable,
            PluginNextStep::MarketplaceInstall { .. } => MessageId::PluginPromptSuggestMarketplace,
            PluginNextStep::AlreadyActive
            | PluginNextStep::Inspect
            | PluginNextStep::SourceInstall { .. } => return false,
        };
        let mut message = tr(self.ui_locale, message_id).replace("{name}", &recommendation.name);
        if let PluginNextStep::MarketplaceInstall { catalog_id } = &recommendation.next_step {
            message = message.replace("{catalog}", catalog_id);
        }
        if let Some(term) = recommendation.matched_term {
            message.push_str(" · ");
            message.push_str(
                &tr(self.ui_locale, MessageId::PluginSuggestionReason).replace("{trigger}", &term),
            );
        }
        self.behavioral_tips.record_guidance_impression();
        let mut toast = StatusToast::new(message, StatusToastLevel::Info, Some(8_000));
        toast.kind = StatusToastKind::PluginSuggestion;
        self.push_status_toast_record(toast);
        true
    }

    /// Cheap idle poll so on-disk plugin changes can surface between turns,
    /// not only on send. Fingerprints directories; never auto-reloads.
    pub fn maybe_poll_plugin_catalog_idle(&mut self) {
        let now = Instant::now();
        if self
            .last_plugin_catalog_poll
            .is_some_and(|seen| now.duration_since(seen) < CATALOG_POLL_INTERVAL)
        {
            return;
        }
        self.last_plugin_catalog_poll = Some(now);
        if let Some(message) = codewhale_plugins::plugin_reload_nudge(
            self.plugin_registry.as_ref(),
            &mut self.plugin_reload_nudge_stamp,
        ) {
            self.push_status_toast(message, StatusToastLevel::Warning, Some(8_000));
            self.needs_redraw = true;
        }
    }

    #[must_use]
    pub fn plugin_cta_row_height(&self) -> u16 {
        u16::from(self.plugin_cta.phase.is_visible())
    }

    /// Esc: hide the row and skip this plugin for the rest of the session.
    /// Persists nothing, so the next session may offer it again.
    pub fn dismiss_plugin_cta_for_session(&mut self) -> bool {
        let Some(name) = self
            .plugin_cta
            .phase
            .matched_name()
            .map(str::to_ascii_lowercase)
        else {
            return false;
        };
        self.plugin_cta.dismissed.insert(name);
        self.plugin_cta.phase = PluginCtaPhase::Hidden;
        self.needs_redraw = true;
        true
    }

    /// "Don't suggest again": the explicit, persisted dismissal. Also hides
    /// the row immediately for this session, even if saving fails.
    pub fn dismiss_plugin_cta(&mut self) -> bool {
        let Some(name) = self.plugin_cta.phase.matched_name().map(str::to_string) else {
            return false;
        };
        let name = name.to_ascii_lowercase();
        self.plugin_cta.dismissed.insert(name.clone());
        self.plugin_cta.phase = PluginCtaPhase::Hidden;
        self.needs_redraw = true;
        if let Err(error) = codewhale_runtime::settings::Settings::transact_opt(|settings| {
            Ok(settings
                .dismissed_plugin_suggestions
                .insert(name)
                .then_some(()))
        }) {
            tracing::warn!(%error, "could not persist plugin suggestion dismissal");
            self.push_status_toast(
                tr(self.ui_locale, MessageId::PluginCtaDismissSaveFailed).into_owned(),
                StatusToastLevel::Warning,
                Some(8_000),
            );
        }
        true
    }

    /// Human-initiated navigation to the existing inventory. Resolve the tab
    /// from current installed facts, never from the model-returned command.
    /// The inventory retains its own explicit install/review/enable actions.
    #[must_use]
    pub fn accept_plugin_cta_review(
        &mut self,
    ) -> Option<crate::tui::views::extensions::ExtensionsTab> {
        let name = match &self.plugin_cta.phase {
            PluginCtaPhase::Matched { name } => name.clone(),
            PluginCtaPhase::Hidden => return None,
        };
        let tab = if self.plugin_registry.get(&name).is_some() {
            crate::tui::views::extensions::ExtensionsTab::Plugins
        } else {
            crate::tui::views::extensions::ExtensionsTab::Marketplace
        };
        self.plugin_cta.dismissed.insert(name.to_ascii_lowercase());
        self.plugin_cta.phase = PluginCtaPhase::Hidden;
        self.needs_redraw = true;
        Some(tab)
    }

    /// Model-requested review: show the review row and a toast naming the
    /// command. Does not run it, so nothing is installed, trusted, or
    /// enabled. Obeys the tips switch and draws from the shared per-session
    /// guidance budget like every other proactive offer.
    pub fn surface_plugin_review_request(&mut self, name: &str, command: &str) {
        if name.trim().is_empty()
            || command.trim().is_empty()
            || !self.behavioral_tips.guidance_available()
            || self
                .plugin_cta
                .dismissed
                .contains(&name.to_ascii_lowercase())
        {
            return;
        }
        self.behavioral_tips.record_guidance_impression();
        self.plugin_cta.phase = PluginCtaPhase::Matched {
            name: name.to_string(),
        };
        let mut toast = StatusToast::new(command.to_string(), StatusToastLevel::Info, Some(8_000));
        toast.kind = StatusToastKind::PluginSuggestion;
        self.push_status_toast_record(toast);
        self.needs_redraw = true;
    }
}

/// Draw the one-line review row above the composer. No-op when hidden.
pub fn draw_plugin_cta(app: &mut App, area: Rect, buf: &mut Buffer) {
    app.viewport.last_plugin_cta_area = None;
    app.viewport.last_plugin_cta_review_area = None;
    app.viewport.last_plugin_cta_dismiss_area = None;
    let PluginCtaPhase::Matched { name } = &app.plugin_cta.phase else {
        return;
    };
    let name = name.clone();
    if area.height == 0 || area.width == 0 {
        return;
    }
    let prompt = tr(app.ui_locale, MessageId::PluginCtaInstallPrompt).replace("{name}", &name);
    let review = tr(app.ui_locale, MessageId::PluginCtaReview);
    let dismiss = tr(app.ui_locale, MessageId::PluginCtaDismiss);
    let review_label = format!("[{review}]");
    let dismiss_label = format!("[{dismiss}]");
    let review_w = review_label.width() as u16;
    let dismiss_w = dismiss_label.width() as u16;
    let gap = 1u16;
    let right_w = review_w.saturating_add(gap).saturating_add(dismiss_w);
    let bg = Style::default().bg(app.ui_theme.composer_bg);
    Block::default().style(bg).render(area, buf);
    let left_budget = if area.width > right_w.saturating_add(1) {
        area.width - right_w - 1
    } else {
        area.width
    };
    let left = Line::from(vec![Span::styled(
        prompt,
        Style::default().fg(app.ui_theme.text_hint),
    )]);
    buf.set_line(area.x, area.y, &left, left_budget);
    if area.width <= right_w {
        app.viewport.last_plugin_cta_area = Some(area);
        return;
    }
    let review_x = area.x + area.width - right_w;
    let dismiss_x = review_x + review_w + gap;
    buf.set_stringn(
        review_x,
        area.y,
        &review_label,
        usize::from(review_w),
        Style::default().fg(app.ui_theme.accent_action),
    );
    buf.set_stringn(
        dismiss_x,
        area.y,
        &dismiss_label,
        usize::from(dismiss_w),
        Style::default().fg(app.ui_theme.text_hint),
    );
    app.viewport.last_plugin_cta_area = Some(area);
    app.viewport.last_plugin_cta_review_area = Some(Rect::new(review_x, area.y, review_w, 1));
    app.viewport.last_plugin_cta_dismiss_area = Some(Rect::new(dismiss_x, area.y, dismiss_w, 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_localization::Locale;
    use codewhale_runtime::config::Config;
    use std::fs;
    use tempfile::TempDir;

    fn app_with_supabase_plugin() -> (App, TempDir, crate::test_support::EnvVarGuard) {
        let root = TempDir::new().unwrap();
        let home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let bundle = root.path().join(".codewhale/plugins/supabase");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"supabase\"\nversion = \"1.0.0\"\ndescription = \"Hosted Postgres and auth\"\nkeywords = [\"supabase\"]\n",
        )
        .unwrap();
        let temp = TempDir::new().unwrap();
        let options = TuiOptions {
            config_path: Some(temp.path().join("config.toml")),
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory.md"),
            notes_path: temp.path().join("notes.txt"),
            mcp_config_path: temp.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(root.path())
        };
        let discovery = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv();
        let registry = discovery.registry_for_workspace(root.path());
        let mut app = App::new_with_plugin_registry(options, &Config::default(), registry);
        app.ui_locale = Locale::En;
        (app, root, home)
    }

    #[test]
    fn sending_a_supabase_prompt_toasts_trust_for_an_installed_idle_plugin() {
        let _lock = crate::test_support::lock_test_env();
        let (mut app, _root, _home) = app_with_supabase_plugin();

        assert!(app.maybe_nudge_plugin_for_prompt("add supabase auth to login"));
        assert_eq!(app.status_toasts.len(), 1);
        assert!(
            app.status_toasts[0].text.contains("/plugin trust supabase"),
            "{}",
            app.status_toasts[0].text
        );
        assert!(!app.maybe_nudge_plugin_for_prompt("add supabase auth to login"));
    }

    #[test]
    fn optional_plugin_and_behavioral_guidance_share_one_session_budget() {
        use crate::tui::behavioral_tips::BehavioralTip;
        let _lock = crate::test_support::lock_test_env();
        for plugin_first in [true, false] {
            let (mut app, _root, _home) = app_with_supabase_plugin();
            if plugin_first {
                assert!(app.maybe_nudge_plugin_for_prompt("add supabase auth"));
                assert!(!app.maybe_show_behavioral_tip(BehavioralTip::McpValidation));
            } else {
                assert!(app.maybe_show_behavioral_tip(BehavioralTip::McpValidation));
                assert!(!app.maybe_nudge_plugin_for_prompt("add supabase auth"));
            }
            assert_eq!(app.status_toasts.len(), 1);
        }
    }

    #[test]
    fn tips_off_removes_every_plugin_offer_but_preserves_required_notices() {
        let _lock = crate::test_support::lock_test_env();
        let (mut app, _root, _home) = app_with_supabase_plugin();
        app.set_contextual_tips_enabled(false);
        assert!(!app.maybe_nudge_plugin_for_prompt("add supabase auth"));
        app.surface_plugin_review_request("supabase", "/plugin trust supabase");
        assert!(
            !app.plugin_cta.phase.is_visible(),
            "tips off: no review row, even when the model asks"
        );
        assert_eq!(app.plugin_cta_row_height(), 0);
        assert!(app.status_toasts.is_empty());

        app.set_contextual_tips_enabled(true);
        app.surface_plugin_review_request("supabase", "/plugin trust supabase");
        assert!(app.plugin_cta.phase.is_visible());
        app.push_status_toast_record(
            StatusToast::new("Review required", StatusToastLevel::Warning, None).for_action("a"),
        );
        app.push_status_toast("Keep this error", StatusToastLevel::Error, None);
        app.set_contextual_tips_enabled(false);
        assert!(
            !app.plugin_cta.phase.is_visible(),
            "turning tips off hides the row"
        );
        assert_eq!(app.status_toasts.len(), 2);
        assert!(
            app.status_toasts
                .iter()
                .all(|toast| toast.kind != StatusToastKind::PluginSuggestion)
        );
        app.set_contextual_tips_enabled(true);
        assert!(
            !app.maybe_nudge_plugin_for_prompt("add supabase auth"),
            "reenabling must not reset the shared cap"
        );
    }

    #[test]
    fn model_requested_review_draws_from_the_shared_budget() {
        let _lock = crate::test_support::lock_test_env();
        let (mut app, _root, _home) = app_with_supabase_plugin();
        assert!(app.maybe_nudge_plugin_for_prompt("add supabase auth"));
        app.surface_plugin_review_request("supabase", "/plugin trust supabase");
        assert!(
            !app.plugin_cta.phase.is_visible(),
            "the send-time toast already spent this session's budget"
        );
    }

    #[test]
    fn typing_a_matching_draft_never_shows_a_row() {
        let _lock = crate::test_support::lock_test_env();
        let (mut app, _root, _home) = app_with_supabase_plugin();
        app.input = "add supabase auth to login".to_string();
        app.maybe_poll_plugin_catalog_idle();
        assert!(!app.plugin_cta.phase.is_visible());
        assert_eq!(app.plugin_cta_row_height(), 0);
    }

    #[test]
    fn review_row_opens_installed_inventory_without_running_suggested_commands() {
        let _lock = crate::test_support::lock_test_env();
        for command in [
            "/plugin trust supabase",
            "/plugin enable supabase",
            "/plugin marketplace install official supabase",
        ] {
            let (mut app, _root, _home) = app_with_supabase_plugin();
            let before = app.plugin_registry.get("supabase").unwrap().clone();
            app.surface_plugin_review_request("supabase", command);
            assert_eq!(
                app.plugin_cta.phase,
                PluginCtaPhase::Matched {
                    name: "supabase".into(),
                }
            );
            let area = Rect::new(0, 0, 140, 1);
            let mut buffer = Buffer::empty(area);
            draw_plugin_cta(&mut app, area, &mut buffer);
            let row = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(row.contains("supabase"), "{row}");
            assert!(row.contains("[Review]"), "{row}");
            assert!(row.contains("[Don't suggest again]"), "{row}");
            assert_eq!(
                app.accept_plugin_cta_review(),
                Some(crate::tui::views::extensions::ExtensionsTab::Plugins),
                "accepting opens the installed inventory, never {command}"
            );
            let after = app.plugin_registry.get("supabase").unwrap();
            assert_eq!(after.trust_status, before.trust_status);
            assert_eq!(after.enabled, before.enabled);
            assert_eq!(after.active(), before.active());
            assert!(!app.plugin_cta.phase.is_visible());
        }
    }

    #[test]
    fn uninstalled_review_opens_marketplace_without_installing_or_running_commands() {
        let _lock = crate::test_support::lock_test_env();
        let (mut app, _root, _home) = app_with_supabase_plugin();
        let before = app.plugin_registry.list().len();
        app.surface_plugin_review_request("catalog-only", "/plugin enable supabase");
        assert!(app.plugin_registry.get("catalog-only").is_none());
        assert_eq!(
            app.accept_plugin_cta_review(),
            Some(crate::tui::views::extensions::ExtensionsTab::Marketplace)
        );
        assert!(app.plugin_registry.get("catalog-only").is_none());
        assert_eq!(app.plugin_registry.list().len(), before);
        assert!(!app.plugin_registry.get("supabase").unwrap().enabled);
        assert_eq!(app.accept_plugin_cta_review(), None);
    }

    #[test]
    fn esc_clears_a_draft_first_then_dismisses_for_the_session_only() {
        use crate::tui::composer_ui::{EscapeAction, next_escape_action};
        use codewhale_runtime::settings::Settings;
        let _lock = crate::test_support::lock_test_env();
        let (mut app, root, _home) = app_with_supabase_plugin();
        app.surface_plugin_review_request("supabase", "/plugin trust supabase");
        app.input = "half-written draft".into();
        assert_eq!(next_escape_action(&app, false), EscapeAction::ClearInput);
        app.input.clear();
        assert_eq!(
            next_escape_action(&app, false),
            EscapeAction::DismissPluginCta
        );

        assert!(app.dismiss_plugin_cta_for_session());
        assert!(!app.plugin_cta.phase.is_visible());
        assert!(!app.maybe_nudge_plugin_for_prompt("add supabase auth"));
        let saved = Settings::load_read_only().unwrap_or_default();
        assert!(
            saved.dismissed_plugin_suggestions.is_empty(),
            "Esc persists nothing"
        );
        let restarted = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &Config::default(),
            app.plugin_registry.clone(),
        );
        assert!(!restarted.plugin_cta.dismissed.contains("supabase"));
    }

    #[test]
    fn dismissal_survives_restart_and_all_proactive_paths_preserving_settings() {
        use codewhale_runtime::settings::Settings;
        let _lock = crate::test_support::lock_test_env();
        let (mut app, root, _home) = app_with_supabase_plugin();
        Settings::transact(|settings| settings.set("max_history", "321")).unwrap();
        app.surface_plugin_review_request("supabase", "/plugin trust supabase");
        assert!(app.dismiss_plugin_cta());
        let saved = Settings::load_read_only().unwrap();
        assert_eq!(saved.max_input_history, 321);
        assert!(saved.dismissed_plugin_suggestions.contains("supabase"));
        // A freshly initialized App must hydrate the persisted preference.
        let mut restarted = App::new_with_plugin_registry(
            crate::tui::test_support::test_tui_options(root.path()),
            &Config::default(),
            app.plugin_registry.clone(),
        );
        assert!(!restarted.maybe_nudge_plugin_for_prompt("add supabase auth to login"));
        restarted.surface_plugin_review_request("supabase", "/plugin trust supabase");
        assert!(!restarted.plugin_cta.phase.is_visible());
        assert!(
            codewhale_plugins::recommend::lookup_reviewable_plugin(
                "supabase",
                restarted.plugin_registry.as_ref(),
                &[],
            )
            .is_some(),
            "manual plugin commands remain available"
        );
    }

    #[test]
    fn failed_dismissal_save_preserves_malformed_preferences_and_hides_this_session() {
        let _lock = crate::test_support::lock_test_env();
        let (mut app, root, _home) = app_with_supabase_plugin();
        app.surface_plugin_review_request("supabase", "/plugin trust supabase");
        let path = codewhale_runtime::settings::Settings::path().unwrap();
        assert!(path.starts_with(root.path()));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let malformed = "theme = [private_fixture_payload\n";
        fs::write(&path, malformed).unwrap();
        assert!(app.dismiss_plugin_cta());
        assert_eq!(fs::read_to_string(&path).unwrap(), malformed);
        assert!(!app.plugin_cta.phase.is_visible());
        let toast = app.status_toasts.back().expect("save failure receipt");
        assert!(toast.text.contains("could not save"));
        assert!(!toast.text.contains("private_fixture_payload"));
    }
}
