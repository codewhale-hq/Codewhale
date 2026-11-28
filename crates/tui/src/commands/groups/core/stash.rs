//! `/stash` slash command — list / pop parked composer drafts (#440).
//!
//! See `crates/tui/src/composer_stash.rs` for the on-disk format
//! and persistence rules. The slash command is the user-facing
//! surface; Ctrl+G (or Ctrl+S) in the composer is the corresponding push entry
//! point.

use codewhale_command_contract::facets::CommandComposerContext;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::CommandResult;

const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "stash",
    aliases: &["park"],
    usage: "/stash [list|pop|clear]",
    description_key: "cmd_stash_description",
};

pub(super) struct StashCmd;

impl<A> RegisterCommand<CommandResult<A>> for StashCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }
    fn handler() -> CommandHandler<CommandResult<A>> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::COMPOSER,
            handler: stash,
        }
    }
}

/// Top-level dispatch for `/stash`. Subcommands:
///
/// * `/stash`        — same as `/stash list`.
/// * `/stash list`   — show parked drafts, oldest first.
/// * `/stash pop`    — restore the most recently parked draft into
///   the composer; the popped entry is removed from disk.
/// * `/stash clear`  — wipe the entire stash file. Reports how many
///   entries were dropped so the user knows what they deleted.
fn stash<A>(mut contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult<A> {
    let Some(composer) = contexts.take_composer() else {
        return CommandResult::error("Command capability unavailable: composer");
    };
    let sub = arg.map(str::trim).unwrap_or("list").to_ascii_lowercase();
    match sub.as_str() {
        "" | "list" | "ls" | "show" => list(composer),
        "pop" | "restore" => pop(composer),
        "clear" | "wipe" | "drop" => clear(composer),
        other => CommandResult::error(format!(
            "unknown subcommand `{other}`. Try `/stash list`, `/stash pop`, or `/stash clear`."
        )),
    }
}

fn list<A>(composer: &dyn CommandComposerContext) -> CommandResult<A> {
    let entries = composer.parked_drafts();
    if entries.is_empty() {
        return CommandResult::message(
            "Stash empty. Press Ctrl+G (or Ctrl+S) in the composer to park the current draft.",
        );
    }
    let mut out = String::new();
    out.push_str(&format!("{} parked draft(s):\n\n", entries.len()));
    for (idx, entry) in entries.iter().enumerate() {
        out.push_str(&format_stash_line(idx, &entry.timestamp, &entry.text));
    }
    out.push_str("\nUse `/stash pop` to restore the most recent draft.");
    CommandResult::message(out)
}

fn clear<A>(composer: &mut dyn CommandComposerContext) -> CommandResult<A> {
    match composer.clear_parked() {
        Ok(0) => CommandResult::message("Stash already empty — nothing to clear."),
        Ok(n) => CommandResult::message(format!("Cleared {n} parked draft(s) from the stash.")),
        Err(err) => CommandResult::error(format!("Failed to clear stash: {err}")),
    }
}

fn pop<A>(composer: &mut dyn CommandComposerContext) -> CommandResult<A> {
    match composer.restore_parked() {
        Some((text, remaining)) => {
            let preview = preview_first_line(&text, 60);
            let suffix = match remaining {
                0 => " (stash now empty)".to_string(),
                1 => " (1 more parked)".to_string(),
                n => format!(" ({n} more parked)"),
            };
            CommandResult::message(format!("Restored stashed draft: {preview}{suffix}"))
        }
        None => CommandResult::message("Stash empty — nothing to pop."),
    }
}

/// One `/stash list` row. `idx` is the 0-based position; users see 1-based.
fn format_stash_line(idx: usize, ts: &str, text: &str) -> String {
    let ts = if ts.is_empty() { "(no ts)" } else { ts };
    let preview = preview_first_line(text, 80);
    format!("  {}. [{ts}] {preview}\n", idx + 1)
}

/// Take a one-line preview of `text`, capped at `max_chars`.
/// Multi-line drafts get a single-line summary so the listing
/// stays scannable.
fn preview_first_line(text: &str, max_chars: usize) -> String {
    let head = text.lines().next().unwrap_or("").trim();
    if head.chars().count() <= max_chars {
        return head.to_string();
    }
    let mut out: String = head.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stash_list_numbers_entries_from_one() {
        assert_eq!(
            format_stash_line(0, "2026-09-22T10:00:00Z", "first draft\nmore"),
            "  1. [2026-09-22T10:00:00Z] first draft\n"
        );
        assert_eq!(format_stash_line(2, "", "third"), "  3. [(no ts)] third\n");
    }

    #[test]
    fn preview_first_line_truncates_to_cap() {
        let body = "x".repeat(200);
        let p = preview_first_line(&body, 10);
        assert_eq!(p.chars().count(), 10);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn preview_first_line_keeps_short_input_intact() {
        assert_eq!(preview_first_line("short", 50), "short");
    }

    #[test]
    fn preview_first_line_only_uses_first_line_of_multiline() {
        let body = "first line of the draft\nsecond line that's longer\nthird";
        assert_eq!(preview_first_line(body, 80), "first line of the draft");
    }

    #[test]
    fn preview_first_line_handles_empty_input() {
        assert_eq!(preview_first_line("", 50), "");
        assert_eq!(preview_first_line("   ", 50), "");
    }
}
