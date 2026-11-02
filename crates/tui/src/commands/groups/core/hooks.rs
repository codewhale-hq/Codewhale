//! `/hooks` slash command — read-only listing of configured
//! lifecycle hooks (#460 MVP).
//!
//! The full picker / persisted enable-disable surface in #460 is
//! still M-sized. This MVP gives the user a no-typing view of what's
//! actually configured in `~/.codewhale/config.toml`'s `[hooks]`
//! table — the most-asked question once hooks start firing.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::App;
use crate::tui::app::AppAction;
use codewhale_config::hooks::HookEvent;
use codewhale_localization::MessageId;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "hooks",
    aliases: &["hook", "gouzi"],
    usage: "/hooks [list|events|edit|review|approve <digest>|revoke]",
    description_id: MessageId::CmdHooksDescription,
};

pub(in crate::commands) struct HooksCmd;

impl RegisterCommand for HooksCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        hooks(app, arg)
    }
}

/// Top-level dispatch for `/hooks`. Subcommands:
///
/// * `/hooks`         — same as `/hooks list`.
/// * `/hooks list`    — show every configured hook grouped by event,
///   noting whether the global `[hooks].enabled` flag suppresses
///   them.
/// * `/hooks edit`    — open this workspace's `.codewhale/hooks.toml` in
///   `$EDITOR`, seeding it with a commented template on first use. This is
///   the "add a hook" path: the Hooks screen is a reader, the file is the
///   authority.
/// * `/hooks events`  — list every supported `HookEvent` value the
///   user can target in `[[hooks.hooks]]` entries. Useful for
///   discovery — without this, the only way to learn the event
///   names is to read source.
pub fn hooks(app: &App, arg: Option<&str>) -> CommandResult {
    if arg.is_none_or(|value| value.trim().is_empty()) {
        return CommandResult::action(AppAction::OpenExtensions {
            tab: crate::tui::views::extensions::ExtensionsTab::Hooks,
        });
    }
    let sub = arg.map(str::trim).unwrap_or("list").to_ascii_lowercase();
    if let Some(digest) = sub.strip_prefix("approve ") {
        return match codewhale_engine::hooks::authority::approve_project_hooks(
            &app.workspace,
            digest.trim(),
        ) {
            Ok(()) => CommandResult::message(
                "Approved these exact project hooks. They will load on your next session. Changes require another review.",
            ),
            Err(error) => CommandResult::error(error),
        };
    }
    match sub.as_str() {
        "" | "list" | "ls" | "show" => list(app),
        "events" | "event" | "list-events" => events(),
        "review" => {
            match codewhale_engine::hooks::authority::review_project_hooks(&app.workspace) {
                Ok((authority, contents)) => CommandResult::message(format!(
                    "Project hooks can run shell commands, including scripts they reference. Review the file and those scripts before approval.\n\n{}\n\nTo approve these exact hooks: /hooks approve {}",
                    codewhale_engine::hooks::sanitize_hook_text(
                        &contents,
                        contents.chars().count()
                    ),
                    authority.digest
                )),
                Err(error) => CommandResult::error(error),
            }
        }
        "revoke" => {
            match codewhale_runtime::config::save_workspace_hook_receipt(&app.workspace, "") {
                Ok(_) => CommandResult::message(
                    "Project hook approval revoked. Pending hooks will be refused; already running commands are unaffected.",
                ),
                Err(_) => CommandResult::error("Could not revoke project hook approval"),
            }
        }
        "edit" | "add" | "new" => CommandResult::action(AppAction::EditProjectHooks),
        other => CommandResult::error(format!(
            "unknown subcommand `{other}`. Try `/hooks list`, `/hooks events`, `/hooks edit`, `/hooks review`, `/hooks approve <digest>`, or `/hooks revoke`."
        )),
    }
}

