//! Session-page MCP + plugin boot surface.
//!
//! Plugin discovery and every enabled MCP server boot as a **set**, not a
//! toast per name. The Tideline footer carries the compact pulse
//! (`MCP · 4 connecting` or `Plugins · Problems: 2 · /plugins`); detailed
//! diagnosis and actions belong in `/mcp` or `/plugins`, never as multi-row
//! boot output between the transcript and composer.
//!
//! One exception, and it is a place rather than a second source: the launch
//! screen has the vertical room the footer does not, so
//! [`crate::tui::underwater::launch_empty_state`] projects the *same*
//! [`SessionBootSurface`] into a block under the recent-work list that names
//! the servers that failed or need a login. Nothing here computes MCP state
//! twice; that renderer reads [`SessionBootSurface::servers`].

use unicode_width::UnicodeWidthStr;

use crate::tui::app::App;
use codewhale_engine::mcp::{McpManagerSnapshot, McpServerSnapshot};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::types::{PluginDiagnosticLevel, PluginTrustStatus};

pub(crate) const ITEM_SEPARATOR: &str = " · ";
const MAX_NAMED_CHIPS: usize = 4;

/// How long a boot *prediction* may stand in for the boot pass's receipt (#7035).
///
/// Configured rows paint as connecting because the engine's pass is expected to
/// name them. That is a guess about work in flight, so it needs a horizon: the
/// pass connects eight servers at a time with a 30 s default connect timeout,
/// which is ten waves — far more than any real workspace — plus room for a
/// first-run `npx`/`uvx` package fetch. Past this, no receipt has arrived, and
/// the honest surface is the last known snapshot rather than an eternal
/// "connecting".
pub const MCP_BOOT_PREDICTION_HORIZON: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBootPhase {
    Hidden,
    Booting,
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerBootState {
    Connecting,
    Connected,
    Failed,
    NeedsLogin,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerBootRow {
    pub name: String,
    pub state: McpServerBootState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PluginBootSummary {
    pub loaded: usize,
    pub invalid: usize,
    pub duplicate: usize,
    pub needs_setup: usize,
}

impl PluginBootSummary {
    #[must_use]
    pub fn is_quiet(self) -> bool {
        self.problem_count() == 0
    }

    #[must_use]
    pub fn problem_count(self) -> usize {
        self.invalid + self.duplicate + self.needs_setup
    }

    #[must_use]
    pub fn from_registry(registry: &PluginRegistry) -> Self {
        let loaded = registry.list().len();
        let mut invalid = 0usize;
        let mut duplicate = 0usize;
        let mut needs_setup = 0usize;
        for diagnostic in registry.diagnostics() {
            match diagnostic.code {
                "duplicate-root" | "name-conflict" => duplicate += 1,
                _ if diagnostic.level == PluginDiagnosticLevel::Error => invalid += 1,
                _ => {}
            }
        }
        for plugin in registry.list() {
            if plugin
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == PluginDiagnosticLevel::Error)
            {
                invalid += 1;
            } else if plugin.enabled && plugin_trust_needs_setup(plugin.trust_status) {
                // A bundle that is enabled but no longer trusted — its content
                // or capabilities moved since the review, or it was switched on
                // without one — is the problem this chip exists for. A disabled,
                // never-reviewed bundle is a shipped default resting where it
                // was shipped (every built-in starts that way), not a problem:
                // counting it put "Plugins · Problems" in the footer of every
                // fresh install until each built-in had been reviewed, whether
                // or not anyone meant to enable it. Extensions still lists it
                // with its review action.
                needs_setup += 1;
            }
        }
        Self {
            loaded,
            invalid,
            duplicate,
            needs_setup,
        }
    }
}

/// Warm render-path caches (syntax highlighting) off the UI thread, once per
/// process. Only the live TUI reads [`SessionBootSurface::from_app`], so
/// headless and one-shot paths never pay for the load.
fn spawn_render_prewarm_once() {
    static PREWARM: std::sync::Once = std::sync::Once::new();
    PREWARM.call_once(|| {
        // Best effort: if the thread cannot start, the first code block
        // loads the sets lazily exactly as before.
        let _ = std::thread::Builder::new()
            .name("cw-syntax-prewarm".to_string())
            .spawn(codewhale_tui_kit::markdown_render::prewarm_syntax_highlighting);
    });
}

fn plugin_trust_needs_setup(status: PluginTrustStatus) -> bool {
    matches!(
        status,
        PluginTrustStatus::NeverReviewed
            | PluginTrustStatus::ContentChanged
            | PluginTrustStatus::CapabilitiesChanged
    )
}

/// Semantic severity for the compact boot activity notice.
///
/// The text carries no color names or inferred state. Its consumer maps this
/// closed state into the Tideline palette, so a plugin warning cannot inherit
/// an unrelated MCP color merely because both use the same footer slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBootActivityLevel {
    Active,
    Attention,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBootActivityChip {
    pub text: String,
    pub level: SessionBootActivityLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBootSurface {
    pub phase: SessionBootPhase,
    pub servers: Vec<McpServerBootRow>,
    pub plugins: PluginBootSummary,
    /// Enabled-server count used when names have not arrived yet, so the
    /// first frame can still say `MCP · N connecting` instead of hiding.
    unnamed_connecting: usize,
}

impl SessionBootSurface {
    #[must_use]
    pub fn from_app(app: &App) -> Self {
        spawn_render_prewarm_once();
        Self::from_parts(
            app.mcp_snapshot.as_ref(),
            app.mcp_boot_initializing(),
            &app.mcp_connecting,
            app.mcp_configured_count,
            PluginBootSummary::from_registry(app.plugin_registry.as_ref()),
        )
    }

    #[must_use]
    pub fn from_parts(
        snapshot: Option<&McpManagerSnapshot>,
        initializing: bool,
        connecting: &[String],
        configured_count: usize,
        plugins: PluginBootSummary,
    ) -> Self {
        let servers = if let Some(snapshot) = snapshot {
            snapshot
                .servers
                .iter()
                .filter_map(|server| row_from_snapshot(server, connecting))
                .collect()
        } else if initializing {
            let mut names = connecting.to_vec();
            names.sort();
            names
                .into_iter()
                .map(|name| McpServerBootRow {
                    name,
                    state: McpServerBootState::Connecting,
                })
                .collect()
        } else {
            Vec::new()
        };

        let connecting_count = servers
            .iter()
            .filter(|row| row.state == McpServerBootState::Connecting)
            .count();
        // The pre-event gap is the only moment the in-flight names are
        // genuinely unknown: once `connecting` arrives it is the engine's
        // real in-flight set (#6033), and an empty set under lazy boot means
        // nothing is connecting — not "names have not arrived yet".
        let unnamed_connecting =
            if connecting_count == 0 && initializing && connecting.is_empty() && snapshot.is_none()
            {
                configured_count
            } else {
                0
            };
        let phase = if servers.is_empty() && plugins.is_quiet() && unnamed_connecting == 0 {
            SessionBootPhase::Hidden
        } else if initializing || connecting_count > 0 || unnamed_connecting > 0 {
            SessionBootPhase::Booting
        } else {
            SessionBootPhase::Settled
        };

        Self {
            phase,
            servers,
            plugins,
            unnamed_connecting,
        }
    }

    /// Enabled servers known to be connecting before any name has arrived.
    ///
    /// The first launch frame can have `mcp_initializing` set with an empty
    /// name list; without this a 23-server workspace would paint nothing at
    /// all until the first boot event lands.
    #[must_use]
    pub fn connecting_without_names(&self) -> usize {
        self.unnamed_connecting
    }

    #[must_use]
    pub fn activity_notice(
        &self,
        locale: Locale,
        budget: usize,
    ) -> Option<SessionBootActivityChip> {
        if self.phase == SessionBootPhase::Hidden || budget == 0 {
            return None;
        }
        let connecting: Vec<&str> = self
            .servers
            .iter()
            .filter(|row| row.state == McpServerBootState::Connecting)
            .map(|row| row.name.as_str())
            .collect();
        // A server whose login expired is not a failure: the engine already
        // knows the remedy (`codewhale mcp login <server>` / the login tool),
        // so the chip counts it under the shared auth-required label and only
        // calls the rest failed (#5926).
        let need_login = self
            .servers
            .iter()
            .filter(|row| row.state == McpServerBootState::NeedsLogin)
            .count();
        let failed = self
            .servers
            .iter()
            .filter(|row| row.state == McpServerBootState::Failed)
            .count();
        let connected = self
            .servers
            .iter()
            .filter(|row| row.state == McpServerBootState::Connected)
            .count();

        if !connecting.is_empty() {
            let count = connecting.len();
            let named = named_chip_line("MCP", count, "connecting", &connecting);
            return activity_notice_from_candidates(
                SessionBootActivityLevel::Active,
                vec![named, format!("MCP{ITEM_SEPARATOR}{count} connecting")],
                budget,
            );
        }
        if failed > 0 || need_login > 0 {
            let auth_label = mcp_auth_required_state_label();
            let mut full = format!(
                "MCP{ITEM_SEPARATOR}{connected} {}",
                tr(locale, MessageId::ExtensionsStateConnected)
            );
            // The narrow form drops the glyph and shortens the verb so both
            // counts survive an 80-column footer.
            let mut compact = String::from("MCP");
            if need_login > 0 {
                full.push_str(&format!("{ITEM_SEPARATOR}{need_login} {auth_label}"));
                compact.push_str(&format!("{ITEM_SEPARATOR}{need_login} login"));
            }
            if failed > 0 {
                full.push_str(&format!(
                    "{ITEM_SEPARATOR}{failed} {}",
                    tr(locale, MessageId::PhaseFailed)
                ));
                compact.push_str(&format!("{ITEM_SEPARATOR}{failed} failed"));
            }
            let level = if failed > 0 {
                SessionBootActivityLevel::Failure
            } else {
                SessionBootActivityLevel::Attention
            };
            return activity_notice_from_candidates(level, vec![full, compact], budget);
        }
        if self.phase == SessionBootPhase::Booting {
            let count = self.servers.len().max(self.unnamed_connecting);
            if count > 0 {
                return activity_notice_from_candidates(
                    SessionBootActivityLevel::Active,
                    vec![format!("MCP{ITEM_SEPARATOR}{count} connecting")],
                    budget,
                );
            }
        }

        // Plugins contribute no footer chip.
        //
        // "Plugins · Problems: N" sat in the footer of every session for as
        // long as the condition held, and a person could do nothing about it
        // from there. Worse, its number and the Plugins tab disagreed by
        // construction: the chip counted enabled-but-unreviewed bundles,
        // while the tab's Problems group lists only registry-level
        // diagnostics, so the rows that made the count were somewhere else
        // entirely — "it says 2 problems ... it's not even clear what the
        // problems are because there are actually 3".
        //
        // A bundle waiting on a review is not an incident; it is a row with
        // an action. `/plugins` states it per bundle, next to the key that
        // resolves it. `self.plugins` is still computed: it decides whether
        // this surface is `Hidden`, and the launch block reads the same
        // summary.
        None
    }
}

fn activity_notice_from_candidates(
    level: SessionBootActivityLevel,
    candidates: Vec<String>,
    budget: usize,
) -> Option<SessionBootActivityChip> {
    candidates
        .into_iter()
        .find(|line| line.width() <= budget)
        .map(|text| SessionBootActivityChip { text, level })
}

fn row_from_snapshot(
    server: &McpServerSnapshot,
    connecting: &[String],
) -> Option<McpServerBootRow> {
    if !server.enabled {
        return Some(McpServerBootRow {
            name: server.name.clone(),
            state: McpServerBootState::Disabled,
        });
    }
    if server.connected {
        return Some(McpServerBootRow {
            name: server.name.clone(),
            state: McpServerBootState::Connected,
        });
    }
    if let Some(error) = server.error.as_deref() {
        let state = if server.auth_required || mcp_error_requires_login(error) {
            McpServerBootState::NeedsLogin
        } else {
            McpServerBootState::Failed
        };
        return Some(McpServerBootRow {
            name: server.name.clone(),
            state,
        });
    }
    if connecting.iter().any(|name| name == &server.name) {
        return Some(McpServerBootRow {
            name: server.name.clone(),
            state: McpServerBootState::Connecting,
        });
    }
    // Enabled, unconnected, no diagnosis, not in flight: a lazy server
    // nobody has asked for yet (#6033). It is not boot activity, so it gets
    // no row — calling it Failed or Connecting would both be lies.
    None
}

/// Text fallback for the typed [`McpServerSnapshot::auth_required`] state:
/// the one shared auth-required classifier, plus the shape the session-boot
/// receipt itself prints.
#[must_use]
pub fn mcp_error_requires_login(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    codewhale_engine::mcp::oauth::error_text_looks_auth_required(error)
        || (lowered.contains("oauth") && lowered.contains("authenticat"))
}

/// The `◆ auth required` state label every TUI MCP surface prints for a
/// server whose login is missing, expired, or revoked.
#[must_use]
pub fn mcp_auth_required_state_label() -> String {
    format!("{} auth required", codewhale_tui_kit::glyphs::ATTENTION)
}

fn named_chip_line(kind: &str, count: usize, verb: &str, names: &[&str]) -> String {
    let chips = names
        .iter()
        .take(MAX_NAMED_CHIPS)
        .copied()
        .collect::<Vec<_>>();
    let extra = names.len().saturating_sub(chips.len());
    let mut line = format!("{kind}{ITEM_SEPARATOR}{count} {verb}");
    if !chips.is_empty() {
        line.push_str(ITEM_SEPARATOR);
        line.push_str(&chips.join(ITEM_SEPARATOR));
        if extra > 0 {
            line.push_str(&format!("{ITEM_SEPARATOR}+{extra}"));
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_engine::mcp::{
        McpManagerSnapshot, McpServerCapabilityMetadata, McpServerSnapshot,
    };
    use std::path::PathBuf;

    fn server(
        name: &str,
        enabled: bool,
        connected: bool,
        error: Option<&str>,
    ) -> McpServerSnapshot {
        McpServerSnapshot {
            name: name.to_string(),
            enabled,
            required: false,
            transport: "stdio".to_string(),
            command_or_url: format!("cmd-{name}"),
            connect_timeout: 5,
            execute_timeout: 5,
            read_timeout: 5,
            connected,
            error: error.map(str::to_string),
            auth_required: false,
            capability_metadata: McpServerCapabilityMetadata::NotObserved,
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
        }
    }

    fn snapshot(servers: Vec<McpServerSnapshot>) -> McpManagerSnapshot {
        McpManagerSnapshot {
            config_path: PathBuf::from("mcp.json"),
            config_exists: true,
            reload_required: false,
            servers,
        }
    }

    #[test]
    fn zero_servers_and_quiet_plugins_hide() {
        let surface =
            SessionBootSurface::from_parts(None, false, &[], 0, PluginBootSummary::default());
        assert_eq!(surface.phase, SessionBootPhase::Hidden);
        assert!(
            surface
                .activity_notice(Locale::En, 80)
                .map(|notice| notice.text)
                .is_none()
        );
    }

    #[test]
    fn healthy_loaded_plugins_do_not_claim_the_boot_surface() {
        let surface = SessionBootSurface::from_parts(
            None,
            false,
            &[],
            0,
            PluginBootSummary {
                loaded: 2,
                ..PluginBootSummary::default()
            },
        );
        assert_eq!(surface.phase, SessionBootPhase::Hidden);
        assert!(surface.activity_notice(Locale::En, 80).is_none());
    }

    #[test]
    fn changed_plugin_content_requires_setup() {
        assert!(plugin_trust_needs_setup(PluginTrustStatus::ContentChanged));
        assert!(plugin_trust_needs_setup(PluginTrustStatus::NeverReviewed));
        assert!(plugin_trust_needs_setup(
            PluginTrustStatus::CapabilitiesChanged
        ));
        assert!(!plugin_trust_needs_setup(PluginTrustStatus::Trusted));
    }

    /// A fresh install ships every built-in bundle disabled and never
    /// reviewed. That is the shipped resting state, not a problem: counting
    /// it kept "Plugins · Problems" in the footer of every new install until
    /// each built-in had been reviewed.
    #[test]
    fn fresh_install_built_ins_keep_the_plugin_chip_quiet() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

        let registry = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv()
            .registry_for_workspace(&workspace);
        let summary = PluginBootSummary::from_registry(&registry);
        assert_eq!(summary.loaded, registry.list().len());
        assert!(
            summary.is_quiet(),
            "a fresh install must not report plugin problems: {summary:?}"
        );
    }

    /// Plugin state never reaches the footer, at any width and at any
    /// severity. The chip it replaced was permanent, unactionable, and
    /// counted a different set than the tab it pointed at.
    #[test]
    fn plugin_problems_never_produce_a_footer_chip() {
        for summary in [
            PluginBootSummary {
                loaded: 3,
                invalid: 1,
                duplicate: 1,
                needs_setup: 1,
            },
            PluginBootSummary {
                loaded: 1,
                needs_setup: 1,
                ..PluginBootSummary::default()
            },
        ] {
            let surface = SessionBootSurface::from_parts(None, false, &[], 0, summary);
            assert_eq!(surface.phase, SessionBootPhase::Settled);
            for budget in [12, 22, 40, 80] {
                assert_eq!(
                    surface.activity_notice(Locale::En, budget),
                    None,
                    "plugin summary {summary:?} produced a chip at width {budget}"
                );
            }
        }
    }

    #[test]
    fn mcp_activity_outranks_plugin_problems() {
        let snap = snapshot(vec![server("alpha", true, false, None)]);
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            true,
            &["alpha".to_string()],
            1,
            PluginBootSummary {
                invalid: 1,
                ..PluginBootSummary::default()
            },
        );
        assert_eq!(
            surface.activity_notice(Locale::En, 80),
            Some(SessionBootActivityChip {
                text: "MCP · 1 connecting · alpha".to_string(),
                level: SessionBootActivityLevel::Active,
            })
        );
    }

    #[test]
    fn one_connecting_server_names_itself() {
        let snap = snapshot(vec![server("alpha", true, false, None)]);
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            true,
            &["alpha".to_string()],
            1,
            PluginBootSummary::default(),
        );
        assert_eq!(surface.phase, SessionBootPhase::Booting);
        assert_eq!(surface.servers.len(), 1);
        assert_eq!(surface.servers[0].state, McpServerBootState::Connecting);
        let chip = surface
            .activity_notice(Locale::En, 80)
            .map(|notice| notice.text)
            .expect("chip");
        assert!(chip.contains("alpha"), "{chip}");
        assert!(!chip.to_ascii_lowercase().contains("slack"), "{chip}");
    }

    #[test]
    fn many_connecting_servers_use_count_and_named_chips() {
        let snap = snapshot(vec![
            server("alpha", true, false, None),
            server("beta", true, false, None),
            server("gamma", true, false, None),
            server("docs", true, false, None),
        ]);
        let connecting = ["alpha", "beta", "gamma", "docs"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            true,
            &connecting,
            4,
            PluginBootSummary::default(),
        );
        assert_eq!(surface.phase, SessionBootPhase::Booting);
        let chip = surface
            .activity_notice(Locale::En, 80)
            .map(|notice| notice.text)
            .expect("chip");
        assert!(chip.contains("4 connecting"), "{chip}");
        assert!(chip.contains("alpha"), "{chip}");
        assert!(chip.contains("docs"), "{chip}");
        assert!(!chip.to_ascii_lowercase().contains("slack"), "{chip}");
    }

    #[test]
    fn settled_failures_remain_classified_for_the_footer_chip() {
        let snap = snapshot(vec![
            server("alpha", true, true, None),
            server("beta", true, false, Some("protocol negotiation timed out")),
            server(
                "gamma",
                true,
                false,
                Some("MCP server 'gamma' requires OAuth authentication. Run `/mcp login gamma`"),
            ),
            server("docs", false, false, Some("disabled")),
        ]);
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            false,
            &[],
            4,
            PluginBootSummary::default(),
        );
        assert_eq!(surface.phase, SessionBootPhase::Settled);
        assert_eq!(
            surface
                .servers
                .iter()
                .find(|row| row.name == "beta")
                .map(|row| row.state),
            Some(McpServerBootState::Failed)
        );
        assert_eq!(
            surface
                .servers
                .iter()
                .find(|row| row.name == "gamma")
                .map(|row| row.state),
            Some(McpServerBootState::NeedsLogin)
        );
    }

    #[test]
    fn typed_auth_required_state_routes_to_login_without_error_text_sniffing() {
        let mut delta = server("delta", true, false, Some("request rejected"));
        delta.auth_required = true;
        let snap = snapshot(vec![delta]);
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            false,
            &[],
            1,
            PluginBootSummary::default(),
        );
        assert_eq!(
            surface
                .servers
                .iter()
                .find(|row| row.name == "delta")
                .map(|row| row.state),
            Some(McpServerBootState::NeedsLogin)
        );
        // The compact activity chip names a needs-login server under the
        // shared auth-required label, at attention level: the remedy is a
        // login, not a repair (#5926).
        assert_eq!(surface.servers.len(), 1);
        let chip = surface
            .activity_notice(Locale::En, 100)
            .expect("needs-login must surface on the boot chip");
        assert_eq!(chip.level, SessionBootActivityLevel::Attention);
        assert_eq!(
            chip.text,
            format!(
                "MCP{ITEM_SEPARATOR}0 connected{ITEM_SEPARATOR}1 {}",
                mcp_auth_required_state_label()
            )
        );
        assert!(!chip.text.contains("failed"), "{}", chip.text);
    }

    #[test]
    fn chip_separates_expired_logins_from_real_failures() {
        let mut expired = server("slack", true, false, Some("401 Unauthorized"));
        expired.auth_required = true;
        let snap = snapshot(vec![
            server("alpha", true, true, None),
            expired,
            server("beta", true, false, Some("Stdio transport closed")),
        ]);
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            false,
            &[],
            3,
            PluginBootSummary::default(),
        );
        let chip = surface
            .activity_notice(Locale::En, 100)
            .expect("mixed states surface on the boot chip");
        assert_eq!(chip.level, SessionBootActivityLevel::Failure);
        assert_eq!(
            chip.text,
            format!(
                "MCP{ITEM_SEPARATOR}1 connected{ITEM_SEPARATOR}1 {}{ITEM_SEPARATOR}1 failed",
                mcp_auth_required_state_label()
            )
        );
        // Under a tight budget the compact form keeps both counts.
        let compact = surface
            .activity_notice(Locale::En, 30)
            .expect("compact chip");
        assert_eq!(
            compact.text,
            format!("MCP{ITEM_SEPARATOR}1 login{ITEM_SEPARATOR}1 failed")
        );
    }

    #[test]
    fn narrow_activity_budget_sheds_names_keeps_count() {
        let snap = snapshot(vec![
            server("alpha", true, false, None),
            server("beta", true, false, None),
            server("gamma", true, false, None),
        ]);
        let connecting = ["alpha", "beta", "gamma"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let surface = SessionBootSurface::from_parts(
            Some(&snap),
            true,
            &connecting,
            3,
            PluginBootSummary::default(),
        );
        let chip = surface
            .activity_notice(Locale::En, 22)
            .map(|notice| notice.text)
            .expect("chip");
        assert_eq!(chip, "MCP · 3 connecting");
    }

    #[test]
    fn first_frame_names_enabled_servers_before_a_snapshot_arrives() {
        let connecting = ["gamma", "alpha", "docs"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let surface = SessionBootSurface::from_parts(
            None,
            true,
            &connecting,
            3,
            PluginBootSummary::default(),
        );
        assert_eq!(surface.phase, SessionBootPhase::Booting);
        assert_eq!(
            surface
                .servers
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "docs", "gamma"]
        );
        let chip = surface
            .activity_notice(Locale::En, 80)
            .map(|notice| notice.text)
            .expect("chip");
        assert!(chip.contains("3 connecting"), "{chip}");
        assert!(chip.contains("alpha"), "{chip}");
        assert!(chip.contains("gamma"), "{chip}");
        assert!(!chip.to_ascii_lowercase().contains("slack"), "{chip}");
    }

    #[test]
    fn initializing_without_names_still_shows_the_count() {
        let surface =
            SessionBootSurface::from_parts(None, true, &[], 4, PluginBootSummary::default());
        assert_eq!(surface.phase, SessionBootPhase::Booting);
        assert!(surface.servers.is_empty());
        assert_eq!(
            surface
                .activity_notice(Locale::En, 80)
                .map(|notice| notice.text)
                .as_deref(),
            Some("MCP · 4 connecting")
        );
    }
}
