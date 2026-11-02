use crate::display_redaction::parse_bool;

/// Where an enabled session's batches go when nobody has said otherwise.
///
/// The first-party ingest service — a Cloudflare Worker that appends to Workers
/// Analytics Engine, with an optional disclosed PostHog sink. See
/// `docs/TELEMETRY.md` for what a batch contains and `telemetry-ingest/` for the handler.
///
/// This is a *default*, not a floor, and it changes nothing about permission:
/// `CODEWHALE_TELEMETRY=0`, `telemetry = false`, and a recorded decline all
/// stop the session long before an endpoint is read.
///
/// An explicit value — `CODEWHALE_TELEMETRY_ENDPOINT` or `telemetry_endpoint` in
/// the config file — wins outright, and an explicit *empty* value resolves to no
/// endpoint at all, which is the local dry-run sink: batches are serialized
/// exactly as a server would see them and appended to
/// `$CODEWHALE_HOME/telemetry/dryrun.jsonl`, and no HTTP client is constructed.
pub const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://telemetry.codewhale.net/v1/telemetry";

/// Provider-neutral credential value forwarded from the CLI dispatcher to the
/// in-process TUI when `--api-key` must survive profile-late route selection.
pub const CLI_API_KEY_ENV: &str = "CODEWHALE_CLI_API_KEY";

/// Source marker paired with [`CLI_API_KEY_ENV`] on the CLI-to-TUI boundary.
pub const CLI_API_KEY_SOURCE_ENV: &str = "CODEWHALE_CLI_API_KEY_SOURCE";

/// Read-only compatibility alias used by dispatchers before v0.9.12.
pub const LEGACY_CLI_API_KEY_SOURCE_ENV: &str = "DEEPSEEK_API_KEY_SOURCE";

/// The dispatcher's statement to the TUI child about *why* telemetry is off.
///
/// Private to the `codewhale` → `codewhale-tui` hop, in the same spirit as
/// [`CLI_API_KEY_SOURCE_ENV`]. Set to `1`/`0` on every delegated run.
pub const TELEMETRY_FLOOR_ENV: &str = "CODEWHALE_TELEMETRY_FLOOR";

/// Whether an environment-level kill switch forces telemetry off here.
///
/// A floor is *not* the same as "telemetry resolved to false": off is the
/// default, and the dispatcher forwards a resolved `CODEWHALE_TELEMETRY=false`
/// on every ordinary run, so a child reading only that value cannot tell an
/// operator's declared kill switch from the shipped default. That distinction
/// matters exactly once — the first-run notice must not ask a question whose
/// answer this environment overrides — so the dispatcher states it outright in
/// [`TELEMETRY_FLOOR_ENV`] and the child believes the statement.
///
/// With no statement (a directly launched `codewhale-tui`) the raw environment
/// is read instead, where an explicit "off" or an unreadable value is a floor.
#[must_use]
pub fn telemetry_floor_in_force() -> bool {
    if do_not_track() {
        return true;
    }
    if let Ok(raw) = std::env::var(TELEMETRY_FLOOR_ENV)
        && let Ok(declared) = parse_bool(&raw)
    {
        return declared;
    }
    let Ok(raw) =
        std::env::var("CODEWHALE_TELEMETRY").or_else(|_| std::env::var("DEEPSEEK_TELEMETRY"))
    else {
        return false;
    };
    !matches!(parse_bool(&raw), Ok(true))
}

fn do_not_track() -> bool {
    std::env::var("DO_NOT_TRACK").is_ok_and(|raw| matches!(parse_bool(&raw), Ok(true)))
}

/// Where resolved telemetry consent came from (#5441).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetrySource {
    /// `--telemetry` on this run's command line.
    Cli,
    /// `CODEWHALE_TELEMETRY`/`DEEPSEEK_TELEMETRY`, including the dispatcher's
    /// floor statement and every environment kill switch.
    Env,
    /// `telemetry = …` written to the config file.
    Config,
    /// Nobody said anything; the shipped preference is on.
    Default,
}

impl TelemetrySource {
    /// Stable label for the doctor row and config display.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Env => "env",
            Self::Config => "config",
            Self::Default => "default",
        }
    }
}

/// Read the telemetry environment override, reporting an unreadable value
/// instead of swallowing it.
///
/// Returns `(value, invalid)`. `invalid` is `true` only when the variable
/// was set to something [`parse_bool`] rejected; an unset variable is
/// simply `(None, false)`. Shared by the runtime resolver and the
/// provenance surfaces so they cannot drift.
pub(crate) fn read_telemetry_env() -> (Option<bool>, bool) {
    if do_not_track() {
        return (Some(false), false);
    }
    let Some(raw) = std::env::var("CODEWHALE_TELEMETRY")
        .or_else(|_| std::env::var("DEEPSEEK_TELEMETRY"))
        .ok()
    else {
        return (None, false);
    };
    match parse_bool(&raw) {
        Ok(value) => (Some(value), false),
        Err(_) => {
            tracing::warn!(
                "Invalid CODEWHALE_TELEMETRY/DEEPSEEK_TELEMETRY value; expected one of \
                 1/0, true/false, yes/no, on/off, enabled/disabled. Telemetry is forced off."
            );
            (None, true)
        }
    }
}

/// Resolved telemetry consent with its source, for surfaces that hold the
/// config-file value but not the full CLI/env resolution chain — the doctor
/// runtime-posture row and the `config get telemetry` display (#5441).
///
/// This is the same resolution `ConfigToml::resolve_runtime_options`
/// applies without its CLI term: environment first (an explicit value, an
/// unreadable one, or a dispatcher floor), then the file, then the shipped
/// default of `on`; a persisted `telemetry = false` is a floor no later
/// term can climb over. The runtime resolver calls this directly, so the
/// preference surfaces agree. The telemetry owner also checks historical
/// durable declines and fails closed on unreadable privacy state.
#[must_use]
pub fn resolved_telemetry_consent(file_telemetry: Option<bool>) -> (bool, TelemetrySource) {
    let (env_telemetry, env_invalid) = read_telemetry_env();
    telemetry_consent_from_env(
        env_telemetry,
        env_invalid,
        telemetry_floor_in_force(),
        file_telemetry,
    )
}

/// The decision core shared by [`resolved_telemetry_consent`] and the runtime
/// resolver, which already holds a snapshot of the same environment facts.
#[must_use]
pub(crate) fn telemetry_consent_from_env(
    env_telemetry: Option<bool>,
    env_invalid: bool,
    floor: bool,
    file_telemetry: Option<bool>,
) -> (bool, TelemetrySource) {
    let persisted_off = file_telemetry == Some(false);
    let allowed = env_telemetry.or(file_telemetry).unwrap_or(true);
    let on = allowed && env_telemetry != Some(false) && !env_invalid && !floor && !persisted_off;
    let source = if !on && (env_telemetry == Some(false) || env_invalid || floor) {
        // An environment kill switch decided the outcome.
        TelemetrySource::Env
    } else if !on && persisted_off {
        // The persistent opt-out outranked everything else in play.
        TelemetrySource::Config
    } else if env_telemetry.is_some() {
        TelemetrySource::Env
    } else if file_telemetry.is_some() {
        TelemetrySource::Config
    } else {
        TelemetrySource::Default
    };
    (on, source)
}
