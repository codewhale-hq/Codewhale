//! Startup facts passed by the canonical CLI to the Engine library.
//!
//! One Clap definition serves the CLI and the library's command decoder. Paths
//! remain native paths; no stringified argv round trip owns startup settings.

use anyhow::{Result, bail};
use clap::Args;
use codewhale_execpolicy::ApprovalMode;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Ask,
    AcceptEdits,
    Plan,
    AutoReview,
    FullAccess,
}

impl PermissionMode {
    #[must_use]
    pub fn approval_mode(self) -> Option<ApprovalMode> {
        match self {
            Self::Ask => Some(ApprovalMode::Suggest),
            Self::AcceptEdits => Some(ApprovalMode::AcceptEdits),
            Self::Plan => None,
            Self::AutoReview => Some(ApprovalMode::Auto),
            Self::FullAccess => Some(ApprovalMode::Bypass),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AcceptEdits => "accept-edits",
            Self::Plan => "plan",
            Self::AutoReview => "auto-review",
            Self::FullAccess => "full-access",
        }
    }
}

pub fn parse_permission_mode(raw: &str) -> std::result::Result<PermissionMode, String> {
    if raw.trim().eq_ignore_ascii_case("plan") {
        return Ok(PermissionMode::Plan);
    }
    match ApprovalMode::from_config_value(raw) {
        Some(ApprovalMode::Suggest) => Ok(PermissionMode::Ask),
        Some(ApprovalMode::AcceptEdits) => Ok(PermissionMode::AcceptEdits),
        Some(ApprovalMode::Auto) => Ok(PermissionMode::AutoReview),
        Some(ApprovalMode::Bypass) => Ok(PermissionMode::FullAccess),
        Some(ApprovalMode::Never) | None => Err(format!(
            "invalid permission mode '{raw}'; expected ask, accept-edits, plan, auto-review, or full-access"
        )),
    }
}

#[derive(Args, Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// Path to the config file to load instead of the default.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Config profile to apply (a `[profiles.<name>]` table).
    #[arg(long)]
    pub profile: Option<String>,
    /// Workspace directory for Codewhale file tools (`-w` is a legacy alias).
    #[arg(
        short = 'C',
        short_alias = 'w',
        long = "workspace",
        alias = "cd",
        value_name = "DIR"
    )]
    pub workspace: Option<PathBuf>,
    /// Enable terminal mouse capture for scrolling and transcript selection.
    #[arg(long = "mouse-capture", conflicts_with = "no_mouse_capture")]
    pub mouse_capture: bool,
    /// Disable terminal mouse capture for terminal-native text selection.
    #[arg(long = "no-mouse-capture", conflicts_with = "mouse_capture")]
    pub no_mouse_capture: bool,
    /// Skip onboarding screens.
    #[arg(long)]
    pub skip_onboarding: bool,
    /// Start a fresh session without automatic resume or crash recovery.
    #[arg(long)]
    pub fresh: bool,
    /// Skip project config and workspace overlays for this run.
    #[arg(long)]
    pub no_project_config: bool,
    /// Enable a feature for this run (repeatable).
    #[arg(long = "enable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    pub enable: Vec<String>,
    /// Disable a feature for this run (repeatable).
    #[arg(long = "disable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    pub disable: Vec<String>,
    /// Run with Full Access: no approval prompts and no Codewhale sandbox
    /// (the same as `yolo = true`; `--yolo` is an alias).
    #[arg(long = "full-access", alias = "yolo")]
    pub yolo: bool,
    #[arg(
        long = "permission-mode",
        value_name = "MODE",
        value_parser = parse_permission_mode,
        global = true,
        help = "Start in this permission mode: ask, accept-edits, plan, auto-review, or full-access."
    )]
    pub permission_mode: Option<PermissionMode>,
    /// Maximum number of concurrent sub-agents (1-128; default 64).
    #[arg(long)]
    pub max_subagents: Option<usize>,
    /// Enable verbose logging.
    #[arg(short, long)]
    pub verbose: bool,
    /// Start account-owned web remote control for this interactive session.
    #[arg(long, hide = true)]
    pub remote_control: bool,
    #[arg(skip)]
    pub control_frontend: Option<codewhale_runtime::runtime_control::RuntimeControlFrontend>,
}

