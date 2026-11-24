//! `/change` command — show a changelog entry, translated to the user's
//! locale when it is not English.
//!
//! Usage: `/change [version]`
//!
//! Uses the Codewhale changelog embedded at compile time. With no argument,
//! extracts the most recent section. With a version argument like `0.8.32`,
//! extracts that specific version's section. When the UI locale is not
//! English and the current session can reach a model, the command also fires a
//! `SendMessage` action that asks the model to translate the changelog into
//! the user's language.

use super::DebugAction;
use codewhale_command_contract::facets::{CommandPresentationContext, DebugChangeProjection};
use codewhale_command_contract::handler::{
    CommandCapabilities as Caps, CommandContexts, CommandHandler,
};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use super::CommandResult;

/// Maximum length of the changelog excerpt we'll show inline (characters).
/// If the changelog section exceeds this, we truncate and show a notice.
/// 4096 chars is large enough for most version entries.
const MAX_INLINE_CHANGELOG_CHARS: usize = 4096;
pub struct ChangeCmd;
impl RegisterCommand<CommandResult> for ChangeCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "change",
            aliases: &[],
            usage: "/change [version]",
            description_key: "cmd_change_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: Caps::DEBUG_CHANGE.union(Caps::PRESENTATION),
            handler: change,
        }
    }
}

pub(super) fn change(contexts: CommandContexts<'_>, version: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(change) = parts.debug_change.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_change");
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    match change_report(change.change_projection(), presentation, version) {
        Ok(result) => result,
        Err(error) => CommandResult::error(error),
    }
}

/// Execute the `/change` command.
///
/// If `version` is `None`, shows the latest non-empty version section.
/// If `version` is `Some(v)`, shows the section for that version.
fn change_report(
    projection: DebugChangeProjection,
    presentation: &dyn CommandPresentationContext,
    version: Option<&str>,
) -> Result<CommandResult, String> {
    let section = if let Some(ver) = version {
        let ver = ver.trim();
        if ver.is_empty() {
            extract_latest_changelog_section(projection.changelog)
        } else {
            extract_changelog_section_by_version(projection.changelog, ver)
        }
    } else {
        extract_latest_changelog_section(projection.changelog)
    };

    let latest_section = match section {
        Some(s) => s,
        None => {
            let msg = if let Some(ver) = version {
                let ver = ver.trim();
                if ver.is_empty() {
                    "Could not find a version section in the bundled Codewhale changelog. \
                     Expected a line starting with `## [`."
                        .to_string()
                } else {
                    format!("Could not find version \"{ver}\" in the bundled Codewhale changelog.")
                }
            } else {
                "Could not find a version section in the bundled Codewhale changelog. \
                 Expected a line starting with `## [`."
                    .to_string()
            };
            return Err(msg);
        }
    };

    let header = presentation.translate("cmd_change_header", &[])?;

    let prev_hint = if let Some(prev_ver) = previous_version_hint(projection.changelog, version) {
        let hint =
            presentation.translate("cmd_change_previous_version", &[("version", &prev_ver)])?;
        format!("\n\n{hint}")
    } else {
        String::new()
    };

    let section_text = inline_changelog_section(&latest_section);

    // If the user's locale is English, just display.
    // Otherwise, also ask the model to translate.
    Ok(if projection.is_english {
        CommandResult::message(format!(
            "{header}\n─────────────────────────────\n{section_text}{prev_hint}"
        ))
    } else if !projection.translation_available {
        let fallback = presentation.translate("cmd_change_translation_unavailable", &[])?;
        CommandResult::message(format!(
            "{header}\n\
─────────────────────────────\n\
{fallback}\n\n\
{section_text}{prev_hint}"
        ))
    } else {
        let queued = presentation.translate("cmd_change_translation_queued", &[])?;
        let display_text = format!(
            "{header}\n\
─────────────────────────────\n\
{queued}\n\n\
{section_text}{prev_hint}"
        );
        let translation_source = format!("{latest_section}{prev_hint}");
        let lang_name = projection.translation_target;

        let translation_prompt = format!(
            "Translate the following changelog into {lang_name}. \
             Keep all markdown formatting, version numbers, dates, \
             contributor names, and code references intact. \
             Output ONLY the translated changelog, no preamble or commentary.\n\n\
             {translation_source}"
        );

        CommandResult::with_message_and_action(
            display_text,
            DebugAction::SendMessage(translation_prompt),
        )
    })
}

pub fn inline_changelog_section(section: &str) -> String {
    let total_chars = section.chars().count();
    if total_chars <= MAX_INLINE_CHANGELOG_CHARS {
        return section.to_string();
    }

    let truncated: String = section.chars().take(MAX_INLINE_CHANGELOG_CHARS).collect();
    format!(
        "{truncated}\n\
\n\
[... {} characters omitted from the bundled Codewhale changelog]",
        total_chars - MAX_INLINE_CHANGELOG_CHARS
    )
}

/// Extract the latest version section from CHANGELOG.md content.
///
/// Looks for the first `## [version] - date` heading and returns all lines
/// from that heading up to the next `## [` heading (or end of file).
/// Leading and trailing whitespace is trimmed.
///
/// Skips empty sections (e.g. `## [Unreleased]` with no content) to find
/// the first section that actually has content.
pub fn extract_latest_changelog_section(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    // Find the first `## [` heading index
    let first_idx = {
        let mut idx = None;
        for (i, line) in lines.iter().enumerate() {
            if line.trim().starts_with("## [") {
                idx = Some(i);
                break;
            }
        }
        idx?
    };

    // Starting from `first_idx`, walk through headings until we find a
    // section with non-empty content.
    let mut pos = first_idx;
    loop {
        let end = lines
            .iter()
            .enumerate()
            .skip(pos + 1)
            .find(|(_, line)| line.trim().starts_with("## ["))
            .map_or(lines.len(), |(i, _)| i);

        if section_has_body_content(&lines[pos + 1..end]) {
            return Some(lines[pos..end].join("\n").trim().to_string());
        }

        // Empty section — try the next heading (if any)
        if end >= lines.len() {
            return None;
        }
        pos = end;
    }
}

