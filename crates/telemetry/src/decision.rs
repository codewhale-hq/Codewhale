//! The one place that decides whether anonymous usage counting may run, and the
//! token that makes that decision unforgeable.
//!
//! Every emitting surface calls [`decide`] and then, if and only if it gets
//! [`TelemetryDecision::Enabled`], hands the contained [`TelemetryConsent`] to
//! [`crate::init`]. `TelemetryConsent` has no `Default`, no public constructor,
//! and cannot be built from a `bool`; `init` takes it **by value**. That is what
//! makes the permission decision enforceable by the type system rather than by six init sites
//! each remembering to re-check the same five-part predicate.

use std::path::{Path, PathBuf};

use codewhale_config::{ResolvedRuntimeOptions, SetupState, TelemetrySource};

use crate::buffer;
use crate::event::Surface;

/// Directory name under `$CODEWHALE_HOME` that holds every telemetry file.
pub const TELEMETRY_DIR: &str = "telemetry";

/// The outcome of the emit predicate.
///
/// The split between [`Self::OptedOut`] and [`Self::ForcedOff`] is load-bearing,
/// not cosmetic. A run-scoped kill switch also resolves telemetry to false, so
/// a wipe keyed on that value would delete a user's identity and unflushed
/// buffer every time they ran one `codewhale exec` with a
/// transient `CODEWHALE_TELEMETRY=0` — the recipe the runtime docs themselves
/// prescribe.
#[derive(Debug)]
pub enum TelemetryDecision {
    /// Anonymous usage counting is enabled and nothing forces it off.
    Enabled(TelemetryConsent),
    /// A human persistently said no — `telemetry = false` in durable config or
    /// declining the notice. **The only variant that touches disk**: it wipes
    /// and leaves a tombstone. CLI and environment false values are run-scoped
    /// kill switches and produce [`Self::ForcedOff`] instead.
    OptedOut,
    /// Off for a run-scoped or environmental reason: an unparseable env value,
    /// an unresolvable home, or a rejected endpoint. Touches nothing, ever.
    /// Leaves identity and buffer exactly as they were.
    ForcedOff,
}

impl TelemetryDecision {
    /// Whether this decision permits emission.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }

    /// A stable label for logs and tests.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Enabled(_) => "enabled",
            Self::OptedOut => "opted_out",
            Self::ForcedOff => "forced_off",
        }
    }
}

/// Proof that a specific machine, at a specific moment, was permitted to
/// collect.
///
/// Constructed only by [`decide`]. Not `Default`, not constructible from a
/// `bool`, and consumed by value.
#[derive(Debug)]
pub struct TelemetryConsent {
    root: PathBuf,
    endpoint: Option<String>,
    surface: Surface,
    config_path: Option<PathBuf>,
    tombstone_generation: Option<buffer::TombstoneGeneration>,
}

impl TelemetryConsent {
    /// Remember which config file this process was launched with, so the flush
    /// path can re-resolve from it.
    ///
    /// Without this the documented mid-session opt-out —
    /// `codewhale config set telemetry false`, an external write by another
    /// process — would never be observed by a session that is already running.
    #[must_use]
    pub fn with_config_path(mut self, config_path: Option<PathBuf>) -> Self {
        self.config_path = config_path;
        self
    }

    /// The config file this process was launched with, if any.
    #[must_use]
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// `$CODEWHALE_HOME/telemetry`.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The validated endpoint, or `None` for the dry-run sink.
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The surface this consent was resolved for.
    #[must_use]
    pub fn surface(&self) -> Surface {
        self.surface
    }

    /// Exact opt-out generation this decision observed.
    pub(crate) fn tombstone_generation(&self) -> Option<&buffer::TombstoneGeneration> {
        self.tombstone_generation.as_ref()
    }
}

enum TelemetryEvaluation {
    Enabled {
        root: PathBuf,
        endpoint: Option<String>,
        tombstone_generation: Option<buffer::TombstoneGeneration>,
    },
    OptedOut(Option<PathBuf>),
    ForcedOff,
}

/// Why an endpoint was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    /// Not a URL we could parse at all.
    Unparseable,
    /// `http://` to something that is not loopback.
    InsecureScheme,
    /// A scheme that is neither `http` nor `https`.
    UnsupportedScheme,
}

impl EndpointError {
    /// A stable label for the single `warn` line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unparseable => "unparseable",
            Self::InsecureScheme => "plaintext http to a non-loopback host",
            Self::UnsupportedScheme => "scheme is neither https nor http",
        }
    }
}