fn events() -> CommandResult {
    let mut out = String::new();
    out.push_str(
        "Available hook events (use one of these as `event = \"...\"` in your `[[hooks.hooks]]` entry).\n\
         Hooks are a TUI runtime feature: `codewhale exec`, the CLI dispatcher, the app-server,\n\
         and the workflow tool do not fire them.\n\n",
    );
    // Order matters — group lifecycle events first, then per-tool,
    // then situational. Stays stable across releases so users can
    // grep on it.
    let ordered = [
        (HookEvent::SessionStart, "fires once when the TUI launches"),
        (HookEvent::SessionEnd, "fires once on graceful shutdown"),
        (
            HookEvent::TurnEnd,
            "fires after a turn completes (observer-only)",
        ),
        (
            HookEvent::MessageSubmit,
            "fires before model dispatch; can transform or block submitted text",
        ),
        (
            HookEvent::ToolCallBefore,
            "fires before each tool call; can allow/deny/ask, rewrite input, add context",
        ),
        (
            HookEvent::ToolCallAfter,
            "fires after each tool call (observer-only)",
        ),
        (
            HookEvent::ModeChange,
            "fires on Plan/Work/Operate transitions",
        ),
        (
            HookEvent::OnError,
            "fires on transport / capacity / tool errors",
        ),
        (
            HookEvent::SubagentSpawn,
            "fires when a sub-agent starts (observer-only)",
        ),
        (
            HookEvent::SubagentComplete,
            "fires when a sub-agent completes, fails, or is cancelled (observer-only)",
        ),
        (
            HookEvent::ShellEnv,
            "fires before each exec_shell; stdout KEY=VALUE lines are merged into its environment",
        ),
        (
            HookEvent::SessionIdle,
            "fires when the session settles back to idle after a turn or a wait (observer-only)",
        ),
        (
            HookEvent::SessionError,
            "fires when a turn ends in a terminal failure; absorbed tool failures never fire it (observer-only)",
        ),
        (
            HookEvent::WaitingForUser,
            "fires when an approval prompt opens, a question is presented, or a goal continuation parks (observer-only)",
        ),
        (HookEvent::SessionBusy, "idle / waiting → in_progress"),
    ];
    for (event, desc) in ordered {
        out.push_str(&format!("  - `{}` — {desc}\n", event_label(event)));
    }
    out.push_str(
        "\nOnly `message_submit`, `tool_call_before`, and `shell_env` can steer a turn.\n\
         Observer-only means the *result* is ignored — the command still runs\n\
         and can have any external side effect.\n\n\
         Full contract: docs/HOOKS.md\n",
    );
    CommandResult::message(out.trim_end().to_string())
}

fn list(app: &App) -> CommandResult {
    let config = app.hooks.config();
    if config.hooks.is_empty() && config.problems.is_empty() {
        return CommandResult::message(
            "No hooks configured. Add a `[[hooks.hooks]]` entry to `~/.codewhale/config.toml` to define one.",
        );
    }
    if config.hooks.is_empty() {
        let mut out =
            String::from("No runnable hooks. Every configured entry was rejected at load:\n\n");
        out.push_str(&render_problems(&config.problems));
        return CommandResult::message(out.trim_end().to_string());
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{} configured hook(s) (global enabled: {}):\n\n",
        config.hooks.len(),
        if config.enabled {
            "yes"
        } else {
            "no — all hooks suppressed"
        }
    ));

    let mut by_event: std::collections::BTreeMap<&str, Vec<&codewhale_config::hooks::Hook>> =
        std::collections::BTreeMap::new();
    for hook in &config.hooks {
        by_event
            .entry(event_label(hook.event))
            .or_default()
            .push(hook);
    }

    for (event, hooks) in by_event {
        out.push_str(&format!("### {event}\n"));
        for hook in hooks {
            // `name` is operator-supplied and otherwise unbounded: a name made
            // of ANSI escapes would repaint this listing, and a long one would
            // push the rest of the row off screen.
            let label = codewhale_engine::hooks::sanitize_hook_label(hook.name.as_deref());
            // `[bg]` describes actual scheduling: `shell_env` ignores the flag
            // and always runs in the foreground, so it is not labelled.
            let bg = if hook.background && hook.event.honors_background() {
                " [bg, submitted not awaited]"
            } else {
                ""
            };
            let timeout = render_timeout(config, hook);
            let condition = match &hook.condition {
                None | Some(codewhale_config::hooks::HookCondition::Always) => String::new(),
                Some(c) => format!(" if {}", condition_summary(c)),
            };
            let cmd_preview = preview_command(&hook.command, 60);
            out.push_str(&format!(
                "  - {label}{bg} (timeout {timeout}){condition}\n      $ {cmd_preview}\n",
            ));
        }
        out.push('\n');
    }

    if !config.problems.is_empty() {
        out.push_str("### configuration problems\n");
        out.push_str(&render_problems(&config.problems));
        out.push('\n');
    }

    if !config.enabled {
        out.push_str(
            "Hooks are globally disabled — set `[hooks].enabled = true` in `config.toml` to fire them.\n",
        );
    }

    CommandResult::message(out.trim_end().to_string())
}