impl RuntimeOptions {
    // A `run` passthrough can still carry startup flags. Preserve the old
    // duplicate/conflict refusal rather than silently picking an authority.
    pub fn merge(mut self, command: Self) -> Result<Self> {
        macro_rules! scalar {
            ($field:ident, $flag:literal) => {
                if self.$field.is_some() && command.$field.is_some() {
                    bail!(
                        "{} cannot be supplied both before and inside the command",
                        $flag
                    );
                }
                self.$field = self.$field.or(command.$field);
            };
        }
        macro_rules! flag {
            ($field:ident, $name:literal) => {
                if self.$field && command.$field {
                    bail!(
                        "{} cannot be supplied both before and inside the command",
                        $name
                    );
                }
                self.$field |= command.$field;
            };
        }
        scalar!(config, "--config");
        scalar!(profile, "--profile");
        scalar!(workspace, "--workspace");
        scalar!(max_subagents, "--max-subagents");
        scalar!(permission_mode, "--permission-mode");
        scalar!(control_frontend, "selected control frontend");
        flag!(mouse_capture, "--mouse-capture");
        flag!(no_mouse_capture, "--no-mouse-capture");
        flag!(skip_onboarding, "--skip-onboarding");
        flag!(fresh, "--fresh");
        flag!(no_project_config, "--no-project-config");
        flag!(yolo, "--full-access");
        flag!(verbose, "--verbose");
        flag!(remote_control, "--remote-control");
        if self.mouse_capture && self.no_mouse_capture {
            bail!("--mouse-capture conflicts with --no-mouse-capture");
        }
        if self.permission_mode.is_some() && self.yolo {
            bail!("--permission-mode cannot be combined with --full-access or --yolo");
        }
        self.enable.extend(command.enable);
        self.disable.extend(command.disable);
        Ok(self)
    }

    pub fn apply_features(&self, config: &mut codewhale_runtime::config::Config) -> Result<()> {
        for feature in &self.enable {
            config.set_feature(feature, true)?;
        }
        for feature in &self.disable {
            config.set_feature(feature, false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_startup_rejects_competing_config_and_opposing_mouse_facts() {
        let root = RuntimeOptions {
            config: Some("root.toml".into()),
            ..Default::default()
        };
        let command = RuntimeOptions {
            config: Some("command.toml".into()),
            ..Default::default()
        };
        assert!(
            root.merge(command)
                .unwrap_err()
                .to_string()
                .contains("--config")
        );
        let root = RuntimeOptions {
            mouse_capture: true,
            ..Default::default()
        };
        let command = RuntimeOptions {
            no_mouse_capture: true,
            ..Default::default()
        };
        assert!(
            root.merge(command)
                .unwrap_err()
                .to_string()
                .contains("conflicts")
        );
    }

    #[test]
    fn command_scope_facts_and_feature_disables_preserve_the_config_floor() {
        let root = RuntimeOptions {
            enable: vec!["web_search".into()],
            ..Default::default()
        };
        let command = RuntimeOptions {
            workspace: Some("command workspace".into()),
            disable: vec!["web_search".into()],
            no_project_config: true,
            ..Default::default()
        };
        let merged = root.merge(command).unwrap();
        assert_eq!(merged.workspace, Some(PathBuf::from("command workspace")));
        assert!(merged.no_project_config);
        let mut config = codewhale_runtime::config::Config::default();
        merged.apply_features(&mut config).unwrap();
        assert!(
            !config
                .features()
                .enabled(codewhale_runtime::features::Feature::WebSearch)
        );
    }

    #[test]
    fn permission_mode_parses_all_five_modes_and_rejects_unknown_or_never() {
        assert_eq!(parse_permission_mode("ask"), Ok(PermissionMode::Ask));
        assert_eq!(
            parse_permission_mode("accept-edits"),
            Ok(PermissionMode::AcceptEdits)
        );
        assert_eq!(parse_permission_mode("plan"), Ok(PermissionMode::Plan));
        assert_eq!(
            parse_permission_mode("auto-review"),
            Ok(PermissionMode::AutoReview)
        );
        assert_eq!(
            parse_permission_mode("full-access"),
            Ok(PermissionMode::FullAccess)
        );
        assert!(parse_permission_mode("never").is_err());
        assert!(parse_permission_mode("bogus").is_err());
    }
}