/// Validate a configured endpoint.
///
/// `https://` is required. Plaintext is permitted **only** for loopback hosts,
/// where a batch never reaches a wire — that is the staging and dogfood case.
///
/// There is deliberately **no environment variable that overrides this**.
/// `CODEWHALE_ALLOW_INSECURE_HTTP` is not consulted: it authorizes an insecure
/// *provider* base URL, for harnesses that legitimately intercept model traffic,
/// and reusing it would let that interception decision also authorize plaintext
/// telemetry POSTs to an arbitrary host. Two unrelated trust decisions must not
/// share one switch, least of all in the subsystem whose whole promise is that
/// the user knows what leaves the machine.
pub fn validate_endpoint(raw: &str) -> Result<String, EndpointError> {
    let trimmed = raw.trim();
    let url = reqwest::Url::parse(trimmed).map_err(|_| EndpointError::Unparseable)?;
    match url.scheme() {
        "https" => Ok(trimmed.to_string()),
        "http" => {
            if is_loopback_host(url.host_str()) {
                Ok(trimmed.to_string())
            } else {
                Err(EndpointError::InsecureScheme)
            }
        }
        _ => Err(EndpointError::UnsupportedScheme),
    }
}

/// Whether a host is one a packet can never leave the machine to reach.
///
/// `Url::host_str` returns an IPv6 literal in its bracketed form (`[::1]`), so
/// the brackets come off before the address is parsed. Anything that parses as
/// an IP is judged by `is_loopback` — 127.0.0.0/8 and `::1` — and the only
/// accepted name is `localhost`.
fn is_loopback_host(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<std::net::IpAddr>() {
        Ok(address) => address.is_loopback(),
        Err(_) => bare.eq_ignore_ascii_case("localhost"),
    }
}

/// Resolve the emit predicate, reading the Codewhale home from the environment.
///
/// See [`decide_in_home`] for the injectable form used by tests.
pub fn decide(
    resolved: &ResolvedRuntimeOptions,
    setup: &SetupState,
    surface: Surface,
) -> TelemetryDecision {
    // `codewhale_home()` returns `Ok(None)` when no home can be resolved, and
    // an error when an explicit override was unusable. Both are "we have
    // nowhere to keep state", which is `ForcedOff`, never a wipe.
    let home = codewhale_paths::codewhale_home().ok().flatten();
    decide_in_home(home.as_deref(), resolved, setup, surface)
}

/// Load the privacy-bearing setup record for a telemetry decision.
///
/// A genuinely missing record is a fresh installation and therefore uses the
/// documented default. An existing record that cannot be read or parsed may
/// contain a durable decline, so it fails closed instead of being replaced by
/// a fresh default.
#[must_use]
pub fn load_setup_state_for_decision() -> Option<SetupState> {
    let path = SetupState::path().ok()?;
    load_setup_state_for_decision_at(&path)
}

/// Injectable form of [`load_setup_state_for_decision`] used by every surface
/// and by regression tests.
#[must_use]
pub fn load_setup_state_for_decision_at(path: &Path) -> Option<SetupState> {
    match path.try_exists() {
        Ok(false) => Some(SetupState::default()),
        Ok(true) => SetupState::load_from(path),
        Err(_) => None,
    }
}

/// Resolve the emit predicate against an explicit Codewhale home.
///
/// The predicate, in order:
///
/// 1. Telemetry resolved to `false` from persistent config → `OptedOut`;
///    resolved `false` from a run-scoped or invalid-value floor → `ForcedOff`.
/// 2. Any recorded notice decline → `OptedOut`, including a decline recorded
///    by the former opt-in notice.
/// 3. No resolvable home → `ForcedOff`.
/// 4. Endpoint configured but refused by [`validate_endpoint`] → `ForcedOff`.
/// 5. Otherwise `Enabled`, including an absent configuration preference.
///
/// The disclosure is policy metadata, not a consent gate. Every armed surface
/// presents a startup disclosure; the TUI also has a localized notice.
pub fn decide_in_home(
    home: Option<&Path>,
    resolved: &ResolvedRuntimeOptions,
    setup: &SetupState,
    surface: Surface,
) -> TelemetryDecision {
    match evaluate_in_home(home, resolved, setup) {
        TelemetryEvaluation::Enabled {
            root,
            endpoint,
            tombstone_generation,
        } => TelemetryDecision::Enabled(TelemetryConsent {
            root,
            endpoint,
            surface,
            config_path: None,
            tombstone_generation,
        }),
        TelemetryEvaluation::OptedOut(root) => opted_out(root.as_deref()),
        TelemetryEvaluation::ForcedOff => TelemetryDecision::ForcedOff,
    }
}

