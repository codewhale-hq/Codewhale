//! Eligibility and persistence for the interactive telemetry disclosure.
//!
//! Rendering belongs to the native TUI event loop.
//! This module owns only the privacy-sensitive state transitions: deciding
//! whether disclosure is owed and applying the explicit Settings preference.
//! Drawing a notice never records acceptance or arms collection.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use codewhale_config::{SetupState, TELEMETRY_NOTICE_VERSION};
use codewhale_telemetry::SessionSource;

use codewhale_localization::{Locale, MessageId, tr};

/// Marker for a nonblocking disclosure of default-on usage and Settings opt-out.
#[derive(Debug, Clone)]
pub struct PendingTelemetryNotice;

/// Whether an interactive launch owes the native notice, may arm immediately,
/// or must stay unarmed because the durable privacy state could not be read.
#[derive(Debug)]
pub enum TelemetryNoticePlan {
    Due(PendingTelemetryNotice),
    NotDue,
    SuppressArming,
}

impl TelemetryNoticePlan {
    pub fn should_arm_before_tui(&self) -> bool {
        !matches!(self, Self::SuppressArming)
    }

    pub fn into_pending(self) -> Option<PendingTelemetryNotice> {
        match self {
            Self::Due(pending) => Some(pending),
            Self::NotDue | Self::SuppressArming => None,
        }
    }
}

/// Typed result of changing the durable telemetry preference from `/settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedTelemetryPreference {
    outcome: TelemetryPreferenceOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TelemetryPreferenceOutcome {
    EnabledNextLaunch,
    Disabled,
    DisabledWithWarning(String),
    DisabledForSession(String),
    SaveFailed(String),
}

impl AppliedTelemetryPreference {
    pub fn is_error(&self) -> bool {
        matches!(
            self.outcome,
            TelemetryPreferenceOutcome::DisabledWithWarning(_)
                | TelemetryPreferenceOutcome::DisabledForSession(_)
                | TelemetryPreferenceOutcome::SaveFailed(_)
        )
    }

    pub fn message(&self, locale: Locale) -> String {
        let (id, detail) = match &self.outcome {
            TelemetryPreferenceOutcome::EnabledNextLaunch => {
                (MessageId::TelemetryPreferenceEnabledNextLaunch, None)
            }
            TelemetryPreferenceOutcome::Disabled => (MessageId::TelemetryPreferenceDisabled, None),
            TelemetryPreferenceOutcome::DisabledWithWarning(detail) => (
                MessageId::TelemetryPreferenceDisabledWithWarning,
                Some(detail),
            ),
            TelemetryPreferenceOutcome::DisabledForSession(detail) => (
                MessageId::TelemetryPreferenceDisabledForSession,
                Some(detail),
            ),
            TelemetryPreferenceOutcome::SaveFailed(detail) => {
                (MessageId::TelemetryPreferenceSaveFailed, Some(detail))
            }
        };
        let mut message = tr(locale, id).into_owned();
        if let Some(detail) = detail {
            message = message.replace("{detail}", detail);
        }
        message
    }
}

/// Return a native-notice plan when this interactive launch owes disclosure.
///
/// This is read-only. It never prints, blocks on a line read, creates telemetry
/// state, or records a fictional answer. `--skip-onboarding` intentionally has
/// no bearing on a privacy disclosure.
pub fn plan_if_due(
    config_path: Option<PathBuf>,
    session_source: SessionSource,
) -> TelemetryNoticePlan {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return TelemetryNoticePlan::NotDue;
    }

    let store = match codewhale_config::ConfigStore::load(config_path) {
        Ok(store) => store,
        Err(error) => {
            // A config we cannot read is a config we must not write.
            tracing::warn!("telemetry stays unarmed; config unreadable: {error}");
            return TelemetryNoticePlan::SuppressArming;
        }
    };
    let setup_state_path = match SetupState::path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!("telemetry stays unarmed; setup-state path unavailable: {error}");
            return TelemetryNoticePlan::SuppressArming;
        }
    };
    plan_for_store_and_state(store, setup_state_path, session_source)
}

