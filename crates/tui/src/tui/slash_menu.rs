//! Slash-command autocomplete + popup-menu helpers.
//!
//! Extracted from `tui/ui.rs` (P1.2). The on-screen popup itself is rendered
//! by the composer widget; these helpers source the entries, apply a
//! selection, and handle Tab-completion when the popup isn't open.
//!
//! Intentionally separate from `tui::file_mention` even though both surface
//! a similar popup — the trigger characters, ranking, and post-selection
//! behaviour differ enough to keep them apart.

use crate::commands;

use super::app::{App, looks_like_slash_command_input};
use super::model_picker::provider_scoped_model_completion_ids;
use super::widgets::SlashMenuEntry;
use super::widgets::slash_completion_hints_for_plugins;

/// Return the slash-menu entries the composer should display, honouring
/// `slash_menu_hidden` (set when the user dismisses the popup with Esc).
pub fn visible_slash_menu_entries(app: &App, limit: usize) -> Vec<SlashMenuEntry> {
    if app.slash_menu_hidden {
        return Vec::new();
    }
    if let Some((byte_start, partial)) =
        partial_inline_skill_mention_at_cursor(&app.input, app.cursor_position)
    {
        let trigger = app.input[byte_start..].chars().next().unwrap_or('/');
        return skill_mention_entries(&partial, trigger, limit, &app.cached_skills);
    }
    if !looks_like_slash_command_input(&app.input) {
        return Vec::new();
    }
    // Building the cross-provider model inventory is unnecessary while the
    // user is merely typing `/model`; command-name completion needs no model
    // rows. Only pay that cost once an argument prefix exists.
    let trimmed = app.input.trim_start();
    let needs_model_candidates = trimmed
        .strip_prefix("/model")
        .is_some_and(|rest| rest.starts_with(char::is_whitespace));
    let model_candidates = if needs_model_candidates {
        provider_scoped_model_completion_ids(app)
    } else {
        Vec::new()
    };
    // Effort completions are per-model: only show levels the current model supports
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("/effort ") || lower.starts_with("/thinking ") {
        let arg_prefix = if lower.starts_with("/effort ") {
            trimmed[8..].trim_start()
        } else {
            trimmed[10..].trim_start()
        };
        let provider = app.api_provider;
        let base_url = app.active_route_base_url.clone();
        let wire_model = app.model.clone();
        let available = crate::tui::model_picker::picker_efforts_for_route(
            provider,
            &base_url,
            &wire_model,
            app.auto_model,
        );
        let mut effort_entries: Vec<SlashMenuEntry> = Vec::new();
        for eff in available {
            let label = eff.display_label_for_provider(provider).to_string();
            if label
                .to_ascii_lowercase()
                .starts_with(&arg_prefix.to_ascii_lowercase())
                || arg_prefix.is_empty()
            {
                effort_entries.push(SlashMenuEntry {
                    name: format!("/effort {}", label),
                    description: match eff {
                        codewhale_engine::reasoning_preference::ReasoningEffort::Auto => {
                            "choose per turn".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::Off => {
                            "no extra reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::Minimal => {
                            "minimal reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::Low => {
                            "lighter reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::Medium => {
                            "balanced reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::High => {
                            "deeper reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::XHigh => {
                            "extra-high reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::Ultra => {
                            "ultra reasoning".into()
                        }
                        codewhale_engine::reasoning_preference::ReasoningEffort::Max => {
                            "maximum reasoning".into()
                        }
                    },
                    is_skill: false,
                    alias_hint: None,
                });
            }
        }
        if !effort_entries.is_empty() {
            return effort_entries.into_iter().take(limit).collect();
        }
    }
    slash_completion_hints_for_plugins(
        &app.input,
        limit,
        &app.cached_skills,
        app.ui_locale,
        Some(&app.workspace),
        &model_candidates,
        Some(app.extension_plugin_view().as_ref()),
    )
}

/// Apply the currently-selected slash menu entry to the composer input.
/// Optionally appends a trailing space when the command takes arguments
/// so the user can type the rest without an extra keystroke.
pub fn apply_slash_menu_selection(
    app: &mut App,
    entries: &[SlashMenuEntry],
    append_space: bool,
) -> bool {
    if entries.is_empty() {
        return false;
    }

    let selected_idx = app.slash_menu_selected.min(entries.len().saturating_sub(1));
    let selected = &entries[selected_idx];

    if selected.is_skill
        && let Some((byte_start, partial)) =
            partial_inline_skill_mention_at_cursor(&app.input, app.cursor_position)
        && let Some(skill_name) = skill_name_from_menu_entry(selected)
    {
        let trigger = app.input[byte_start..].chars().next().unwrap_or('/');
        replace_inline_skill_mention(app, byte_start, trigger, &partial, &skill_name);
        app.slash_menu_hidden = false;
        app.status_message = Some(format!("Skill selected: {trigger}{skill_name}"));
        return true;
    }

    let mut command = selected.name.clone();

    let command_key = command.trim_start_matches('/');
    let user_takes_arguments = commands::user_registry::with_registry_for_app(app, |registry| {
        registry
            .get(command_key)
            .map(|metadata| metadata.takes_arguments())
    });
    let takes_arguments = user_takes_arguments.unwrap_or_else(|| {
        commands::get_command_info(command_key)
            .is_some_and(|info| info.composer_wants_trailing_space())
    });

    if append_space
        && !command.ends_with(' ')
        && !command.contains(char::is_whitespace)
        && takes_arguments
    {
        command.push(' ');
    }

    app.input = command;
    app.cursor_position = app.input.chars().count();
    app.slash_menu_hidden = false;
    app.status_message = Some(format!("Command selected: {}", app.input.trim_end()));
    true
}

/// Return the `/<skill>` or `$<skill>` token under the cursor when it is used as
/// an inline mention inside a normal message. A `/` or `$` at the start of the
/// composer, even after leading whitespace, remains reserved for slash commands
/// (handled by `slash_completion_hints`).
pub(crate) fn partial_inline_skill_mention_at_cursor(
    input: &str,
    cursor_chars: usize,
) -> Option<(usize, String)> {
    if looks_like_slash_command_input(input) {
        return None;
    }

    let chars: Vec<char> = input.chars().collect();
    if cursor_chars > chars.len() {
        return None;
    }

    let mut start_chars = cursor_chars;
    while start_chars > 0 {
        let prev = chars[start_chars - 1];
        if prev == '/' || prev == '$' {
            start_chars -= 1;
            break;
        }
        if prev.is_whitespace() {
            return None;
        }
        start_chars -= 1;
    }

    if start_chars == cursor_chars {
        return None;
    }
    let trigger = *chars.get(start_chars)?;
    if trigger != '/' && trigger != '$' {
        return None;
    }
    if !is_inline_skill_mention_start(&chars, start_chars) {
        return None;
    }

    let byte_start: usize = chars[..start_chars].iter().map(|c| c.len_utf8()).sum();
    if input[..byte_start].trim().is_empty() {
        return None;
    }

    let mut end_chars = start_chars + 1;
    while end_chars < chars.len() && !chars[end_chars].is_whitespace() {
        end_chars += 1;
    }
    let partial: String = chars[start_chars + 1..end_chars].iter().collect();
    if partial.contains('/') || partial.contains('$') {
        return None;
    }

    Some((byte_start, partial))
}

fn is_inline_skill_mention_start(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    chars
        .get(idx.saturating_sub(1))
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '<' | '"' | '\''))
}

