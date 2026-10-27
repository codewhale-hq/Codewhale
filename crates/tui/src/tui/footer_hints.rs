//! Footer key hints stand down once their binding has been used.
//!
//! The posture bar teaches each chord only until the user has pressed it
//! [`USES_TO_RETIRE`] times; after that the chip goes bare (or the hint
//! goes away) and the row stays quiet. Counts persist in [`Settings`]
//! beside `behavioral_tip_impressions`, so a learned binding stays learned
//! across sessions.
//!
//! [`Settings`]: codewhale_runtime::settings::Settings

use std::collections::BTreeMap;

use crate::tui::app::App;
use codewhale_runtime::settings::Settings;

/// Uses of a binding after which its footer hint retires.
pub(crate) const USES_TO_RETIRE: u8 = 2;

/// Stable hint keys. The footer shows a hint until its key reaches
/// [`USES_TO_RETIRE`] uses, then renders the bare state.
pub(crate) const PERMISSION_CYCLE: &str = "permission_cycle";
pub(crate) const MODE_CYCLE: &str = "mode_cycle";
pub(crate) const ESC_INTERRUPT: &str = "esc_interrupt";
pub(crate) const ENTER_AGAIN: &str = "enter_again";
pub(crate) const AGENT_ARROWS: &str = "agent_arrows";
/// The idle bottom-of-screen affordance that opens the work dock. Founder
/// live-test: "what do we press at the bottom to get the workbar to show up?"
pub(crate) const DOCK_OPEN: &str = "dock_open";
/// The `/help` route hint on the metrics line. It retires like every other
/// hint so the row stops advertising a route the user already knows
/// (founder, 2026-09-08: an always-on key hint is noise).
pub(crate) const HELP_ROUTE: &str = "help_route";

/// Whether the hint for `key` has been used often enough to retire.
pub(crate) fn retired(uses: &BTreeMap<String, u8>, key: &str) -> bool {
    uses.get(key).copied().unwrap_or(0) >= USES_TO_RETIRE
}

impl App {
    /// Record one use of the binding behind footer hint `key`.
    ///
    /// Persistence is best-effort. Once the in-memory count (seeded from
    /// settings at startup) retires the hint, a use touches no file at all;
    /// before that each of at most [`USES_TO_RETIRE`] uses per key runs one
    /// settings transaction on this thread. A read-only home still retires
    /// the hint for the running session.
    pub(crate) fn note_footer_hint_used(&mut self, key: &str) {
        if cfg!(test) {
            // Tests never touch the settings file here, so only the
            // in-memory count moves. Outside tests the read and the bump are
            // one transaction, exactly like the behavioral-tip impressions.
            let count = self.footer_hint_uses.entry(key.to_string()).or_default();
            *count = count.saturating_add(1);
            return;
        }
        if retired(&self.footer_hint_uses, key) {
            return;
        }
        let owned = key.to_string();
        let persisted = Settings::transact_opt(|settings| {
            let count = settings.footer_hint_uses.get(&owned).copied().unwrap_or(0);
            if count >= USES_TO_RETIRE {
                return Ok(None);
            }
            let next = count.saturating_add(1);
            settings.footer_hint_uses.insert(owned.clone(), next);
            Ok(Some(next))
        });
        match persisted {
            Ok(next) => {
                // `None` means already retired on disk: pin the in-memory
                // count at the retire line so the next frame stands down.
                let count = self.footer_hint_uses.entry(owned).or_default();
                *count = (*count).max(next.unwrap_or(USES_TO_RETIRE));
            }
            Err(err) => {
                tracing::warn!(hint = key, error = %err, "footer hint use was not persisted");
                let count = self.footer_hint_uses.entry(owned).or_default();
                *count = count.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_retire_after_two_uses() {
        let uses = BTreeMap::new();
        assert!(!retired(&uses, PERMISSION_CYCLE));
        for key in [
            PERMISSION_CYCLE,
            MODE_CYCLE,
            ESC_INTERRUPT,
            ENTER_AGAIN,
            AGENT_ARROWS,
        ] {
            let mut uses = BTreeMap::new();
            uses.insert(key.to_string(), 1);
            assert!(!retired(&uses, key), "{key} at 1 use still shows");
            uses.insert(key.to_string(), USES_TO_RETIRE);
            assert!(retired(&uses, key), "{key} at 2 uses is gone");
            uses.insert(key.to_string(), u8::MAX);
            assert!(retired(&uses, key), "{key} stays retired");
        }
    }

    #[test]
    fn recorded_uses_accumulate_per_key() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
        );
        app.note_footer_hint_used(PERMISSION_CYCLE);
        assert_eq!(app.footer_hint_uses.get(PERMISSION_CYCLE).copied(), Some(1));
        assert!(!retired(&app.footer_hint_uses, PERMISSION_CYCLE));
        app.note_footer_hint_used(PERMISSION_CYCLE);
        assert!(retired(&app.footer_hint_uses, PERMISSION_CYCLE));
        // Other keys are unaffected.
        assert!(!retired(&app.footer_hint_uses, MODE_CYCLE));
    }
}