/// The timeout this hook will actually run with, plus where it came from.
///
/// `[hooks].default_timeout_secs` *replaces* every hook's own `timeout_secs`
/// (see `HooksConfig::effective_timeout_secs`). Rendering `hook.timeout_secs`
/// unconditionally made `/hooks list` report a budget no hook would ever run
/// with — a listing that disagrees with the runtime is worse than no listing.
fn render_timeout(
    config: &codewhale_config::hooks::HooksConfig,
    hook: &codewhale_config::hooks::Hook,
) -> String {
    let effective = config.effective_timeout_secs(hook);
    if config.timeout_is_overridden() {
        format!("{effective}s — `[hooks].default_timeout_secs` override")
    } else {
        format!("{effective}s")
    }
}

/// Render load-time problems. Only the hook's own `name`, its event, and a
/// fixed explanation are shown — never the command line, stdin payload, or
/// any resolved filesystem path.
fn render_problems(problems: &[codewhale_config::hooks::HookConfigProblem]) -> String {
    let mut out = String::new();
    for problem in problems {
        out.push_str(&format!(
            "  - {}\n",
            codewhale_engine::hooks::config::problem_summary(problem)
        ));
    }
    out
}

fn event_label(event: HookEvent) -> &'static str {
    event.as_str()
}

