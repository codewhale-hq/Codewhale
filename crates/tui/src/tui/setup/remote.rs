//! Remote runtime setup step (#3409).
//!
//! The step answers one question — *where can this agent be reached from?* —
//! with four modes and no aspiration:
//!
//! | mode | what it actually is |
//! |------|---------------------|
//! | [`RemoteMode::LocalOnly`] | the default: this machine, nothing exposed |
//! | [`RemoteMode::RuntimeApi`] | `codewhale serve --http` on loopback, token-authenticated |
//! | [`RemoteMode::MobileLan`] | the same runtime reachable from a phone on the LAN |
//! | [`RemoteMode::ChatBridge`] | a chat bridge (Telegram, Feishu/Lark) in front of the runtime |
//!
//! Every status is derived from a fact this process can actually observe —
//! whether a credential *name* is set in the environment, what the shipped
//! runtime unit binds to, and what the bridge registry declares. Two rules are
//! absolute and are pinned by tests:
//!
//! 1. **Secret values are never read and never rendered.** Only the *name* of
//!    an environment variable and whether it is set are used.
//! 2. **Nothing is written, applied, or provisioned.** The plan preview is
//!    rendered in memory through the existing `remote_setup::bundle` contract
//!    with redacted placeholders.

use crate::tui::app::App;
use codewhale_app_server::remote_setup::bundle;
use codewhale_app_server::remote_setup::registry;
use codewhale_localization::{Locale, MessageId, tr};

/// Env var carrying the runtime API bearer token, plus the legacy alias the
/// runtime still honors. Only presence is ever inspected.
const RUNTIME_TOKEN_VARS: [&str; 2] = ["CODEWHALE_RUNTIME_TOKEN", "DEEPSEEK_RUNTIME_TOKEN"];

/// Env var a user sets to bind the runtime somewhere other than loopback. The
/// shipped systemd unit hardcodes `127.0.0.1`, so without this the LAN/mobile
/// mode is genuinely unavailable rather than merely unconfigured.
const RUNTIME_HOST_VARS: [&str; 2] = ["CODEWHALE_RUNTIME_HOST", "DEEPSEEK_RUNTIME_HOST"];

/// Placeholder substituted for every secret in the generated plan preview.
const REDACTED: &str = "<redacted>";

/// The four ways a Codewhale runtime can be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteMode {
    LocalOnly,
    RuntimeApi,
    MobileLan,
    ChatBridge,
}

impl RemoteMode {
    /// Only the locale-parity test needs to walk every mode; the wizard renders
    /// whatever [`observe_modes`] actually produced.
    #[cfg(test)]
    pub(super) const ALL: [RemoteMode; 4] = [
        RemoteMode::LocalOnly,
        RemoteMode::RuntimeApi,
        RemoteMode::MobileLan,
        RemoteMode::ChatBridge,
    ];

    /// Stable id used in the persisted step result and in tests.
    pub(super) const fn id(self) -> &'static str {
        match self {
            RemoteMode::LocalOnly => "local_only",
            RemoteMode::RuntimeApi => "runtime_api",
            RemoteMode::MobileLan => "mobile_lan",
            RemoteMode::ChatBridge => "chat_bridge",
        }
    }

    pub(super) const fn label_id(self) -> MessageId {
        match self {
            RemoteMode::LocalOnly => MessageId::SetupRemoteModeLocalOnly,
            RemoteMode::RuntimeApi => MessageId::SetupRemoteModeRuntimeApi,
            RemoteMode::MobileLan => MessageId::SetupRemoteModeMobileLan,
            RemoteMode::ChatBridge => MessageId::SetupRemoteModeChatBridge,
        }
    }
}

/// What the user can do with a mode *right now*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteModeStatus {
    /// Not available on this install; no user action would change it here.
    Disabled,
    /// Usable as configured.
    Ready,
    /// Available, but something is missing. Never blocks `ready`.
    NeedsAction,
}

impl RemoteModeStatus {
    pub(super) const fn id(self) -> &'static str {
        match self {
            RemoteModeStatus::Disabled => "disabled",
            RemoteModeStatus::Ready => "ready",
            RemoteModeStatus::NeedsAction => "needs_action",
        }
    }

    pub(super) const fn label_id(self) -> MessageId {
        match self {
            RemoteModeStatus::Disabled => MessageId::SetupRemoteStatusDisabled,
            RemoteModeStatus::Ready => MessageId::SetupRemoteStatusReady,
            RemoteModeStatus::NeedsAction => MessageId::SetupRemoteStatusNeedsAction,
        }
    }
}

