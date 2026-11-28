use std::collections::BTreeMap;

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::hooks::{CommandHooksContext, HooksAction, HooksListSnapshot};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::model_setup::resolve_action;
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "hooks",
    aliases: &["hook", "gouzi"],
    usage: "/hooks [list|events|edit|review|approve <digest>|revoke]",
    description_key: "cmd_hooks_description",
};

pub struct HooksCmd;

impl<A: TryFrom<HooksAction>> RegisterCommand<CommandResult<A>> for HooksCmd
where
    A::Error: std::fmt::Display,
{
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::HOOKS,
            handler: hooks,
        }
    }
}

fn hooks<A: TryFrom<HooksAction>>(
    mut contexts: CommandContexts<'_>,
    arg: Option<&str>,
) -> CommandResult<A>
where
    A::Error: std::fmt::Display,
{
    if arg.is_none_or(|value| value.trim().is_empty()) {
        return resolve_action(HooksAction::OpenExtensionsTab, None);
    }
    let sub = arg.map(str::trim).unwrap_or("list").to_ascii_lowercase();
    if let Some(digest) = sub.strip_prefix("approve ") {
        let Some(ctx) = contexts.take_hooks() else {
            return CommandResult::error("Command capability unavailable: hooks");
        };
        return match ctx.approve_project_hooks(digest.trim()) {
            Ok(()) => CommandResult::message(
                "Approved these exact project hooks. They will load on your next session. Changes require another review.",
            ),
            Err(error) => CommandResult::error(error),
        };
    }
    match sub.as_str() {
        "" | "list" | "ls" | "show" => {
            let Some(ctx) = contexts.take_hooks() else {
                return CommandResult::error("Command capability unavailable: hooks");
            };
            render_list(&ctx.list_hooks())
        }
        "events" | "event" | "list-events" => events(),
        "review" => {
            let Some(ctx) = contexts.take_hooks() else {
                return CommandResult::error("Command capability unavailable: hooks");
            };
            match ctx.review_project_hooks() {
                Ok(review) => CommandResult::message(format!(
                    "Project hooks can run shell commands, including scripts they reference. Review the file and those scripts before approval.\n\n{}\n\nTo approve these exact hooks: /hooks approve {}",
                    review.sanitized_contents, review.digest
                )),
                Err(error) => CommandResult::error(error),
            }
        }
        "revoke" => {
            let Some(ctx) = contexts.take_hooks() else {
                return CommandResult::error("Command capability unavailable: hooks");
            };
            match ctx.revoke_project_hooks() {
                Ok(()) => CommandResult::message(
                    "Project hook approval revoked. Pending hooks will be refused; already running commands are unaffected.",
                ),
                Err(_) => CommandResult::error("Could not revoke project hook approval"),
            }
        }
        "edit" | "add" | "new" => resolve_action(HooksAction::EditProjectHooks, None),
        other => CommandResult::error(format!(
            "unknown subcommand `{other}`. Try `/hooks list`, `/hooks events`, `/hooks edit`, `/hooks review`, `/hooks approve <digest>`, or `/hooks revoke`."
        )),
    }
}

pub fn events<A>() -> CommandResult<A> {
    let mut out = String::new();
    out.push_str(
        "Available hook events (use one of these as `event = \"...\"` in your `[[hooks.hooks]]` entry).\n\
         Hooks are a TUI runtime feature: `codewhale exec`, the CLI dispatcher, the app-server,\n\
         and the workflow tool do not fire them.\n\n",
    );
    let ordered = [
        ("session_start", "fires once when the TUI launches"),
        ("session_end", "fires once on graceful shutdown"),
        ("turn_end", "fires after a turn completes (observer-only)"),
        (
            "message_submit",
            "fires before model dispatch; can transform or block submitted text",
        ),
        (
            "tool_call_before",
            "fires before each tool call; can allow/deny/ask, rewrite input, add context",
        ),
        (
            "tool_call_after",
            "fires after each tool call (observer-only)",
        ),
        ("mode_change", "fires on Plan/Work transitions"),
        ("on_error", "fires on transport / capacity / tool errors"),
        (
            "subagent_spawn",
            "fires when a sub-agent starts (observer-only)",
        ),
        (
            "subagent_complete",
            "fires when a sub-agent completes, fails, or is cancelled (observer-only)",
        ),
        (
            "shell_env",
            "fires before each exec_shell; stdout KEY=VALUE lines are merged into its environment",
        ),
        (
            "session_idle",
            "fires when the session settles back to idle after a turn or a wait (observer-only)",
        ),
        (
            "session_error",
            "fires when a turn ends in a terminal failure; absorbed tool failures never fire it (observer-only)",
        ),
        (
            "waiting_for_user",
            "fires when an approval prompt opens, a question is presented, or a goal continuation parks (observer-only)",
        ),
        ("session_busy", "idle / waiting → in_progress"),
    ];
    for (event, desc) in ordered {
        out.push_str(&format!("  - `{event}` — {desc}\n"));
    }
    out.push_str(
        "\nOnly `message_submit`, `tool_call_before`, and `shell_env` can steer a turn.\n\
         Observer-only means the *result* is ignored — the command still runs\n\
         and can have any external side effect.\n\n\
         Full contract: docs/HOOKS.md\n",
    );
    CommandResult::message(out.trim_end().to_string())
}

