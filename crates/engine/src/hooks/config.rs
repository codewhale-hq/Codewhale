#[cfg(test)]
use codewhale_config::hooks::{ALL_HOOK_EVENTS, Hook, HookCondition, HookEvent, HookSteering};
use codewhale_config::hooks::{HookConfigProblem, HooksConfig};
use std::io::Read as _;
use std::path::Path;

/// Project hook files are executable configuration and must not become an
/// unbounded startup allocation merely because a trusted repository supplied
/// a very large file.
const PROJECT_HOOKS_FILE_MAX_BYTES: usize = 1024 * 1024;

pub(super) fn read_project_hooks_file(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut contents = String::new();
    file.take((PROJECT_HOOKS_FILE_MAX_BYTES + 1) as u64)
        .read_to_string(&mut contents)?;
    if contents.len() > PROJECT_HOOKS_FILE_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "project hooks file exceeds the 1 MiB limit",
        ));
    }
    Ok(contents)
}

/// Stable, redaction-safe one-line rendering for logs and `/hooks`.
#[must_use]
pub fn problem_summary(problem: &HookConfigProblem) -> String {
    let disposition = if problem.rejected {
        "rejected"
    } else {
        "warning"
    };
    let Some(event) = problem.event else {
        return format!("{disposition}: `[hooks]` setting — {}", problem.detail);
    };
    // The name is operator-supplied and lands in `/hooks list` and the
    // tracing stream; bound it and strip control characters here rather
    // than trusting every caller to remember.
    let label = super::executor::sanitize_hook_label(problem.name.as_deref());
    format!(
        "{disposition}: `{}` hook `{label}` — {}",
        event.as_str(),
        problem.detail
    )
}

/// Seed for a workspace's `.codewhale/hooks.toml` when it does not exist yet.
///
/// Entirely commented out: creating the file must never change behaviour, and
/// an empty file that teaches the schema beats an empty file that does not.
/// The event list here is the one `/hooks events` prints.
pub const PROJECT_HOOKS_TEMPLATE: &str = r#"# Codewhale project hooks.
#
# Hooks are executable repository configuration: they run only after this
# workspace is trusted and these exact bytes approved (`/hooks review`, then
# `/hooks approve <digest>`). Global hooks live in the `[hooks]`
# table of your own config.toml; the entries here are appended after those.
#
# Run `/hooks events` in Codewhale for the full event list with descriptions.
#
# Uncomment to try one:
#
# [[hooks]]
# name = "format on write"
# event = "post_tool_use"
# command = "cargo fmt --all"
# timeout_secs = 30
# background = true
# continue_on_error = true
"#;

/// Load global hooks merged with project-local `.codewhale/hooks.toml` (#3026).
///
/// Project hooks are executable repository configuration, so they are only
/// honored after workspace trust and exact-byte hook approval in user config.
/// Trusted project hooks are appended after global hooks.  A malformed
/// trusted project file logs a warning and falls back to global-only.
#[cfg(any(test, feature = "test-support"))]
pub fn load_with_project(global: HooksConfig, workspace: &Path) -> HooksConfig {
    load_with_project_and_plugins(global, workspace, None)
}