fn skill_mention_entries(
    partial: &str,
    trigger: char,
    limit: usize,
    cached_skills: &[(String, String)],
) -> Vec<SlashMenuEntry> {
    if limit == 0 {
        return Vec::new();
    }
    let partial_lower = partial.to_ascii_lowercase();
    let mut entries = cached_skills
        .iter()
        .filter(|(skill_name, _)| skill_name.to_ascii_lowercase().starts_with(&partial_lower))
        .map(|(skill_name, skill_desc)| SlashMenuEntry {
            name: format!("{trigger}{skill_name}"),
            description: skill_desc.clone(),
            is_skill: true,
            alias_hint: None,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.dedup_by(|a, b| a.name == b.name);
    entries.into_iter().take(limit).collect()
}

fn skill_name_from_menu_entry(entry: &SlashMenuEntry) -> Option<String> {
    if !entry.is_skill {
        return None;
    }
    if let Some(name) = entry.name.strip_prefix("/skill ") {
        return Some(name.trim().to_string());
    }
    entry
        .name
        .strip_prefix('/')
        .or_else(|| entry.name.strip_prefix('$'))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

fn replace_inline_skill_mention(
    app: &mut App,
    byte_start: usize,
    trigger: char,
    partial: &str,
    skill_name: &str,
) {
    let original_token_len = trigger.len_utf8() + partial.len();
    let original_token_end = byte_start + original_token_len;
    let mut new_input =
        String::with_capacity(app.input.len() - original_token_len + 1 + skill_name.len());
    new_input.push_str(&app.input[..byte_start]);
    new_input.push(trigger);
    new_input.push_str(skill_name);
    if original_token_end < app.input.len() {
        new_input.push_str(&app.input[original_token_end..]);
    }
    let new_cursor_chars = app.input[..byte_start].chars().count() + 1 + skill_name.chars().count();
    app.input = new_input;
    app.cursor_position = new_cursor_chars;
}

/// Tab-completion for a slash-command-like input. Extends the input to the
/// longest unambiguous prefix; if exactly one command matches, completes it
/// fully (with trailing space). On ambiguity, posts a status hint listing
/// up to five candidates. Also considers skill names as completion candidates.
pub fn try_autocomplete_slash_command(app: &mut App) -> bool {
    if !looks_like_slash_command_input(&app.input) {
        return false;
    }

    let model_candidates = provider_scoped_model_completion_ids(app);
    let candidates = slash_completion_hints_for_plugins(
        &app.input,
        128,
        &app.cached_skills,
        app.ui_locale,
        Some(&app.workspace),
        &model_candidates,
        Some(app.extension_plugin_view().as_ref()),
    )
    .into_iter()
    .map(|entry| entry.name)
    .collect::<Vec<_>>();

    if candidates.is_empty() {
        return false;
    }

    let prefix = app.input.trim_start_matches('/');
    let refs: Vec<&str> = candidates
        .iter()
        .map(|name| name.trim_start_matches('/'))
        .collect();
    let shared = crate::tui::file_mention::longest_common_prefix(&refs);

    if !shared.is_empty() && shared.len() > prefix.len() {
        app.input = format!("/{shared}");
        app.cursor_position = app.input.chars().count();
        app.slash_menu_hidden = false;
        app.status_message = Some(format!("Autocomplete: /{shared}"));
        return true;
    }

    if candidates.len() == 1 {
        let mut completed = candidates[0].clone();
        if !completed.ends_with(' ') {
            completed.push(' ');
        }
        app.input = completed.clone();
        app.cursor_position = completed.chars().count();
        app.slash_menu_hidden = false;
        app.status_message = Some(format!("Command completed: {}", completed.trim_end()));
        return true;
    }

    let preview = candidates
        .iter()
        .take(5)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    app.status_message = Some(format!("Suggestions: {preview}"));
    true
}