fn plan_for_store_and_state(
    store: codewhale_config::ConfigStore,
    setup_state_path: PathBuf,
    _session_source: SessionSource,
) -> TelemetryNoticePlan {
    let resolved = store
        .config
        .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());
    let state = match load_notice_state_at(&setup_state_path) {
        Ok(state) => state,
        Err(error) => {
            // Never replace a corrupt constitution/setup sidecar with a fresh
            // telemetry-only record. The next successful setup repair can
            // make this notice eligible again.
            tracing::warn!("telemetry stays unarmed; setup state unreadable: {error}");
            return TelemetryNoticePlan::SuppressArming;
        }
    };
    let gate = NoticeGate {
        needs_notice: state.needs_telemetry_notice(TELEMETRY_NOTICE_VERSION),
        persisted_off: resolved.telemetry_explicit_off,
        recorded_opt_out: state.telemetry_opted_out(),
        floor_in_force: codewhale_config::telemetry_floor_in_force(),
    };
    if gate.may_ask() {
        TelemetryNoticePlan::Due(PendingTelemetryNotice)
    } else {
        TelemetryNoticePlan::NotDue
    }
}

/// Record that the interactive disclosure was drawn for this policy version.
///
/// Display bookkeeping only: it never touches the preference registers, and a
/// failed write merely repeats the notice on a later launch.
pub fn record_presented() {
    let Ok(path) = SetupState::path() else {
        return;
    };
    let _ = SetupState::update_at(&path, |state| {
        state.record_telemetry_notice_shown(TELEMETRY_NOTICE_VERSION);
    });
}

/// Return the saved telemetry preference shown by `/settings`.
///
/// A missing preference defaults on. An existing
/// unreadable record may contain an opt-out, so the Settings row fails closed
/// and shows Off rather than guessing.
pub fn saved_preference_enabled(config: &codewhale_runtime::config::Config) -> bool {
    let Ok(path) = SetupState::path() else {
        return false;
    };
    saved_preference_enabled_at(config, &path)
}

fn saved_preference_enabled_at(
    config: &codewhale_runtime::config::Config,
    setup_state_path: &Path,
) -> bool {
    if config.telemetry == Some(false) {
        return false;
    }
    match codewhale_telemetry::load_setup_state_for_decision_at(setup_state_path) {
        // The telemetry owner returns a fresh default state for a genuinely
        // absent sidecar. `None` therefore means an existing record was
        // unreadable (or the path could not be inspected) and must fail closed.
        Some(state) => !state.telemetry_opted_out(),
        None => false,
    }
}

/// Persist the `/settings` telemetry toggle through the same two privacy
/// registers as the CLI preference command.
///
/// Turning off is successful when either durable register records the opt-out;
/// existing local telemetry is then wiped under the telemetry ordering lock.
/// The wipe writes its tombstone first, so a partial erase still fails closed.
/// Turning on is stricter: both registers must agree before the UI reports the
/// preference as enabled. Re-enabling takes effect for new sessions so a
/// process that was disabled never starts collecting again behind the user's
/// back.
pub fn apply_persistent_preference(
    config_path: Option<PathBuf>,
    enabled: bool,
) -> AppliedTelemetryPreference {
    let setup_state_path = match SetupState::path() {
        Ok(path) => path,
        Err(error) => {
            return AppliedTelemetryPreference {
                outcome: TelemetryPreferenceOutcome::SaveFailed(bounded_failure_detail(
                    error.to_string(),
                )),
            };
        }
    };
    let telemetry_root = codewhale_config::codewhale_home()
        .ok()
        .map(|home| home.join(codewhale_telemetry::TELEMETRY_DIR));
    apply_persistent_preference_at(config_path, setup_state_path, telemetry_root, enabled)
}