/// Merge global, reviewed plugin, then trusted project hooks.
///
/// Project hooks intentionally remain last because that is the existing
/// tie-breaking contract for mutable `message_submit` transformations.
/// Plugin files are read only from Codewhale's immutable staged snapshot;
/// their attached authority is rechecked at every process-spawn boundary.
pub fn load_with_project_and_plugins(
    global: HooksConfig,
    workspace: &Path,
    plugins: Option<&codewhale_plugins::PluginRegistry>,
) -> HooksConfig {
    let mut merged = global;
    if let Some(plugins) = plugins {
        let (sources, adapter_errors) = codewhale_plugins::runtime::active_component_sources(
            plugins,
            codewhale_plugins::activation::PluginActivationCapability::Hooks,
        );
        for error in adapter_errors {
            merged.problems.push(HookConfigProblem {
                name: None,
                event: None,
                detail: error,
                rejected: true,
            });
        }
        for source in sources {
            match load_plugin_hook_component(&source.path, &source.authority) {
                Ok(mut plugin) => {
                    merged.problems.append(&mut plugin.problems);
                    merged.hooks.append(&mut plugin.hooks);
                }
                Err(error) => merged.problems.push(HookConfigProblem {
                    name: Some(source.plugin_name),
                    event: None,
                    detail: error,
                    rejected: true,
                }),
            }
        }
    }
    let project_path = workspace.join(".codewhale").join("hooks.toml");
    if project_path.symlink_metadata().is_ok() {
        match super::authority::approved_project_hooks(workspace) {
            Ok((authority, contents)) => match toml::from_str::<HooksConfig>(&contents) {
                Ok(mut project) => {
                    for hook in &mut project.hooks {
                        hook.project_authority = Some(authority.clone());
                    }
                    merged.hooks.extend(project.hooks);
                }
                Err(_) => merged.problems.push(HookConfigProblem {
                    name: None,
                    event: None,
                    detail: "Invalid project hooks TOML; project hooks were not loaded".into(),
                    rejected: true,
                }),
            },
            Err(detail) => merged.problems.push(HookConfigProblem {
                name: None,
                event: None,
                detail,
                rejected: true,
            }),
        }
    }
    // Validation runs on every path, not just the project-hooks path, so a
    // globally-configured hook that can never match is rejected too.
    apply_validation(&mut merged);
    merged
}

/// Run [`HooksConfig::validate_indexed`], drop every rejected hook, and record the
/// problems so `/hooks` and the logs can show them.
///
/// Rejection is by position, so a broken entry never takes an innocent one
/// with it just because the two share a name (or share the absence of one).
fn apply_validation(config: &mut HooksConfig) {
    let inherited_problems = std::mem::take(&mut config.problems);
    let setting_problems = config.validate_settings();
    // Reject the value, not just report it: the executor reads
    // `default_timeout_secs` directly, so leaving `Some(0)` in place would
    // make the warning cosmetic.
    if setting_problems.iter().any(|p| p.rejected) {
        config.default_timeout_secs = config.default_timeout_secs.filter(|secs| *secs > 0);
    }
    let problems = config.validate_indexed();
    for problem in setting_problems
        .iter()
        .chain(problems.iter().map(|(_, p)| p))
    {
        tracing::warn!(target: "hooks", "{}", problem_summary(problem));
    }
    let rejected: std::collections::HashSet<usize> = problems
        .iter()
        .filter(|(_, problem)| problem.rejected)
        .map(|(index, _)| *index)
        .collect();
    if !rejected.is_empty() {
        let mut index = 0;
        config.hooks.retain(|_| {
            let keep = !rejected.contains(&index);
            index += 1;
            keep
        });
    }
    config.problems = inherited_problems
        .into_iter()
        .chain(setting_problems)
        .chain(problems.into_iter().map(|(_, problem)| problem))
        .collect();
}