/// One observed mode. `detail` is a short, secret-free English fact used in the
/// persisted step result and the preview; the localized label is composed at
/// render time from `mode` and `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteModeFact {
    pub(super) mode: RemoteMode,
    pub(super) status: RemoteModeStatus,
    pub(super) detail: String,
}

/// Whether any of `vars` is set to a non-empty value. **Only presence is
/// observed** — the value is never bound to a name, logged, or returned.
fn any_var_present(vars: &[&str], lookup: &dyn Fn(&str) -> Option<String>) -> bool {
    vars.iter()
        .any(|name| lookup(name).is_some_and(|value| !value.trim().is_empty()))
}

/// The first var in `vars` that is set, by name only. Used so the UI can say
/// *which* variable satisfied the check without revealing what it holds.
fn present_var_name(
    vars: &[&'static str],
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<&'static str> {
    vars.iter()
        .copied()
        .find(|name| lookup(name).is_some_and(|value| !value.trim().is_empty()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SetupRemoteFacts {
    pub(super) modes: Vec<RemoteModeFact>,
    pub(super) clouds_result: String,
    pub(super) bridges_result: String,
    pub(super) providers_result: String,
    pub(super) mode_result: String,
    pub(super) command_provider: String,
    pub(super) result: String,
}

impl SetupRemoteFacts {
    pub(super) fn from_app(app: &App) -> Self {
        Self::from_app_with_env(app, &|name| std::env::var(name).ok())
    }

    /// Test seam: the env lookup is injected so the hostile-token and
    /// no-leak cases can be exercised without mutating the process
    /// environment.
    pub(super) fn from_app_with_env(app: &App, lookup: &dyn Fn(&str) -> Option<String>) -> Self {
        let cloud_slugs = registry::CLOUD_TARGETS
            .iter()
            .map(|cloud| cloud.slug)
            .collect::<Vec<_>>();
        let bridge_slugs = registry::BRIDGES
            .iter()
            .map(|bridge| bridge.slug)
            .collect::<Vec<_>>();
        let provider_count = codewhale_config::ProviderKind::all().len();
        // Keep the exact route identity. Named custom routes are not yet
        // representable by the remote bundle registry, so the generated CLI
        // command must fail explicitly for that name instead of silently
        // substituting DeepSeek's endpoint and credential contract.
        let command_provider = app.provider_identity_for_persistence().to_string();

        let modes = observe_modes(lookup);
        let mode_result = modes
            .iter()
            .map(|fact| format!("{}={}", fact.mode.id(), fact.status.id()))
            .collect::<Vec<_>>()
            .join(", ");

        Self {
            clouds_result: format!(
                "{} cloud targets: {}",
                cloud_slugs.len(),
                cloud_slugs.join(", ")
            ),
            bridges_result: format!(
                "{} chat bridges: {}",
                bridge_slugs.len(),
                bridge_slugs.join(", ")
            ),
            providers_result: format!(
                "{provider_count} providers from the provider registry; active route {} / {}",
                app.provider_identity_for_persistence(),
                app.model
            ),
            result: format!("{mode_result}; plan=generate_only, apply=not_implemented"),
            mode_result,
            command_provider,
            modes,
        }
    }

    /// The mode the step defaults to. Local-only is always the default and is
    /// always skippable in one key — a user who never wants remote access is
    /// done here immediately. Asserted by test rather than branched on: the
    /// wizard's Enter path is unconditional precisely because of this.
    #[cfg(test)]
    pub(super) fn default_mode(&self) -> RemoteMode {
        RemoteMode::LocalOnly
    }

    #[cfg(test)]
    pub(super) fn status_for(&self, mode: RemoteMode) -> RemoteModeStatus {
        self.modes
            .iter()
            .find(|fact| fact.mode == mode)
            .map_or(RemoteModeStatus::NeedsAction, |fact| fact.status)
    }

    /// True when something is missing but nothing is broken. Callers record
    /// `NeedsAction` — which by contract never blocks the ready screen.
    pub(super) fn needs_action(&self) -> bool {
        self.modes
            .iter()
            .any(|fact| fact.status == RemoteModeStatus::NeedsAction)
    }
}

/// Derive all four mode facts from observable state.
fn observe_modes(lookup: &dyn Fn(&str) -> Option<String>) -> Vec<RemoteModeFact> {
    let runtime_token = present_var_name(&RUNTIME_TOKEN_VARS, lookup);
    let lan_host = present_var_name(&RUNTIME_HOST_VARS, lookup);

    let runtime_api = match runtime_token {
        Some(name) => RemoteModeFact {
            mode: RemoteMode::RuntimeApi,
            status: RemoteModeStatus::Ready,
            detail: format!(
                "{name} is set; runtime serves 127.0.0.1:{} with token auth",
                bundle::DEFAULT_PORT
            ),
        },
        None => RemoteModeFact {
            mode: RemoteMode::RuntimeApi,
            status: RemoteModeStatus::NeedsAction,
            detail: format!(
                "no runtime token set ({}); the runtime would refuse authenticated calls",
                RUNTIME_TOKEN_VARS[0]
            ),
        },
    };

    // The shipped systemd unit binds loopback. Without an explicit host
    // override there is nothing for a phone to reach, and saying "not
    // configured" would imply a switch the user could flip here.
    let mobile_lan = match lan_host {
        None => RemoteModeFact {
            mode: RemoteMode::MobileLan,
            status: RemoteModeStatus::Disabled,
            detail: format!(
                "runtime binds 127.0.0.1 only; set {} to expose it on a LAN",
                RUNTIME_HOST_VARS[0]
            ),
        },
        Some(name) => {
            // A bound LAN address with no token is reachable *and*
            // unauthenticated, so it is the one case that must ask for action.
            if runtime_token.is_some() {
                RemoteModeFact {
                    mode: RemoteMode::MobileLan,
                    status: RemoteModeStatus::Ready,
                    detail: format!("{name} is set and the runtime token is present"),
                }
            } else {
                RemoteModeFact {
                    mode: RemoteMode::MobileLan,
                    status: RemoteModeStatus::NeedsAction,
                    detail: format!(
                        "{name} is set but no runtime token is; add {} before exposing a LAN port",
                        RUNTIME_TOKEN_VARS[0]
                    ),
                }
            }
        }
    };

    let ready_bridges = registry::BRIDGES
        .iter()
        .filter(|bridge| {
            bridge
                .secret_keys
                .iter()
                .all(|key| any_var_present(&[key], lookup))
        })
        .map(|bridge| bridge.slug)
        .collect::<Vec<_>>();
    let chat_bridge = if ready_bridges.is_empty() {
        RemoteModeFact {
            mode: RemoteMode::ChatBridge,
            status: RemoteModeStatus::NeedsAction,
            detail: format!(
                "{} bridges available ({}); none has its credentials set yet",
                registry::BRIDGES.len(),
                registry::BRIDGES
                    .iter()
                    .map(|bridge| bridge.slug)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    } else {
        RemoteModeFact {
            mode: RemoteMode::ChatBridge,
            status: RemoteModeStatus::Ready,
            detail: format!("credentials present for: {}", ready_bridges.join(", ")),
        }
    };

    vec![
        RemoteModeFact {
            mode: RemoteMode::LocalOnly,
            status: RemoteModeStatus::Ready,
            detail: "this machine only; nothing is exposed and nothing to configure".to_string(),
        },
        runtime_api,
        mobile_lan,
        chat_bridge,
    ]
}

/// Render the plan preview **in memory**, with every secret replaced by
/// [`REDACTED`].
///
/// This deliberately goes through the same `remote_setup::bundle` planning
/// contract the CLI uses, so the preview cannot drift from what
/// `codewhale remote-setup --generate-only` would produce. It calls
/// [`bundle::render_bundle`] (a pure function) and never
/// [`bundle::write_bundle`]: no file is created, no command is run, and no
/// cloud resource is provisioned.
pub(super) fn redacted_plan_preview(command_provider: &str) -> String {
    let Some(cloud) = registry::cloud_by_slug("lighthouse") else {
        return "No cloud target is registered; nothing to preview.".to_string();
    };
    let Some(bridge) = registry::bridge_by_slug("telegram") else {
        return "No chat bridge is registered; nothing to preview.".to_string();
    };
    let Some(provider) = bundle::ProviderInfo::from_slug(command_provider) else {
        return format!(
            "Route \"{command_provider}\" is not representable as a remote bundle provider yet, \
so no plan can be generated for it."
        );
    };

    let inputs = bundle::BundleInputs {
        cloud,
        bridge,
        provider,
        model: "auto".to_string(),
        // Every secret slot is a constant placeholder. No token is generated,
        // no environment value is read, and nothing here is usable.
        runtime_token: REDACTED.to_string(),
        provider_key_value: REDACTED.to_string(),
        bridge_secret_values: bridge
            .secret_keys
            .iter()
            .map(|key| ((*key).to_string(), REDACTED.to_string()))
            .collect(),
        allowlist: String::new(),
        port: bundle::DEFAULT_PORT,
        workers: bundle::DEFAULT_WORKERS,
        workspace: "<your workspace>".to_string(),
    };

    let mut out = String::from(
        "Preview only. Nothing below has been written, applied, or provisioned.\n\
Every secret is shown as <redacted>; Codewhale never reads their values here.\n\n",
    );
    for file in bundle::render_bundle(&inputs) {
        out.push_str(&format!("── {} ──\n", file.relative_path));
        out.push_str(&file.contents);
        if !file.contents.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

pub(super) fn on_ramp_text(
    locale: Locale,
    clouds_result: &str,
    bridges_result: &str,
    providers_result: &str,
    mode_result: &str,
    command_provider: &str,
) -> String {
    let command = format!(
        "codewhale remote-setup --generate-only --cloud lighthouse --bridge telegram --provider {command_provider} --out ./codewhale-deploy/lighthouse-telegram"
    );
    let base = tr(locale, MessageId::SetupRemoteOnRampText);
    let mut out = base
        .replace("{clouds_result}", clouds_result)
        .replace("{bridges_result}", bridges_result)
        .replace("{providers_result}", providers_result)
        .replace("{mode_result}", mode_result)
        .replace("{command}", &command);
    out.push_str("\n\n");
    out.push_str(&redacted_plan_preview(command_provider));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<HashMap<_, _>>();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn local_only_is_always_ready_and_needs_no_configuration() {
        let modes = observe_modes(&env(&[]));
        let local = modes
            .iter()
            .find(|fact| fact.mode == RemoteMode::LocalOnly)
            .expect("local-only mode");
        assert_eq!(local.status, RemoteModeStatus::Ready);
        assert!(local.detail.contains("nothing to configure"));
    }

    #[test]
    fn a_bare_install_reports_the_honest_mode_matrix() {
        let modes = observe_modes(&env(&[]));
        let by_mode = |mode| {
            modes
                .iter()
                .find(|fact| fact.mode == mode)
                .expect("mode")
                .status
        };

        assert_eq!(by_mode(RemoteMode::LocalOnly), RemoteModeStatus::Ready);
        // Missing token/config is NeedsAction, never Failed and never blocking.
        assert_eq!(
            by_mode(RemoteMode::RuntimeApi),
            RemoteModeStatus::NeedsAction
        );
        // Nothing the user does on this screen can expose a loopback bind.
        assert_eq!(by_mode(RemoteMode::MobileLan), RemoteModeStatus::Disabled);
        assert_eq!(
            by_mode(RemoteMode::ChatBridge),
            RemoteModeStatus::NeedsAction
        );
    }

    #[test]
    fn a_lan_bind_without_a_token_asks_for_action_rather_than_claiming_ready() {
        let modes = observe_modes(&env(&[("CODEWHALE_RUNTIME_HOST", "0.0.0.0")]));
        let lan = modes
            .iter()
            .find(|fact| fact.mode == RemoteMode::MobileLan)
            .expect("mobile mode");
        assert_eq!(lan.status, RemoteModeStatus::NeedsAction);

        let both = observe_modes(&env(&[
            ("CODEWHALE_RUNTIME_HOST", "0.0.0.0"),
            ("CODEWHALE_RUNTIME_TOKEN", "t-secret-value"),
        ]));
        assert_eq!(
            both.iter()
                .find(|fact| fact.mode == RemoteMode::MobileLan)
                .expect("mobile mode")
                .status,
            RemoteModeStatus::Ready
        );
    }

    #[test]
    fn bridge_credentials_flip_the_bridge_mode_to_ready() {
        let modes = observe_modes(&env(&[("TELEGRAM_BOT_TOKEN", "12345:AAhostile")]));
        let bridge = modes
            .iter()
            .find(|fact| fact.mode == RemoteMode::ChatBridge)
            .expect("bridge mode");
        assert_eq!(bridge.status, RemoteModeStatus::Ready);
        assert!(bridge.detail.contains("telegram"));
    }

    /// The single most important property of this step: it inspects
    /// credentials without ever surfacing one.
    #[test]
    fn hostile_secret_values_never_reach_any_rendered_fact() {
        let hostile_token = "t-\u{202e}AKIAIOSFODNN7EXAMPLE/../../etc/passwd";
        let hostile_bot = "999:AAsuper-secret-bot-token";
        let modes = observe_modes(&env(&[
            ("CODEWHALE_RUNTIME_TOKEN", hostile_token),
            ("CODEWHALE_RUNTIME_HOST", "10.0.0.5"),
            ("TELEGRAM_BOT_TOKEN", hostile_bot),
            ("FEISHU_APP_ID", "cli_hostile"),
            ("FEISHU_APP_SECRET", "shh"),
        ]));

        let rendered = modes
            .iter()
            .map(|fact| fact.detail.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for secret in [hostile_token, hostile_bot, "cli_hostile", "shh", "AKIA"] {
            assert!(
                !rendered.contains(secret),
                "secret value leaked into the step facts: {rendered}"
            );
        }
        // Names are fine and are what makes the status explainable.
        assert!(rendered.contains("CODEWHALE_RUNTIME_TOKEN"));
        // A LAN address is host configuration the user typed, not a secret,
        // but it is still not echoed back.
        assert!(!rendered.contains("10.0.0.5"));
    }

    #[test]
    fn plan_preview_is_generated_in_memory_and_fully_redacted() {
        let preview = redacted_plan_preview("deepseek");

        assert!(preview.contains("Nothing below has been written"));
        assert!(preview.contains("RUNBOOK.md"));
        assert!(preview.contains("runtime.env"));
        assert!(
            preview.contains(REDACTED),
            "secrets must render as the redaction placeholder"
        );
        // The generated token slot must be the placeholder, not a real token.
        assert!(preview.contains(&format!("CODEWHALE_RUNTIME_TOKEN={REDACTED}")));
    }

    #[test]
    fn plan_preview_writes_nothing_to_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let before = std::fs::read_dir(tmp.path()).expect("read tempdir").count();

        let _ = redacted_plan_preview("deepseek");

        let after = std::fs::read_dir(tmp.path()).expect("read tempdir").count();
        assert_eq!(before, after, "the preview must not create any file");
        assert!(!std::path::Path::new("./codewhale-deploy").exists());
    }

    #[test]
    fn an_unrepresentable_route_says_so_instead_of_substituting_a_provider() {
        let preview = redacted_plan_preview("my-private-gateway");
        assert!(preview.contains("not representable"));
        assert!(
            !preview.contains("DEEPSEEK_API_KEY"),
            "must not silently substitute another provider's credential contract"
        );
    }

    /// A user who wants none of this must be able to leave in one key: the
    /// default mode is local-only and local-only is always usable.
    #[test]
    fn the_default_mode_is_local_only_and_is_never_blocking() {
        let facts = SetupRemoteFacts {
            modes: observe_modes(&env(&[])),
            clouds_result: String::new(),
            bridges_result: String::new(),
            providers_result: String::new(),
            mode_result: String::new(),
            command_provider: "deepseek".to_string(),
            result: String::new(),
        };

        assert_eq!(facts.default_mode(), RemoteMode::LocalOnly);
        assert_eq!(
            facts.status_for(RemoteMode::LocalOnly),
            RemoteModeStatus::Ready
        );
        // Missing tokens/config are surfaced, but only ever as NeedsAction.
        assert!(facts.needs_action());
        assert!(
            facts
                .modes
                .iter()
                .all(|fact| fact.status != RemoteModeStatus::Disabled
                    || fact.mode == RemoteMode::MobileLan)
        );
    }

    #[test]
    fn mode_rows_render_in_every_complete_pack_without_placeholders() {
        for locale in Locale::shipped_complete() {
            for mode in RemoteMode::ALL {
                let label = tr(*locale, mode.label_id());
                assert!(!label.is_empty(), "{locale:?} {mode:?}");
                assert!(!label.contains('{'), "{locale:?} {mode:?}: {label}");
            }
            for status in [
                RemoteModeStatus::Disabled,
                RemoteModeStatus::Ready,
                RemoteModeStatus::NeedsAction,
            ] {
                let label = tr(*locale, status.label_id());
                assert!(!label.is_empty(), "{locale:?} {status:?}");
            }
        }
    }
}