fn apply_persistent_preference_at(
    config_path: Option<PathBuf>,
    setup_state_path: PathBuf,
    telemetry_root: Option<PathBuf>,
    enabled: bool,
) -> AppliedTelemetryPreference {
    if enabled {
        // Keep a durable Off floor in place until both privacy registers have
        // accepted the explicit re-enable. This makes every partial failure
        // resolve Off on the next launch.
        match load_notice_state_at(&setup_state_path) {
            Ok(_) => {}
            Err(error) => {
                return AppliedTelemetryPreference {
                    outcome: TelemetryPreferenceOutcome::SaveFailed(bounded_failure_detail(
                        format!("privacy record: {error}"),
                    )),
                };
            }
        };
        if let Err(error) = write_config_preference(config_path.clone(), false) {
            return AppliedTelemetryPreference {
                outcome: TelemetryPreferenceOutcome::SaveFailed(bounded_failure_detail(format!(
                    "config: {error}"
                ))),
            };
        }
        if let Err(error) = SetupState::update_at(&setup_state_path, |state| {
            state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, true);
        }) {
            return AppliedTelemetryPreference {
                outcome: TelemetryPreferenceOutcome::SaveFailed(bounded_failure_detail(format!(
                    "privacy record: {error}"
                ))),
            };
        }
        if let Err(error) = write_config_preference(config_path.clone(), true) {
            let mut failures = vec![format!("config: {error}")];
            if let Err(rollback) = write_config_preference(config_path, false) {
                failures.push(format!("restoring the config Off floor: {rollback}"));
            }
            // The config Off floor is authoritative, but put the privacy
            // sidecar back in the same fail-closed state as well. Otherwise a
            // later manual edit could expose the partial enable as consent.
            if let Err(rollback) = SetupState::update_at(&setup_state_path, |state| {
                state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, false);
            }) {
                failures.push(format!("restoring the privacy opt-out: {rollback}"));
            }
            return AppliedTelemetryPreference {
                outcome: TelemetryPreferenceOutcome::SaveFailed(bounded_failure_detail(
                    failures.join("; "),
                )),
            };
        }
        return AppliedTelemetryPreference {
            outcome: TelemetryPreferenceOutcome::EnabledNextLaunch,
        };
    }

    let config_result = write_config_preference(config_path, false);
    let state_result = SetupState::update_at(&setup_state_path, |state| {
        state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, false);
    });
    // Wipe even if both durable writes fail: a successful tombstone stops the
    // already-armed process for the remainder of this session.
    let wipe_result = match telemetry_root.as_deref().filter(|root| root.is_dir()) {
        Some(root) => codewhale_telemetry::buffer::wipe(root),
        None => Ok(()),
    };
    let durable = config_result.is_ok() || state_result.is_ok();
    if !durable {
        let mut failures = vec![
            format!("config: {}", config_result.expect_err("failed result")),
            format!(
                "privacy record: {}",
                state_result.expect_err("failed result")
            ),
        ];
        if let Err(error) = wipe_result {
            failures.push(format!("local erase: {error}"));
            return AppliedTelemetryPreference {
                outcome: TelemetryPreferenceOutcome::SaveFailed(bounded_failure_detail(
                    failures.join("; "),
                )),
            };
        }
        let detail = bounded_failure_detail(failures.join("; "));
        return AppliedTelemetryPreference {
            outcome: TelemetryPreferenceOutcome::DisabledForSession(detail),
        };
    }

    let mut warnings = Vec::new();
    if let Err(error) = config_result {
        warnings.push(format!("config: {error}"));
    }
    if let Err(error) = state_result {
        warnings.push(format!("privacy record: {error}"));
    }
    if let Err(error) = wipe_result {
        warnings.push(format!("local erase: {error}"));
    }
    if warnings.is_empty() {
        AppliedTelemetryPreference {
            outcome: TelemetryPreferenceOutcome::Disabled,
        }
    } else {
        AppliedTelemetryPreference {
            outcome: TelemetryPreferenceOutcome::DisabledWithWarning(bounded_failure_detail(
                warnings.join("; "),
            )),
        }
    }
}