fn load_plugin_hook_component(
    component: &Path,
    authority: &codewhale_config::PluginAuthority,
) -> Result<HooksConfig, String> {
    let mut paths = if component.is_file() {
        vec![component.to_path_buf()]
    } else if component.is_dir() {
        let mut paths = std::fs::read_dir(component)
            .map_err(|error| format!("failed to read plugin Hooks component: {error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("toml"))
            .collect::<Vec<_>>();
        paths.sort();
        paths
    } else {
        return Err("plugin Hooks component is unavailable".to_string());
    };
    if paths.is_empty() {
        return Err("plugin Hooks component contains no TOML configuration".to_string());
    }

    let mut merged = HooksConfig::default();
    for path in paths.drain(..) {
        let contents = read_project_hooks_file(&path)
            .map_err(|error| format!("failed to read plugin Hooks file: {error}"))?;
        let mut parsed: HooksConfig = toml::from_str(&contents)
            .map_err(|error| format!("failed to parse plugin Hooks file: {error}"))?;
        if parsed.working_dir.is_some() {
            return Err(
                "plugin Hooks may not set working_dir; hooks run in the active workspace"
                    .to_string(),
            );
        }
        apply_validation(&mut parsed);
        if !parsed.enabled {
            continue;
        }
        if let Some(timeout) = parsed.default_timeout_secs.filter(|value| *value > 0) {
            for hook in &mut parsed.hooks {
                hook.timeout_secs = timeout;
            }
        }
        for hook in &mut parsed.hooks {
            hook.plugin_authority = Some(authority.clone());
        }
        merged.problems.append(&mut parsed.problems);
        merged.hooks.append(&mut parsed.hooks);
    }
    Ok(merged)
}

pub fn workspace_allows_project_hooks(workspace: &Path) -> bool {
    super::authority::approved_project_hooks(workspace).is_ok()
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    /// The fifteen event names are a public contract: they appear in
    /// `config.toml`, in `/hooks events`, and in `docs/HOOKS.md`. A rename is
    /// a breaking change, and a new variant must be added deliberately.
    #[test]
    fn all_fifteen_event_names_are_stable_and_exhaustive() {
        let names: Vec<&str> = ALL_HOOK_EVENTS.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            names,
            vec![
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
        );

        // Exhaustiveness: every variant appears exactly once. The `match` here
        // fails to compile if a variant is added without updating the list.
        for event in ALL_HOOK_EVENTS {
            let covered = match event {
                HookEvent::SessionStart
                | HookEvent::SessionEnd
                | HookEvent::TurnEnd
                | HookEvent::MessageSubmit
                | HookEvent::ToolCallBefore
                | HookEvent::ToolCallAfter
                | HookEvent::ModeChange
                | HookEvent::OnError
                | HookEvent::SubagentSpawn
                | HookEvent::SubagentComplete
                | HookEvent::ShellEnv
                | HookEvent::SessionIdle
                | HookEvent::SessionError
                | HookEvent::WaitingForUser
                | HookEvent::SessionBusy => true,
            };
            assert!(covered);
        }
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), 15);
    }

    #[test]
    fn documented_event_table_matches_the_runtime_registry() {
        let docs = include_str!("../../../../docs/HOOKS.md");
        let names: Vec<&str> = docs
            .lines()
            .skip_while(|line| *line != "| Event | Fires | Steering |")
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .map(|line| line.split('`').nth(1).expect("event name in table row"))
            .collect();
        assert_eq!(names, ALL_HOOK_EVENTS.map(HookEvent::as_str));
        let heading = format!("## The {} events", names.len());
        assert!(docs.lines().any(|line| line == heading));
    }

    /// Serde round-trip for every event name, in the exact `event = "..."`
    /// spelling users write in `config.toml`.
    #[test]
    fn every_event_name_round_trips_through_serde() {
        for event in ALL_HOOK_EVENTS {
            let json = serde_json::to_string(&event).expect("serialize");
            assert_eq!(json, format!("\"{}\"", event.as_str()));
            let parsed: HookEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, event);

            let toml_src = format!("event = \"{}\"\ncommand = \"true\"\n", event.as_str());
            let hook: Hook = toml::from_str(&toml_src).expect("hook parses from minimal toml");
            assert_eq!(hook.event, event);
        }
    }

    /// Backward compatibility: a pre-existing hook table with only the two
    /// required keys still parses, and the defaults are the documented ones.
    #[test]
    fn minimal_hook_toml_keeps_its_documented_defaults() {
        let hook: Hook = toml::from_str(
            r#"
event = "session_start"
command = "echo hi"
"#,
        )
        .expect("parse");
        assert_eq!(hook.timeout_secs, 30);
        assert!(!hook.background);
        assert!(hook.continue_on_error);
        assert!(hook.condition.is_none());
        assert!(hook.name.is_none());
    }

    /// `problems` is runtime-only state. It must never appear in a serialized
    /// config, and its absence must not break deserialization.
    #[test]
    fn config_problems_are_not_part_of_the_serialized_config() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::SessionStart, "true")],
            problems: vec![HookConfigProblem {
                name: Some("x".to_string()),
                event: Some(HookEvent::SessionStart),
                detail: "example".to_string(),
                rejected: true,
            }],
            ..HooksConfig::default()
        };
        let serialized = serde_json::to_string(&config).expect("serialize");
        assert!(!serialized.contains("problems"), "{serialized}");
        assert!(!serialized.contains("example"), "{serialized}");

        let reparsed: HooksConfig = serde_json::from_str(&serialized).expect("reparse");
        assert!(reparsed.problems.is_empty());
        assert_eq!(reparsed.hooks.len(), 1);

        // And a config that predates the field still deserializes.
        let legacy: HooksConfig = toml::from_str(
            r#"
enabled = true

[[hooks]]
event = "session_start"
command = "echo hi"
"#,
        )
        .expect("legacy config parses");
        assert!(legacy.problems.is_empty());
        assert_eq!(legacy.hooks.len(), 1);
    }

    /// The steering allowlist. Exactly three events can change what Codewhale
    /// does; every other event defaults to observer.
    #[test]
    fn steering_allowlist_is_exactly_three_events() {
        let steering: Vec<&str> = ALL_HOOK_EVENTS
            .iter()
            .filter(|e| e.can_steer())
            .map(|e| e.as_str())
            .collect();
        assert_eq!(
            steering,
            vec!["message_submit", "tool_call_before", "shell_env"]
        );

        assert_eq!(
            HookEvent::MessageSubmit.steering(),
            HookSteering::TransformsSubmittedText
        );
        assert_eq!(
            HookEvent::ToolCallBefore.steering(),
            HookSteering::DecidesToolCall
        );
        assert_eq!(
            HookEvent::ShellEnv.steering(),
            HookSteering::ContributesShellEnv
        );

        for event in ALL_HOOK_EVENTS {
            if !steering.contains(&event.as_str()) {
                assert_eq!(
                    event.steering(),
                    HookSteering::Observer,
                    "`{}` must default to observer",
                    event.as_str()
                );
            }
        }
    }

    /// Observer-only is a claim about Codewhale's control flow, not about the
    /// command. This test exists so the distinction is written down somewhere
    /// executable: an observer hook is still an arbitrary shell command and
    /// its external side effects are entirely real.
    #[test]
    fn observer_only_still_runs_a_real_command_with_real_side_effects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("observer-side-effect.txt");
        assert!(!marker.exists());

        let command = if cfg!(windows) {
            format!("echo touched> {}", marker.display())
        } else {
            format!("echo touched > {}", marker.display())
        };
        let executor = crate::hooks::HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::SessionEnd, &command).with_name("observer")],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let results = executor.execute(
            HookEvent::SessionEnd,
            &crate::hooks::HookContext::new().with_session_id("sess_test"),
        );

        // Codewhale ignored the result...
        assert_eq!(results.len(), 1);
        assert_eq!(HookEvent::SessionEnd.steering(), HookSteering::Observer);
        // ...and the command still changed the filesystem.
        assert!(
            marker.exists(),
            "an observer hook is not side-effect free; it just cannot steer"
        );
    }

    #[test]
    fn exit_code_conditions_are_rejected_only_where_no_exit_code_exists() {
        for event in ALL_HOOK_EVENTS {
            let config = HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(event, "true")
                        .with_name("gate")
                        .with_condition(HookCondition::ExitCode { code: 1 }),
                ],
                ..HooksConfig::default()
            };
            let problems = config.validate();
            if event.provides_exit_code() {
                assert!(
                    problems.is_empty(),
                    "`{}` should accept an exit_code condition: {problems:?}",
                    event.as_str()
                );
            } else {
                assert!(
                    problems.iter().any(|p| p.rejected),
                    "`{}` must reject an exit_code condition",
                    event.as_str()
                );
            }
        }
        assert!(HookEvent::ToolCallAfter.provides_exit_code());
        // `on_error` fires for tool failures with the tool name, call id, and
        // reported exit code attached (`tui/tool_routing.rs`), so scoping an
        // `on_error` hook by tool or exit code is a supported configuration —
        // it used to be rejected at load while the runtime and docs both
        // promised those fields.
        assert!(HookEvent::OnError.provides_exit_code());
        assert!(HookEvent::OnError.provides_tool_identity());
    }

    /// A tool-scoped `on_error` hook — the shape `docs/HOOKS.md` documents and
    /// `tool_routing.rs` supplies context for — must survive load intact.
    #[test]
    fn tool_scoped_on_error_hooks_load_and_dispatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::OnError, "notify.sh")
                    .with_name("shell-failure")
                    .with_condition(HookCondition::All {
                        conditions: vec![
                            HookCondition::ToolName {
                                name: "exec_shell".to_string(),
                            },
                            HookCondition::ExitCode { code: 127 },
                        ],
                    }),
            ],
            ..HooksConfig::default()
        };

        let loaded = load_with_project(global, dir.path());

        assert_eq!(loaded.hooks.len(), 1, "{:?}", loaded.problems);
        assert!(
            loaded.problems.iter().all(|p| !p.rejected),
            "{:?}",
            loaded.problems
        );
        assert_eq!(loaded.hooks_for_event(HookEvent::OnError).len(), 1);
    }

    /// A Windows crash code such as `0xC0000005` does not fit in `i32`. The
    /// predicate has to hold it, or the hook silently never matches.
    #[test]
    fn exit_code_conditions_hold_large_windows_crash_codes() {
        let hook: Hook = toml::from_str(
            r#"
event = "tool_call_after"
command = "echo crashed"
condition = { type = "exit_code", code = 3221225477 }
"#,
        )
        .expect("large exit code parses");
        assert!(matches!(
            hook.condition,
            Some(HookCondition::ExitCode {
                code: 3_221_225_477
            })
        ));

        // Old, small values keep parsing exactly as before.
        let legacy: Hook = toml::from_str(
            r#"
event = "tool_call_after"
command = "echo failed"
condition = { type = "exit_code", code = 1 }
"#,
        )
        .expect("small exit code still parses");
        assert!(matches!(
            legacy.condition,
            Some(HookCondition::ExitCode { code: 1 })
        ));
    }

    /// Rejection is per entry. One broken hook must not delete the hooks that
    /// merely share its name — or share its lack of one.
    #[test]
    fn rejection_drops_only_the_offending_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global = HooksConfig {
            enabled: true,
            hooks: vec![
                // Two unnamed `session_start` entries; only the second is
                // invalid (an `exit_code` predicate that can never match).
                Hook::new(HookEvent::SessionStart, "echo innocent-unnamed"),
                Hook::new(HookEvent::SessionStart, "echo broken-unnamed")
                    .with_condition(HookCondition::ExitCode { code: 0 }),
                // Two hooks sharing the name `gate`; only the second is empty.
                Hook::new(HookEvent::ToolCallBefore, "echo innocent-gate").with_name("gate"),
                Hook::new(HookEvent::ToolCallBefore, "   ").with_name("gate"),
            ],
            ..HooksConfig::default()
        };

        let loaded = load_with_project(global, dir.path());

        let surviving: Vec<&str> = loaded.hooks.iter().map(|h| h.command.as_str()).collect();
        assert_eq!(
            surviving,
            vec!["echo innocent-unnamed", "echo innocent-gate"],
            "an invalid entry took an innocent same-identity entry with it"
        );
        assert_eq!(
            loaded.problems.iter().filter(|p| p.rejected).count(),
            2,
            "{:?}",
            loaded.problems
        );
        assert_eq!(loaded.hooks_for_event(HookEvent::SessionStart).len(), 1);
        assert_eq!(loaded.hooks_for_event(HookEvent::ToolCallBefore).len(), 1);
    }

    #[test]
    fn effective_timeout_reports_the_global_override() {
        let hook = Hook::new(HookEvent::SessionStart, "true").with_timeout(90);

        let per_hook = HooksConfig::default();
        assert_eq!(per_hook.effective_timeout_secs(&hook), 90);
        assert!(!per_hook.timeout_is_overridden());

        let overridden = HooksConfig {
            default_timeout_secs: Some(5),
            ..HooksConfig::default()
        };
        assert_eq!(overridden.effective_timeout_secs(&hook), 5);
        assert!(overridden.timeout_is_overridden());
    }

    /// `timeout_secs = 0` was rejected per hook, but the override that
    /// *replaces* every hook's value was not checked at all — so a single
    /// `default_timeout_secs = 0` killed every hook before it could speak,
    /// including `tool_call_before` gates that then fail closed on every call.
    #[test]
    fn zero_default_timeout_is_rejected_and_ignored() {
        let hook = Hook::new(HookEvent::SessionStart, "true").with_timeout(90);
        let zeroed = HooksConfig {
            enabled: true,
            hooks: vec![hook.clone()],
            default_timeout_secs: Some(0),
            ..HooksConfig::default()
        };

        let problems = zeroed.validate();
        let problem = problems
            .iter()
            .find(|p| p.detail.contains("default_timeout_secs"))
            .expect("zero override reported");
        assert!(problem.rejected, "{problem:?}");
        assert!(
            problem.event.is_none(),
            "the override is not one hook's problem: {problem:?}"
        );
        assert!(problem_summary(problem).contains("`[hooks]` setting"));

        // Even unvalidated, the accessors refuse the value rather than hand a
        // zero budget to the executor.
        assert_eq!(zeroed.effective_timeout_secs(&hook), 90);
        assert!(!zeroed.timeout_is_overridden());
    }

    /// The load path must strip the value, not merely warn about it: the
    /// executor reads `default_timeout_secs` and the hook itself is innocent,
    /// so it has to survive.
    #[test]
    fn zero_default_timeout_is_stripped_at_load_and_the_hook_survives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "true")
                    .with_name("greet")
                    .with_timeout(90),
            ],
            default_timeout_secs: Some(0),
            ..HooksConfig::default()
        };

        let loaded = load_with_project(global, dir.path());
        assert_eq!(loaded.default_timeout_secs, None, "override not stripped");
        assert_eq!(loaded.hooks.len(), 1, "the hook itself was not at fault");
        assert_eq!(loaded.effective_timeout_secs(&loaded.hooks[0]), 90);
        assert!(
            loaded
                .problems
                .iter()
                .any(|p| p.rejected && p.event.is_none()),
            "{:?}",
            loaded.problems
        );
    }

    /// A positive override still loads untouched — the rejection is for zero
    /// only, not a general distrust of the setting.
    #[test]
    fn positive_default_timeout_survives_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loaded = load_with_project(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::SessionStart, "true").with_timeout(90)],
                default_timeout_secs: Some(5),
                ..HooksConfig::default()
            },
            dir.path(),
        );
        assert_eq!(loaded.default_timeout_secs, Some(5));
        assert!(loaded.timeout_is_overridden());
        assert_eq!(loaded.effective_timeout_secs(&loaded.hooks[0]), 5);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    }

    /// Names are operator text and reach `/hooks list` and the tracing stream.
    #[test]
    fn problem_summaries_bound_and_defang_the_hook_name() {
        let problem = HookConfigProblem {
            name: Some(format!("\u{1b}[2Jgate\n{}", "n".repeat(500))),
            event: Some(HookEvent::ToolCallBefore),
            detail: "example detail".to_string(),
            rejected: true,
        };
        let summary = problem_summary(&problem);
        assert!(!summary.contains('\u{1b}'), "{summary}");
        assert!(!summary.contains('\n'), "{summary}");
        assert!(summary.contains("gate"), "{summary}");
        assert!(
            summary.chars().count() < 200,
            "unbounded summary: {} chars",
            summary.chars().count()
        );
    }

    #[test]
    fn mode_conditions_are_rejected_on_shell_env_only() {
        for event in ALL_HOOK_EVENTS {
            let config = HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(event, "true").with_condition(HookCondition::Mode {
                        mode: "plan".to_string(),
                    }),
                ],
                ..HooksConfig::default()
            };
            let rejected = config.validate().iter().any(|p| p.rejected);
            assert_eq!(
                rejected,
                matches!(event, HookEvent::ShellEnv),
                "unexpected mode-condition disposition for `{}`",
                event.as_str()
            );
        }
    }

    #[test]
    fn tool_conditions_are_rejected_on_events_with_no_tool() {
        for event in ALL_HOOK_EVENTS {
            for condition in [
                HookCondition::ToolName {
                    name: "exec_shell".to_string(),
                },
                HookCondition::ToolCategory {
                    category: "shell".to_string(),
                },
            ] {
                let config = HooksConfig {
                    enabled: true,
                    hooks: vec![Hook::new(event, "true").with_condition(condition)],
                    ..HooksConfig::default()
                };
                let rejected = config.validate().iter().any(|p| p.rejected);
                assert_eq!(
                    rejected,
                    !event.provides_tool_identity(),
                    "unexpected tool-condition disposition for `{}`",
                    event.as_str()
                );
            }
        }
    }

    #[test]
    fn unsupported_conditions_nested_in_combinators_are_still_rejected() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "true")
                    .with_name("sneaky")
                    .with_condition(HookCondition::Any {
                        conditions: vec![
                            HookCondition::Always,
                            HookCondition::All {
                                conditions: vec![HookCondition::ExitCode { code: 0 }],
                            },
                        ],
                    }),
            ],
            ..HooksConfig::default()
        };
        let problems = config.validate();
        assert!(
            problems.iter().any(|p| p.rejected),
            "a nested unsupported predicate must not hide behind a combinator"
        );
    }

    #[test]
    fn rejected_hooks_are_dropped_at_load_and_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "echo ok").with_name("good"),
                Hook::new(HookEvent::SessionStart, "echo never")
                    .with_name("inert")
                    .with_condition(HookCondition::ExitCode { code: 0 }),
            ],
            ..HooksConfig::default()
        };

        let loaded = load_with_project(global, dir.path());

        assert_eq!(
            loaded.hooks.len(),
            1,
            "the inert hook must not survive load"
        );
        assert_eq!(loaded.hooks[0].name.as_deref(), Some("good"));
        assert!(loaded.problems.iter().any(|p| p.rejected));
        // It is also invisible to dispatch, not merely to the listing.
        assert_eq!(loaded.hooks_for_event(HookEvent::SessionStart).len(), 1);
    }

    #[test]
    fn empty_command_and_zero_timeout_are_rejected() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "   ").with_name("blank"),
                Hook::new(HookEvent::SessionEnd, "true")
                    .with_name("instant")
                    .with_timeout(0),
            ],
            ..HooksConfig::default()
        };
        let problems = config.validate();
        assert_eq!(problems.iter().filter(|p| p.rejected).count(), 2);
    }

    #[test]
    fn background_flag_truth_is_reported_per_event() {
        // `shell_env` does not honor the flag at all — that is a warning, and
        // the hook still runs.
        let shell_env = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::ShellEnv, "true")
                    .with_name("creds")
                    .background(),
            ],
            ..HooksConfig::default()
        };
        let problems = shell_env.validate();
        assert_eq!(problems.len(), 1);
        assert!(!problems[0].rejected, "the hook still runs, in foreground");
        assert!(problems[0].detail.contains("not honored"));
        assert!(!HookEvent::ShellEnv.honors_background());

        // A background steering hook is honored scheduling, but it silently
        // stops steering — worth saying out loud.
        for event in [HookEvent::MessageSubmit, HookEvent::ToolCallBefore] {
            let config = HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(event, "true").with_name("gate").background()],
                ..HooksConfig::default()
            };
            let problems = config.validate();
            assert_eq!(problems.len(), 1, "{}", event.as_str());
            assert!(!problems[0].rejected);
            assert!(problems[0].detail.contains("observer-only"));
            assert!(event.honors_background());
        }

        // A background observer hook is unremarkable.
        let observer = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::TurnEnd, "true").background()],
            ..HooksConfig::default()
        };
        assert!(observer.validate().is_empty());
    }

    #[test]
    fn problem_summaries_carry_no_command_or_path() {
        let problem = HookConfigProblem {
            name: Some("gate".to_string()),
            event: Some(HookEvent::ToolCallBefore),
            detail: "example detail".to_string(),
            rejected: true,
        };
        let summary = problem_summary(&problem);
        assert!(summary.contains("rejected"));
        assert!(summary.contains("tool_call_before"));
        assert!(summary.contains("gate"));

        let unnamed = HookConfigProblem {
            name: None,
            rejected: false,
            ..problem
        };
        assert!(problem_summary(&unnamed).contains("(unnamed)"));
        assert!(problem_summary(&unnamed).contains("warning"));
    }

    #[test]
    fn project_hook_file_read_is_bounded_before_toml_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hooks.toml");
        std::fs::write(&path, "x".repeat(super::PROJECT_HOOKS_FILE_MAX_BYTES + 1))
            .expect("write oversized hook config");
        let error = super::read_project_hooks_file(&path)
            .expect_err("oversized project hook config must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("1 MiB"));
    }

    /// The seeded project hooks file must be inert: creating it can never
    /// change behaviour, only teach the schema.
    #[test]
    fn project_hooks_template_parses_and_configures_nothing() {
        let parsed: HooksConfig =
            toml::from_str(PROJECT_HOOKS_TEMPLATE).expect("the seeded template must be valid TOML");
        assert!(
            parsed.hooks.is_empty(),
            "a freshly created hooks file must define no hooks"
        );
        assert!(parsed.problems.is_empty());
        assert!(
            PROJECT_HOOKS_TEMPLATE.contains("/hooks events"),
            "the template must point at the event list rather than restate it"
        );
    }
}