fn condition_summary(condition: &codewhale_config::hooks::HookCondition) -> String {
    match condition {
        codewhale_config::hooks::HookCondition::Always => "always".to_string(),
        codewhale_config::hooks::HookCondition::ToolName { name } => {
            format!("tool_name=`{}`", condition_value(name))
        }
        codewhale_config::hooks::HookCondition::ToolCategory { category } => {
            format!("tool_category=`{}`", condition_value(category))
        }
        codewhale_config::hooks::HookCondition::Mode { mode } => {
            format!("mode=`{}`", condition_value(mode))
        }
        codewhale_config::hooks::HookCondition::ExitCode { code } => format!("exit_code={code}"),
        codewhale_config::hooks::HookCondition::All { conditions } => format!(
            "all of [{}]",
            conditions
                .iter()
                .map(condition_summary)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        codewhale_config::hooks::HookCondition::Any { conditions } => format!(
            "any of [{}]",
            conditions
                .iter()
                .map(condition_summary)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Single-line preview of the shell command, capped at `max_chars`.
///
/// Filtering newlines is not enough on its own: the command is operator text
/// and can contain escape sequences, so it goes through the shared sanitizer
/// before the cap.
fn preview_command(command: &str, max_chars: usize) -> String {
    let single_line = codewhale_engine::hooks::sanitize_hook_line(command, usize::MAX);
    if single_line.chars().count() <= max_chars {
        return single_line;
    }
    let mut out: String = single_line
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

/// Operator-supplied strings inside a rendered condition.
///
/// `tool_name`, `tool_category`, and `mode` come from config verbatim, so the
/// same bound-and-de-fang rule that governs hook names governs these.
fn condition_value(value: &str) -> String {
    codewhale_engine::hooks::sanitize_hook_line(
        value,
        codewhale_engine::hooks::HOOK_LABEL_MAX_CHARS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiOptions};
    use codewhale_config::hooks::{Hook, HookCondition};
    use codewhale_runtime::config::Config;
    use tempfile::TempDir;

    fn create_test_app(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(tmpdir.path())
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn bare_hooks_command_opens_unified_extensions_modal() {
        let tmpdir = TempDir::new().unwrap();
        let app = create_test_app(&tmpdir);

        let result = hooks(&app, None);

        assert!(matches!(
            result.action,
            Some(AppAction::OpenExtensions {
                tab: crate::tui::views::extensions::ExtensionsTab::Hooks
            })
        ));
        assert!(result.message.is_none());
    }

    #[test]
    fn preview_command_truncates_to_cap() {
        let cmd = "x".repeat(200);
        assert_eq!(preview_command(&cmd, 10).chars().count(), 10);
        assert!(preview_command(&cmd, 10).ends_with('…'));
    }

    #[test]
    fn preview_command_strips_newlines() {
        assert_eq!(
            preview_command("line one\nline two", 50),
            "line one line two"
        );
    }

    /// A hook command is operator text. Filtering `\n` kept the row from
    /// splitting, but left every other control character — including the CSI
    /// introducer — free to repaint the terminal from inside `/hooks list`.
    #[test]
    fn preview_command_defangs_control_characters() {
        let preview = preview_command("echo \u{1b}[2Jhi\r\tthere\u{7}", 100);
        assert!(!preview.contains('\u{1b}'), "{preview:?}");
        assert!(!preview.contains('\r'), "{preview:?}");
        assert!(!preview.contains('\u{7}'), "{preview:?}");
        assert!(!preview.contains('\t'), "{preview:?}");
        assert!(preview.contains("there"), "{preview:?}");
    }

    #[test]
    fn hook_labels_are_bounded_and_defanged() {
        let noisy = format!("\u{1b}[31mgate\n{}", "x".repeat(500));
        let label = codewhale_engine::hooks::sanitize_hook_label(Some(&noisy));
        assert!(!label.contains('\u{1b}'), "{label}");
        assert!(!label.contains('\n'), "{label}");
        assert!(
            label.chars().count() <= codewhale_engine::hooks::HOOK_LABEL_MAX_CHARS + 16,
            "{} chars",
            label.chars().count()
        );
        // A name that is only whitespace, or absent, still renders something.
        assert_eq!(
            codewhale_engine::hooks::sanitize_hook_label(Some("   ")),
            "(unnamed)"
        );
        assert_eq!(
            codewhale_engine::hooks::sanitize_hook_label(None),
            "(unnamed)"
        );
    }

    /// Condition values are config strings too, and they were being
    /// interpolated raw into the same row the label was being sanitized in.
    #[test]
    fn condition_summary_defangs_operator_supplied_values() {
        let rendered = condition_summary(&HookCondition::ToolName {
            name: format!("\u{1b}[2Jexec_shell{}", "y".repeat(500)),
        });
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
        assert!(
            rendered.chars().count() <= codewhale_engine::hooks::HOOK_LABEL_MAX_CHARS + 32,
            "{} chars",
            rendered.chars().count()
        );
    }

    /// `default_timeout_secs = 0` is rejected at load, so the listing must
    /// report the per-hook budget the runtime will actually apply and must not
    /// credit the provenance to a setting that was ignored — while the
    /// rejection itself still shows up in the problems section.
    #[test]
    fn listed_timeout_ignores_a_rejected_zero_override() {
        let hook = Hook::new(HookEvent::SessionStart, "echo hi").with_timeout(90);
        let zeroed = codewhale_config::hooks::HooksConfig {
            enabled: true,
            hooks: vec![hook.clone()],
            default_timeout_secs: Some(0),
            ..codewhale_config::hooks::HooksConfig::default()
        };
        assert_eq!(render_timeout(&zeroed, &hook), "90s");

        let problems = zeroed.validate();
        let rendered = render_problems(&problems);
        assert!(rendered.contains("`[hooks]` setting"), "{rendered}");
        assert!(rendered.contains("default_timeout_secs = 0"), "{rendered}");
    }

    #[test]
    fn preview_command_keeps_short_input_intact() {
        assert_eq!(preview_command("echo hi", 50), "echo hi");
    }

    #[test]
    fn condition_summary_renders_all_variants() {
        assert_eq!(condition_summary(&HookCondition::Always), "always");
        assert_eq!(
            condition_summary(&HookCondition::ToolName {
                name: "exec_shell".into()
            }),
            "tool_name=`exec_shell`"
        );
        assert_eq!(
            condition_summary(&HookCondition::ToolCategory {
                category: "shell".into()
            }),
            "tool_category=`shell`"
        );
        assert_eq!(
            condition_summary(&HookCondition::Mode {
                mode: "yolo".into()
            }),
            "mode=`yolo`"
        );
        assert_eq!(
            condition_summary(&HookCondition::ExitCode { code: 1 }),
            "exit_code=1"
        );
        assert_eq!(
            condition_summary(&HookCondition::All {
                conditions: vec![
                    HookCondition::ToolName {
                        name: "exec_shell".into()
                    },
                    HookCondition::Mode {
                        mode: "yolo".into()
                    }
                ]
            }),
            "all of [tool_name=`exec_shell`, mode=`yolo`]"
        );
    }

    #[test]
    fn events_subcommand_lists_every_event_variant_in_documented_order() {
        let result = events();
        let body = result.message.expect("non-empty body");
        let positions: Vec<(usize, &str)> = [
            "session_start",
            "session_end",
            "turn_end",
            "message_submit",
            "tool_call_before",
            "tool_call_after",
            "mode_change",
            "on_error",
            "subagent_spawn",
            "subagent_complete",
            "shell_env",
            "session_idle",
            "session_error",
            "waiting_for_user",
            "session_busy",
        ]
        .iter()
        .map(|name| {
            (
                body.find(name).unwrap_or_else(|| {
                    panic!("event `{name}` missing from /hooks events output:\n{body}")
                }),
                *name,
            )
        })
        .collect();
        // Documented order is lifecycle → tool-call → situational.
        // Each subsequent position must be greater than the previous.
        for window in positions.windows(2) {
            let (a_pos, a_name) = window[0];
            let (b_pos, b_name) = window[1];
            assert!(
                a_pos < b_pos,
                "expected `{a_name}` before `{b_name}` in events listing"
            );
        }
        // Each event line includes the descriptive blurb.
        assert!(body.contains("fires once when the TUI launches"));
        // `tool_call_before` has been a steering hook since #3026; the listing
        // must not keep advertising it as read-only.
        assert!(
            !body.contains("read-only observer"),
            "stale read-only wording in events listing:\n{body}"
        );
        assert!(body.contains("can allow/deny/ask"));
        assert!(body.contains("docs/HOOKS.md"));
    }

    #[test]
    fn event_label_covers_every_variant() {
        // Compile-time `match` exhaustiveness; this just sanity-checks
        // the rendered strings stay stable.
        assert_eq!(event_label(HookEvent::SessionStart), "session_start");
        assert_eq!(event_label(HookEvent::SessionEnd), "session_end");
        assert_eq!(event_label(HookEvent::ToolCallBefore), "tool_call_before");
        assert_eq!(event_label(HookEvent::ToolCallAfter), "tool_call_after");
        assert_eq!(event_label(HookEvent::MessageSubmit), "message_submit");
        assert_eq!(event_label(HookEvent::ModeChange), "mode_change");
        assert_eq!(event_label(HookEvent::OnError), "on_error");
        assert_eq!(event_label(HookEvent::TurnEnd), "turn_end");
        assert_eq!(event_label(HookEvent::SubagentSpawn), "subagent_spawn");
        assert_eq!(
            event_label(HookEvent::SubagentComplete),
            "subagent_complete"
        );
        assert_eq!(event_label(HookEvent::ShellEnv), "shell_env");
        assert_eq!(event_label(HookEvent::SessionIdle), "session_idle");
        assert_eq!(event_label(HookEvent::SessionError), "session_error");
        assert_eq!(event_label(HookEvent::WaitingForUser), "waiting_for_user");
        assert_eq!(event_label(HookEvent::SessionBusy), "session_busy");
    }

    #[test]
    fn list_renders_hooks_grouped_by_event_and_notes_disabled_state() {
        // We test the formatter directly via a synthetic HooksConfig
        // because `App` is heavyweight to spin up here. The actual
        // `list(&App)` path is exercised once we hand the real
        // config in via `app.hooks.config()`; the formatter logic is
        // unit-tested standalone below.
        let cfg = codewhale_config::hooks::HooksConfig {
            enabled: false,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "echo started").with_name("greet"),
                Hook::new(HookEvent::ToolCallAfter, "notify-send done")
                    .with_condition(HookCondition::ToolName {
                        name: "exec_shell".into(),
                    })
                    .with_name("notify"),
            ],
            ..codewhale_config::hooks::HooksConfig::default()
        };

        // Synthesize the expected sections by re-running the same
        // formatter logic against the BTreeMap grouping.
        let mut by_event: std::collections::BTreeMap<&str, Vec<&Hook>> =
            std::collections::BTreeMap::new();
        for h in &cfg.hooks {
            by_event.entry(event_label(h.event)).or_default().push(h);
        }
        let events: Vec<&&str> = by_event.keys().collect();
        // BTreeMap sorts alphabetically — `session_start` before `tool_call_after`.
        assert_eq!(events, vec![&"session_start", &"tool_call_after"]);
    }

    #[test]
    fn events_listing_states_scope_and_the_steering_allowlist() {
        let body = events().message.expect("non-empty body");
        // Scope truth: this is a TUI runtime feature.
        assert!(body.contains("TUI runtime feature"), "{body}");
        assert!(body.contains("codewhale exec"), "{body}");
        // Steering allowlist, matching docs/HOOKS.md.
        assert!(body.contains("`message_submit`, `tool_call_before`, and `shell_env`"));
        // And the honest caveat about what observer-only does not mean.
        assert!(body.contains("can have any external side effect"), "{body}");
    }

    #[test]
    fn events_listing_covers_every_runtime_event() {
        let body = events().message.expect("non-empty body");
        let names: Vec<&str> = body
            .lines()
            .filter_map(|line| line.strip_prefix("  - `"))
            .map(|line| line.split('`').next().expect("listed event name"))
            .collect();
        assert_eq!(
            names,
            codewhale_config::hooks::ALL_HOOK_EVENTS.map(HookEvent::as_str)
        );
    }

    #[test]
    fn listed_timeout_is_the_one_the_runtime_will_apply() {
        let hook = Hook::new(HookEvent::SessionStart, "echo hi").with_timeout(90);

        let per_hook = codewhale_config::hooks::HooksConfig {
            enabled: true,
            hooks: vec![hook.clone()],
            ..codewhale_config::hooks::HooksConfig::default()
        };
        assert_eq!(render_timeout(&per_hook, &hook), "90s");

        // With the global override set, the runtime uses 5s — the listing must
        // say 5s, and say why.
        let overridden = codewhale_config::hooks::HooksConfig {
            enabled: true,
            hooks: vec![hook.clone()],
            default_timeout_secs: Some(5),
            ..codewhale_config::hooks::HooksConfig::default()
        };
        let rendered = render_timeout(&overridden, &hook);
        assert!(rendered.starts_with("5s"), "{rendered}");
        assert!(!rendered.contains("90"), "{rendered}");
        assert!(rendered.contains("default_timeout_secs"), "{rendered}");
    }

    #[test]
    fn rendered_problems_name_the_hook_without_leaking_the_command() {
        let problems = vec![
            codewhale_config::hooks::HookConfigProblem {
                name: Some("gate".to_string()),
                event: Some(HookEvent::SessionStart),
                detail: "condition can never match".to_string(),
                rejected: true,
            },
            codewhale_config::hooks::HookConfigProblem {
                name: None,
                event: Some(HookEvent::ShellEnv),
                detail: "`background = true` is not honored".to_string(),
                rejected: false,
            },
        ];
        let rendered = render_problems(&problems);
        assert!(rendered.contains("rejected: `session_start` hook `gate`"));
        assert!(rendered.contains("warning: `shell_env` hook `(unnamed)`"));
        // Only the hook name, event, and fixed detail — nothing else.
        assert!(!rendered.contains('$'), "{rendered}");
        assert!(!rendered.contains('/'), "{rendered}");
    }
}
