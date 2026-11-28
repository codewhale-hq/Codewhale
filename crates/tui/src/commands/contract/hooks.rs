use super::SharedCommandHost;
use crate::tui::app::AppAction;
use codewhale_command_contract::hooks::{
    CommandHooksContext, HookEntrySummary, HooksAction, HooksListSnapshot, ProjectHooksReview,
};
use codewhale_config::hooks::HookCondition;

pub(crate) struct HooksAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl TryFrom<HooksAction> for AppAction {
    type Error = String;

    fn try_from(action: HooksAction) -> Result<Self, String> {
        Ok(match action {
            HooksAction::OpenExtensionsTab => Self::OpenExtensions {
                tab: crate::tui::views::extensions::ExtensionsTab::Hooks,
            },
            HooksAction::EditProjectHooks => Self::EditProjectHooks,
        })
    }
}

impl CommandHooksContext for HooksAdapter<'_> {
    fn list_hooks(&self) -> HooksListSnapshot {
        let app = self.host.app.borrow();
        let config = app.hooks.config();
        let hooks = config
            .hooks
            .iter()
            .map(|hook| HookEntrySummary {
                event_label: hook.event.as_str().to_string(),
                label: codewhale_engine::hooks::sanitize_hook_label(hook.name.as_deref()),
                background: hook.background,
                honors_background: hook.event.honors_background(),
                timeout_display: render_timeout(config, hook),
                condition_summary: match &hook.condition {
                    None | Some(HookCondition::Always) => None,
                    Some(c) => Some(condition_summary(c)),
                },
                command_preview: preview_command(&hook.command, 60),
            })
            .collect();
        let problem_summaries = config
            .problems
            .iter()
            .map(codewhale_engine::hooks::config::problem_summary)
            .collect();
        HooksListSnapshot {
            enabled: config.enabled,
            hooks,
            problem_summaries,
        }
    }

    fn review_project_hooks(&self) -> Result<ProjectHooksReview, String> {
        let app = self.host.app.borrow();
        let (authority, contents) =
            codewhale_engine::hooks::authority::review_project_hooks(&app.workspace)?;
        Ok(ProjectHooksReview {
            digest: authority.digest,
            sanitized_contents: codewhale_engine::hooks::sanitize_hook_text(
                &contents,
                contents.chars().count(),
            ),
        })
    }

    fn approve_project_hooks(&mut self, digest: &str) -> Result<(), String> {
        let app = self.host.app.borrow();
        codewhale_engine::hooks::authority::approve_project_hooks(&app.workspace, digest)
    }

    fn revoke_project_hooks(&mut self) -> Result<(), String> {
        let app = self.host.app.borrow();
        codewhale_runtime::config::save_workspace_hook_receipt(&app.workspace, "")
            .map(|_| ())
            .map_err(|_| "Could not revoke project hook approval".to_string())
    }
}

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

#[cfg(test)]
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

fn condition_summary(condition: &HookCondition) -> String {
    match condition {
        HookCondition::Always => "always".to_string(),
        HookCondition::ToolName { name } => format!("tool_name=`{}`", condition_value(name)),
        HookCondition::ToolCategory { category } => {
            format!("tool_category=`{}`", condition_value(category))
        }
        HookCondition::Mode { mode } => format!("mode=`{}`", condition_value(mode)),
        HookCondition::ExitCode { code } => format!("exit_code={code}"),
        HookCondition::All { conditions } => format!(
            "all of [{}]",
            conditions
                .iter()
                .map(condition_summary)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        HookCondition::Any { conditions } => format!(
            "any of [{}]",
            conditions
                .iter()
                .map(condition_summary)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

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

fn condition_value(value: &str) -> String {
    codewhale_engine::hooks::sanitize_hook_line(
        value,
        codewhale_engine::hooks::HOOK_LABEL_MAX_CHARS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, AppAction, TuiOptions};
    use codewhale_config::hooks::{Hook, HookCondition, HookEvent};
    use codewhale_runtime::config::Config;
    use tempfile::TempDir;

    fn create_test_app(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            ..crate::tui::test_support::test_tui_options(tmpdir.path())
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn bare_hooks_command_opens_unified_extensions_modal() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app(&tmpdir);

        let result = crate::commands::execute("/hooks", &mut app);

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
        assert_eq!(
            codewhale_engine::hooks::sanitize_hook_label(Some("   ")),
            "(unnamed)"
        );
        assert_eq!(
            codewhale_engine::hooks::sanitize_hook_label(None),
            "(unnamed)"
        );
    }

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
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app(&tmpdir);
        let result = crate::commands::execute("/hooks events", &mut app);
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
        for window in positions.windows(2) {
            let (a_pos, a_name) = window[0];
            let (b_pos, b_name) = window[1];
            assert!(
                a_pos < b_pos,
                "expected `{a_name}` before `{b_name}` in events listing"
            );
        }
        assert!(body.contains("fires once when the TUI launches"));
        assert!(
            !body.contains("read-only observer"),
            "stale read-only wording in events listing:\n{body}"
        );
        assert!(body.contains("can allow/deny/ask"));
        assert!(body.contains("docs/HOOKS.md"));
    }

    #[test]
    fn list_renders_hooks_grouped_by_event_and_notes_disabled_state() {
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

        let mut by_event: std::collections::BTreeMap<&str, Vec<&Hook>> =
            std::collections::BTreeMap::new();
        for h in &cfg.hooks {
            by_event.entry(h.event.as_str()).or_default().push(h);
        }
        let events: Vec<&&str> = by_event.keys().collect();
        assert_eq!(events, vec![&"session_start", &"tool_call_after"]);
    }

    #[test]
    fn events_listing_states_scope_and_the_steering_allowlist() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app(&tmpdir);
        let body = crate::commands::execute("/hooks events", &mut app)
            .message
            .expect("non-empty body");
        assert!(body.contains("TUI runtime feature"), "{body}");
        assert!(body.contains("codewhale exec"), "{body}");
        assert!(body.contains("`message_submit`, `tool_call_before`, and `shell_env`"));
        assert!(body.contains("can have any external side effect"), "{body}");
    }

    #[test]
    fn events_listing_covers_every_runtime_event() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app(&tmpdir);
        let body = crate::commands::execute("/hooks events", &mut app)
            .message
            .expect("non-empty body");
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
        assert!(!rendered.contains('$'), "{rendered}");
        assert!(!rendered.contains('/'), "{rendered}");
    }
}