/// Extract a specific version section from CHANGELOG.md content.
///
/// Looks for `## [<version>]` or `## [<version> - date]` and returns all
/// lines from that heading up to the next `## [` heading (or end of file).
pub fn extract_changelog_section_by_version(content: &str, version: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut start_idx: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("## [") {
            // Check if this heading matches the requested version.
            // Format: `## [0.8.32] - 2026-05-12` or `## [0.8.32]`
            let bracket_end = trimmed.find(']')?;
            let heading_ver = &trimmed[4..bracket_end]; // skip "## ["
            if heading_ver == version {
                start_idx = Some(i);
                break;
            }
        }
    }

    let start = start_idx?;

    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| line.trim().starts_with("## ["))
        .map_or(lines.len(), |(i, _)| i);

    if !section_has_body_content(&lines[start + 1..end]) {
        return None;
    }

    Some(lines[start..end].join("\n").trim().to_string())
}

/// Extract the version number of the section immediately preceding the latest
/// non-empty section in the changelog.
///
/// Walks past empty sections (e.g. `## [Unreleased]`) the same way
/// [`extract_latest_changelog_section`] does, then returns the version from
/// the next `## [version]` heading after the first contentful section.
pub fn extract_previous_version_number(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let first_idx = lines.iter().position(|l| l.trim().starts_with("## ["))?;

    let mut pos = first_idx;
    loop {
        let end = lines
            .iter()
            .enumerate()
            .skip(pos + 1)
            .find(|(_, l)| l.trim().starts_with("## ["))
            .map_or(lines.len(), |(i, _)| i);

        if section_has_body_content(&lines[pos + 1..end]) {
            // Found the latest contentful section heading at `pos`.
            return next_contentful_version_after(&lines, end);
        }

        if end >= lines.len() {
            return None;
        }
        pos = end;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangelogEntryCounts {
    pub features: usize,
    pub fixes: usize,
    pub other: usize,
}

pub fn count_changelog_entries(section: &str) -> ChangelogEntryCounts {
    const UNCOUNTED: [&str; 5] = [
        "contributors",
        "thanks",
        "community",
        "known ",
        "retroactive",
    ];
    let mut counts = ChangelogEntryCounts::default();
    let mut slot: Option<usize> = None;
    for line in section.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            let heading = heading.trim().to_ascii_lowercase();
            slot = if heading.starts_with("added") || heading.starts_with("new") {
                Some(0)
            } else if heading.starts_with("fixed")
                || heading.starts_with("security")
                || heading.contains("fixes")
            {
                Some(1)
            } else if UNCOUNTED.iter().any(|prefix| heading.starts_with(prefix)) {
                None
            } else {
                Some(2)
            };
        } else if line.starts_with("## ") {
            slot = None;
        } else if line.starts_with("- ") {
            match slot {
                Some(0) => counts.features += 1,
                Some(1) => counts.fixes += 1,
                Some(_) => counts.other += 1,
                None => {}
            }
        }
    }
    counts
}

pub fn section_has_body_content(lines: &[&str]) -> bool {
    lines.iter().any(|line| !line.trim().is_empty())
}

pub fn previous_version_hint(content: &str, version: Option<&str>) -> Option<String> {
    match version.map(str::trim).filter(|v| !v.is_empty()) {
        Some(version) => extract_previous_version_number_after_version(content, version),
        None => extract_previous_version_number(content),
    }
}

pub fn extract_previous_version_number_after_version(
    content: &str,
    version: &str,
) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let current_start = lines.iter().position(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("## [")
            .and_then(|rest| rest.split_once(']'))
            .is_some_and(|(heading_ver, _)| heading_ver == version)
    })?;

    let current_end = lines
        .iter()
        .enumerate()
        .skip(current_start + 1)
        .find(|(_, line)| line.trim().starts_with("## ["))
        .map_or(lines.len(), |(i, _)| i);

    next_contentful_version_after(&lines, current_end)
}

pub fn next_contentful_version_after(lines: &[&str], mut pos: usize) -> Option<String> {
    while pos < lines.len() {
        let heading = lines[pos].trim();
        if !heading.starts_with("## [") {
            pos += 1;
            continue;
        }

        let end = lines
            .iter()
            .enumerate()
            .skip(pos + 1)
            .find(|(_, line)| line.trim().starts_with("## ["))
            .map_or(lines.len(), |(i, _)| i);

        if section_has_body_content(&lines[pos + 1..end]) {
            let bracket_end = heading.find(']')?;
            return Some(heading[4..bracket_end].to_string());
        }

        pos = end;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_changelog_section_uses_char_count_for_threshold_and_omission() {
        let within_char_budget = format!("{}{}", "a".repeat(4080), "中".repeat(10));
        assert!(within_char_budget.len() > MAX_INLINE_CHANGELOG_CHARS);
        assert_eq!(
            inline_changelog_section(&within_char_budget),
            within_char_budget
        );

        let over_char_budget = "中".repeat(4200);
        let output = inline_changelog_section(&over_char_budget);
        assert!(output.starts_with(&"中".repeat(MAX_INLINE_CHANGELOG_CHARS)));
        assert!(
            output.ends_with("[... 104 characters omitted from the bundled Codewhale changelog]"),
            "unexpected omission notice: {output}"
        );
    }
}