/// Evaluate the permission predicate without performing the opt-out wipe.
///
/// Keeping the classification pure lets `init` re-check it while holding the
/// privacy lock. The public decision path maps `OptedOut` to the destructive
/// wipe exactly once, outside that already-held lock.
fn evaluate_in_home(
    home: Option<&Path>,
    resolved: &ResolvedRuntimeOptions,
    setup: &SetupState,
) -> TelemetryEvaluation {
    let root = home.map(|home| home.join(TELEMETRY_DIR));

    // 1. An explicit persistent "off" is an opt-out and wipes. Run-scoped or
    //    invalid-value false is only a kill switch and leaves disk alone.
    if !resolved.telemetry {
        if resolved.telemetry_explicit_off
            || (resolved.telemetry_source == TelemetrySource::Default
                && setup.telemetry_opted_out())
        {
            // A run-scoped preference must not hide a durable decline in
            // the sidecar when the config register was never written.
            return TelemetryEvaluation::OptedOut(root);
        }
        return TelemetryEvaluation::ForcedOff;
    }

    // 2. A historical or current decline remains a durable opt-out. Notice
    //    version bumps may update disclosure, never reverse a user's "no".
    if setup.telemetry_opted_out() {
        return TelemetryEvaluation::OptedOut(root);
    }

    // Nowhere to keep an install id or a buffer.
    let Some(root) = root else {
        return TelemetryEvaluation::ForcedOff;
    };

    // 4. A refused endpoint is a configuration error, not a user answer.
    let endpoint = match resolved.telemetry_endpoint.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match validate_endpoint(raw) {
            Ok(endpoint) => Some(endpoint),
            Err(error) => {
                tracing::warn!(
                    "telemetry endpoint refused ({}); telemetry is off for this run",
                    error.label()
                );
                return TelemetryEvaluation::ForcedOff;
            }
        },
        _ => None,
    };

    let Ok(tombstone_generation) = buffer::tombstone_generation(&root) else {
        return TelemetryEvaluation::ForcedOff;
    };
    TelemetryEvaluation::Enabled {
        root,
        endpoint,
        tombstone_generation,
    }
}

/// Re-check the current durable permission without wiping or clearing state.
///
/// Called only while `init` holds the telemetry privacy lock. A stale consent
/// token may arm only when the config, setup-state answer, home, and endpoint
/// still classify as enabled.
pub(crate) fn permission_still_enabled(config_path: Option<&Path>, expected_root: &Path) -> bool {
    let Ok(setup_path) = SetupState::path() else {
        return false;
    };
    let home = codewhale_paths::codewhale_home().ok().flatten();
    permission_still_enabled_in_home(config_path, &setup_path, home.as_deref(), expected_root)
}

pub(crate) fn permission_still_enabled_in_home(
    config_path: Option<&Path>,
    setup_path: &Path,
    home: Option<&Path>,
    expected_root: &Path,
) -> bool {
    let Ok(store) = codewhale_config::ConfigStore::load(config_path.map(Path::to_path_buf)) else {
        return false;
    };
    let resolved = store
        .config
        .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());
    let Some(setup) = load_setup_state_for_decision_at(setup_path) else {
        return false;
    };
    matches!(
        evaluate_in_home(home, &resolved, &setup),
        TelemetryEvaluation::Enabled { root, .. } if root == expected_root
    )
}

/// Re-run the predicate from the filesystem, for the flush path.
///
/// Loads the same config file the process was launched with and the current
/// setup state, so a `codewhale config set telemetry false` written by another
/// process between init and flush is honoured. Returns `ForcedOff` if either
/// load fails: a flush is never the right place to guess.
#[must_use]
pub fn re_decide(config_path: Option<&Path>, surface: Surface) -> TelemetryDecision {
    let Ok(setup_path) = SetupState::path() else {
        return TelemetryDecision::ForcedOff;
    };
    re_decide_with_setup_path(config_path, &setup_path, surface)
}

pub(crate) fn re_decide_with_setup_path(
    config_path: Option<&Path>,
    setup_path: &Path,
    surface: Surface,
) -> TelemetryDecision {
    let Ok(store) = codewhale_config::ConfigStore::load(config_path.map(Path::to_path_buf)) else {
        return TelemetryDecision::ForcedOff;
    };
    let resolved = store
        .config
        .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());
    let Some(setup) = load_setup_state_for_decision_at(setup_path) else {
        return TelemetryDecision::ForcedOff;
    };
    decide(&resolved, &setup, surface)
}

/// Perform the opt-out wipe, then report `OptedOut`.
///
/// Nothing is created for a user who never opted in: if the telemetry directory
/// does not exist there is nothing to wipe and nothing to announce, so this
/// returns without touching the filesystem.
fn opted_out(root: Option<&Path>) -> TelemetryDecision {
    if let Some(root) = root
        && root.is_dir()
        && let Err(error) = buffer::wipe_on_startup(root)
    {
        // A failed wipe fails **closed**: the tombstone is written first and is
        // never removed by the wipe, so even a partial failure leaves the
        // buffer permanently undrainable.
        tracing::warn!("telemetry opt-out wipe was incomplete: {error}");
    }
    TelemetryDecision::OptedOut
}