fn bounded_failure_detail(detail: String) -> String {
    const MAX_CHARS: usize = 240;
    let single_line = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = single_line.chars();
    let bounded = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn write_config_preference(config_path: Option<PathBuf>, enabled: bool) -> Result<()> {
    let mut store = codewhale_config::ConfigStore::load(config_path)?;
    store
        .config
        .set_value("telemetry", if enabled { "true" } else { "false" })?;
    store.save()
}

/// Load a missing sidecar as a fresh state, but distinguish it from an
/// existing unreadable/corrupt sidecar so the notice can never overwrite the
/// latter with defaults.
fn load_notice_state_at(path: &Path) -> Result<SetupState> {
    if !path
        .try_exists()
        .map_err(|error| anyhow!("could not inspect {}: {error}", path.display()))?
    {
        return Ok(SetupState::default());
    }
    SetupState::load_from(path)
        .ok_or_else(|| anyhow!("{} could not be read as setup state", path.display()))
}

/// Everything that decides whether the disclosure may be shown.
struct NoticeGate {
    needs_notice: bool,
    persisted_off: bool,
    recorded_opt_out: bool,
    floor_in_force: bool,
}

impl NoticeGate {
    fn may_ask(&self) -> bool {
        self.needs_notice && !self.persisted_off && !self.recorded_opt_out && !self.floor_in_force
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(
        needs_notice: bool,
        persisted_off: bool,
        recorded_opt_out: bool,
        floor_in_force: bool,
    ) -> NoticeGate {
        NoticeGate {
            needs_notice,
            persisted_off,
            recorded_opt_out,
            floor_in_force,
        }
    }

    #[test]
    fn the_notice_is_not_put_to_someone_who_already_answered_durably() {
        assert!(gate(true, false, false, false).may_ask());
        assert!(!gate(true, true, false, false).may_ask());
        assert!(!gate(true, false, true, false).may_ask());
        assert!(!gate(true, false, false, true).may_ask());
        assert!(!gate(false, false, false, false).may_ask());
    }

    #[test]
    fn a_due_disclosure_does_not_gate_default_on_collection() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let store = codewhale_config::ConfigStore::load(Some(config_path)).unwrap();
        let plan = plan_for_store_and_state(
            store,
            dir.path().join("setup_state.json"),
            SessionSource::Interactive,
        );
        assert!(matches!(plan, TelemetryNoticePlan::Due(_)));
        assert!(plan.should_arm_before_tui());
        assert!(
            !dir.path().join("setup_state.json").exists(),
            "planning is not presentation or acceptance"
        );
    }

    #[test]
    fn corrupt_setup_state_is_never_replaced_with_telemetry_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let state_path = dir.path().join("setup_state.json");
        std::fs::write(&config_path, "").expect("seed config");
        std::fs::write(&state_path, "not-json").expect("seed corrupt state");

        assert!(load_notice_state_at(&state_path).is_err());
        let store = codewhale_config::ConfigStore::load(Some(config_path)).expect("load config");
        let plan = plan_for_store_and_state(store, state_path.clone(), SessionSource::Interactive);
        assert!(matches!(&plan, TelemetryNoticePlan::SuppressArming));
        assert!(
            !plan.should_arm_before_tui(),
            "unreadable privacy state must fail closed instead of arming by default"
        );
        assert_eq!(
            std::fs::read_to_string(&state_path).expect("read corrupt state"),
            "not-json"
        );
    }

    #[test]
    fn settings_preference_defaults_on_and_preserves_old_declines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("setup_state.json");
        let config = {
            let __cw_config_init_0_0 = Some(true);
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config =
                codewhale_runtime::config::Config::default();
            __cw_config_init_0_value.telemetry = __cw_config_init_0_0;
            __cw_config_init_0_value
        };

        assert!(
            saved_preference_enabled_at(&config, &state_path),
            "a missing privacy record uses default-on"
        );

        let mut state = SetupState::default();
        state.record_telemetry_notice("3", true);
        state.save_to(&state_path).expect("seed old acceptance");
        assert!(saved_preference_enabled_at(&config, &state_path));
        state.record_telemetry_notice("4", false);
        state.save_to(&state_path).expect("seed historical decline");
        assert!(!saved_preference_enabled_at(&config, &state_path));
        state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, true);
        state.save_to(&state_path).expect("seed current acceptance");
        assert!(saved_preference_enabled_at(&config, &state_path));
        assert!(saved_preference_enabled_at(
            &codewhale_runtime::config::Config::default(),
            &state_path
        ));

        std::fs::write(&state_path, "not-json").expect("seed corrupt state");
        assert!(
            !saved_preference_enabled_at(&config, &state_path),
            "an unreadable privacy record may contain an opt-out"
        );

        let explicitly_off = {
            let __cw_config_init_1_0 = Some(false);
            let mut __cw_config_init_1_value: codewhale_runtime::config::Config =
                codewhale_runtime::config::Config::default();
            __cw_config_init_1_value.telemetry = __cw_config_init_1_0;
            __cw_config_init_1_value
        };
        assert!(!saved_preference_enabled_at(&explicitly_off, &state_path));
    }

    #[test]
    fn settings_off_persists_both_registers_and_stops_the_armed_buffer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let state_path = dir.path().join("setup_state.json");
        let telemetry_root = dir.path().join("telemetry");
        std::fs::write(&config_path, "telemetry = true\n").expect("seed config");
        std::fs::create_dir_all(&telemetry_root).expect("seed telemetry root");
        std::fs::write(
            codewhale_telemetry::buffer::buffer_path(&telemetry_root),
            "queued-event\n",
        )
        .expect("seed buffer");

        let applied = apply_persistent_preference_at(
            Some(config_path.clone()),
            state_path.clone(),
            Some(telemetry_root.clone()),
            false,
        );

        assert_eq!(applied.outcome, TelemetryPreferenceOutcome::Disabled);
        assert!(
            std::fs::read_to_string(&config_path)
                .expect("read config")
                .contains("telemetry = false")
        );
        assert!(
            SetupState::load_from(&state_path)
                .expect("saved state")
                .telemetry_opted_out()
        );
        assert!(codewhale_telemetry::buffer::tombstone_present(
            &telemetry_root
        ));
        assert_eq!(
            std::fs::read_to_string(codewhale_telemetry::buffer::buffer_path(&telemetry_root))
                .expect("read wiped buffer"),
            ""
        );
    }

    #[test]
    fn settings_on_is_saved_for_next_launch_without_clearing_the_tombstone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let state_path = dir.path().join("setup_state.json");
        let telemetry_root = dir.path().join("telemetry");
        std::fs::write(&config_path, "telemetry = true\n").expect("seed config");
        std::fs::create_dir_all(&telemetry_root).expect("seed telemetry root");
        let disabled = apply_persistent_preference_at(
            Some(config_path.clone()),
            state_path.clone(),
            Some(telemetry_root.clone()),
            false,
        );
        assert_eq!(disabled.outcome, TelemetryPreferenceOutcome::Disabled);

        let enabled = apply_persistent_preference_at(
            Some(config_path.clone()),
            state_path.clone(),
            Some(telemetry_root.clone()),
            true,
        );

        assert_eq!(
            enabled.outcome,
            TelemetryPreferenceOutcome::EnabledNextLaunch
        );
        assert!(
            std::fs::read_to_string(&config_path)
                .expect("read config")
                .contains("telemetry = true")
        );
        assert!(
            SetupState::load_from(&state_path)
                .expect("saved state")
                .telemetry_accepted(TELEMETRY_NOTICE_VERSION)
        );
        assert!(
            codewhale_telemetry::buffer::tombstone_present(&telemetry_root),
            "the already-running process stays off; the next launch clears this after a fresh permission check"
        );

        let store = codewhale_config::ConfigStore::load(Some(config_path))
            .expect("reload enabled config for the next launch");
        let resolved = store
            .config
            .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());
        let reloaded_state = SetupState::load_from(&state_path).expect("reload enabled state");
        assert!(
            codewhale_telemetry::decide_in_home(
                Some(dir.path()),
                &resolved,
                &reloaded_state,
                codewhale_telemetry::Surface::Tui,
            )
            .is_enabled(),
            "a fresh launch may clear the prior tombstone only after both saved registers resolve enabled"
        );
    }

    #[test]
    fn failed_settings_enable_preserves_the_existing_opt_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().to_path_buf();
        let state_path = dir.path().join("setup_state.json");
        let mut state = SetupState::default();
        state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, false);
        state.save_to(&state_path).expect("seed opt-out");

        let applied =
            apply_persistent_preference_at(Some(config_path), state_path.clone(), None, true);

        assert!(matches!(
            applied.outcome,
            TelemetryPreferenceOutcome::SaveFailed(_)
        ));
        assert!(
            SetupState::load_from(&state_path)
                .expect("saved state")
                .telemetry_opted_out()
        );
    }

    #[test]
    fn unsaved_settings_off_still_suppresses_the_current_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().to_path_buf();
        let state_path = dir.path().join("setup_state.json");
        let telemetry_root = dir.path().join("telemetry");
        std::fs::write(&state_path, "not-json").expect("seed corrupt state");
        std::fs::create_dir_all(&telemetry_root).expect("seed telemetry root");

        let applied = apply_persistent_preference_at(
            Some(config_path),
            state_path,
            Some(telemetry_root.clone()),
            false,
        );

        assert!(matches!(
            applied.outcome,
            TelemetryPreferenceOutcome::DisabledForSession(_)
        ));
        assert!(codewhale_telemetry::buffer::tombstone_present(
            &telemetry_root
        ));
    }

    #[test]
    fn a_non_tty_test_surface_cannot_schedule_the_native_notice() {
        assert!(matches!(
            plan_if_due(None, SessionSource::Interactive),
            TelemetryNoticePlan::NotDue
        ));
    }
}