fn render_list<A>(snapshot: &HooksListSnapshot) -> CommandResult<A> {
    if snapshot.hooks.is_empty() && snapshot.problem_summaries.is_empty() {
        return CommandResult::message(
            "No hooks configured. Add a `[[hooks.hooks]]` entry to `~/.codewhale/config.toml` to define one.",
        );
    }
    if snapshot.hooks.is_empty() {
        let mut out =
            String::from("No runnable hooks. Every configured entry was rejected at load:\n\n");
        for problem in &snapshot.problem_summaries {
            out.push_str(&format!("  - {problem}\n"));
        }
        return CommandResult::message(out.trim_end().to_string());
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{} configured hook(s) (global enabled: {}):\n\n",
        snapshot.hooks.len(),
        if snapshot.enabled {
            "yes"
        } else {
            "no — all hooks suppressed"
        }
    ));

    let mut by_event: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    for hook in &snapshot.hooks {
        by_event
            .entry(hook.event_label.as_str())
            .or_default()
            .push(hook);
    }

    for (event, hooks) in by_event {
        out.push_str(&format!("### {event}\n"));
        for hook in hooks {
            let bg = if hook.background && hook.honors_background {
                " [bg, submitted not awaited]"
            } else {
                ""
            };
            let condition = hook
                .condition_summary
                .as_deref()
                .map(|c| format!(" if {c}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  - {}{bg} (timeout {}){condition}\n      $ {}\n",
                hook.label, hook.timeout_display, hook.command_preview
            ));
        }
        out.push('\n');
    }

    if !snapshot.problem_summaries.is_empty() {
        out.push_str("### configuration problems\n");
        for problem in &snapshot.problem_summaries {
            out.push_str(&format!("  - {problem}\n"));
        }
        out.push('\n');
    }

    if !snapshot.enabled {
        out.push_str(
            "Hooks are globally disabled — set `[hooks].enabled = true` in `config.toml` to fire them.\n",
        );
    }

    CommandResult::message(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_command_contract::hooks::ProjectHooksReview;

    #[derive(Default)]
    struct MockHooksContext {
        snapshot: HooksListSnapshot,
        approved_digest: Option<String>,
        revoked: bool,
    }

    impl CommandHooksContext for MockHooksContext {
        fn list_hooks(&self) -> HooksListSnapshot {
            self.snapshot.clone()
        }
        fn review_project_hooks(&self) -> Result<ProjectHooksReview, String> {
            Ok(ProjectHooksReview {
                digest: "abc123".to_string(),
                sanitized_contents: "[[hooks]]".to_string(),
            })
        }
        fn approve_project_hooks(&mut self, digest: &str) -> Result<(), String> {
            self.approved_digest = Some(digest.to_string());
            Ok(())
        }
        fn revoke_project_hooks(&mut self) -> Result<(), String> {
            self.revoked = true;
            Ok(())
        }
    }

    #[test]
    fn bare_and_edit_hooks_dispatch_without_capability() {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = HooksCmd::handler()
        else {
            panic!("expected contextual handler");
        };
        assert_eq!(capabilities, CommandCapabilities::HOOKS);
        let res: CommandResult<HooksAction> = handler(CommandContexts::empty(), None);
        assert_eq!(res.action, Some(HooksAction::OpenExtensionsTab));
        let res: CommandResult<HooksAction> = handler(CommandContexts::empty(), Some("edit"));
        assert_eq!(res.action, Some(HooksAction::EditProjectHooks));
        let res: CommandResult<HooksAction> = handler(CommandContexts::empty(), Some("events"));
        assert!(!res.is_error);
        assert!(res.message.unwrap().contains("session_start"));
    }

    #[test]
    fn contextual_hooks_require_capability() {
        let CommandHandler::Contextual { handler, .. } = HooksCmd::handler() else {
            panic!("expected contextual handler");
        };
        let res: CommandResult<HooksAction> = handler(CommandContexts::empty(), Some("list"));
        assert!(res.is_error);
        assert_eq!(
            res.message.as_deref(),
            Some("Error: Command capability unavailable: hooks")
        );
    }

    #[test]
    fn review_approve_and_revoke_preserve_receipts() {
        let CommandHandler::Contextual { handler, .. } = HooksCmd::handler() else {
            panic!("expected contextual handler");
        };
        let mut ctx = MockHooksContext::default();
        let review: CommandResult<HooksAction> = handler(
            CommandContexts::empty().with_hooks(&mut ctx),
            Some("review"),
        );
        assert!(review.message.unwrap().contains("/hooks approve abc123"));

        let approve: CommandResult<HooksAction> = handler(
            CommandContexts::empty().with_hooks(&mut ctx),
            Some("approve abc123"),
        );
        assert!(
            approve
                .message
                .unwrap()
                .contains("Approved these exact project hooks")
        );
        assert_eq!(ctx.approved_digest.as_deref(), Some("abc123"));

        let revoke: CommandResult<HooksAction> = handler(
            CommandContexts::empty().with_hooks(&mut ctx),
            Some("revoke"),
        );
        assert!(
            revoke
                .message
                .unwrap()
                .contains("Project hook approval revoked")
        );
        assert!(ctx.revoked);
    }
}
