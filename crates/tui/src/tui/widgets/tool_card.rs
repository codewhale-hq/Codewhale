//! Tool-card visual vocabulary for the v0.6.6 transcript redesign.
//!
//! Tool cards are the boxes that appear when the agent runs `read_file`,
//! `exec_shell`, `apply_patch`, etc. The visual vocabulary is intentionally
//! sparse: a single verb glyph identifies the family, a left rail anchors
//! the card to the timeline, and the spinner cadence reuses the existing
//! tool-status animation.
//!
//! This module owns:
//!
//! - [`ToolFamily`] — the canonical semantic families plus a `Generic`
//!   fallback for anything we don't have a family for yet.
//! - [`tool_family_for_title`] — maps the legacy `render_tool_header` title
//!   string (`"Shell"`, `"Patch"`, `"Workspace"`, etc.) to a family. Lets
//!   the existing call sites drop in family glyphs without re-architecting
//!   each cell.
//! - [`family_glyph`] / [`family_label`] — the verb glyph + label per
//!   family. Glyphs are single graphemes; labels are short verbs.
//! - [`CardRail`] / [`rail_glyph`] — the `╭ │ ╰` rail anchored to the
//!   left margin so the eye can group multi-line cards.
//!
//! The actual line composition still happens inside `history.rs`; this
//! module is the vocabulary, not the layout engine. Keeping it small means
//! a future visual refresh only has to touch the constants here.

use codewhale_localization::Locale;

/// Tool family — the verb the agent is performing. Used to pick a glyph
/// and label for the card header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFamily {
    /// Reads, listings, exploration. `▷ read`.
    Read,
    /// Edits, patches, writes. `◆ patch`.
    Patch,
    /// Shell, child processes. `▶ run`.
    Run,
    /// Grep, fuzzy file search, web search. `⌕ find`.
    Find,
    /// Single sub-agent dispatch. `◐ agent`.
    Delegate,
    /// Multi-agent fanout dispatch (rlm). `⋮⋮ fanout`.
    Fanout,
    /// Recursive language model work. `⋮⋮ rlm`.
    Rlm,
    /// Verification gates, tests, and validators. `✓ verify`.
    Verify,
    /// Reasoning / chain-of-thought. `… think`. Reasoning has its own
    /// render path (`render_thinking` in `history.rs`); the family is
    /// declared here for completeness so any future code that reaches for
    /// it has the matching glyph + label vocabulary.
    Think,
    /// Anything we don't have a family glyph for yet — falls back to a
    /// neutral bullet so the card still renders cleanly.
    Generic,
}

/// Map a legacy tool-header title string (the value passed to
/// `render_tool_header`) to a family. Anything unrecognised falls back to
/// [`ToolFamily::Generic`] so cards still render — they just lose the
/// verb-glyph treatment until the family is added here.
#[must_use]
pub fn tool_family_for_title(title: &str) -> ToolFamily {
    match title {
        "Shell" => ToolFamily::Run,
        "Patch" | "Diff" | "File" => ToolFamily::Patch,
        "Workspace" | "Image" => ToolFamily::Read,
        "Search" => ToolFamily::Find,
        "Plan" | "Legacy plan" | "Review" => ToolFamily::Generic,
        _ => ToolFamily::Generic,
    }
}

/// Map an arbitrary tool name (as exposed to the model — e.g. `read_file`,
/// `apply_patch`, `agent`) to a family. Used by `GenericToolCell`
/// where the `tool_family_for_title` shortcut isn't enough because every
/// generic cell shares the title `"Tool"`.
#[must_use]
pub fn tool_family_for_name(name: &str) -> ToolFamily {
    match name {
        "read_file" | "list_dir" | "view_image" | "git_status" | "git_diff" | "git_log"
        | "git_show" | "git_blame" | "git_commit_plan" => ToolFamily::Read,
        "edit_file" | "apply_patch" | "write_file" => ToolFamily::Patch,
        "exec_shell"
        | "exec_shell_wait"
        | "exec_shell_interact"
        | "exec_shell_cancel"
        | "task_shell_start"
        | "task_shell_wait"
        | "start_registry_mcp_server" => ToolFamily::Run,
        "grep_files" | "file_search" | "web_search" | "fetch_url" | "registry_sync" => {
            ToolFamily::Find
        }
        "agent" => ToolFamily::Delegate,
        "rlm_open" | "rlm_eval" | "rlm_configure" | "rlm_close" | "rlm" => ToolFamily::Rlm,
        "run_tests"
        | "run_verifiers"
        | "task_gate_run"
        | "validate_data"
        | "wait_for_dev_server" => ToolFamily::Verify,
        // Workflow runs are multi-child activity; reuse fanout glyph so the
        // compact history card (#4122) shares visual vocabulary with direct
        // multi-agent cards rather than the neutral generic bullet.
        "workflow" => ToolFamily::Fanout,
        _ => ToolFamily::Generic,
    }
}

/// Resolve an action-parameterized model tool before assigning its visual
/// family. Legacy names pass through unchanged.
#[cfg(test)]
#[must_use]
pub fn tool_family_for_call(name: &str, input: &serde_json::Value) -> ToolFamily {
    tool_family_for_name(crate::tools::canonical_action::canonical_action_alias(
        name, input,
    ))
}

/// User-facing label for an arbitrary tool name. Known tools collapse to the
/// semantic verb; unknown tools keep their exact name for debugging.
#[cfg(test)]
#[must_use]
fn tool_display_label_for_name(name: &str) -> String {
    let family = tool_family_for_name(name);
    if matches!(family, ToolFamily::Generic) {
        name.to_string()
    } else {
        family_label(family).to_string()
    }
}

fn family_message_id(family: ToolFamily) -> codewhale_localization::MessageId {
    match family {
        ToolFamily::Read => codewhale_localization::MessageId::ToolFamilyRead,
        ToolFamily::Patch => codewhale_localization::MessageId::ToolFamilyPatch,
        ToolFamily::Run => codewhale_localization::MessageId::ToolFamilyRun,
        ToolFamily::Find => codewhale_localization::MessageId::ToolFamilyFind,
        ToolFamily::Delegate => codewhale_localization::MessageId::ToolFamilyDelegate,
        ToolFamily::Fanout => codewhale_localization::MessageId::ToolFamilyFanout,
        ToolFamily::Rlm => codewhale_localization::MessageId::ToolFamilyRlm,
        ToolFamily::Verify => codewhale_localization::MessageId::ToolFamilyVerify,
        ToolFamily::Think => codewhale_localization::MessageId::ToolFamilyThink,
        ToolFamily::Generic => codewhale_localization::MessageId::ToolFamilyGeneric,
    }
}

/// Compact activity/status label for arbitrary tool names. Known built-ins use
/// the semantic verb; unknown tools keep the `tool NAME` form.
#[must_use]
pub fn tool_activity_label_for_name(name: &str, locale: Locale) -> String {
    let family = tool_family_for_name(name);
    let mid = family_message_id(family);
    if matches!(family, ToolFamily::Generic) {
        format!("{} {name}", codewhale_localization::tr(locale, mid))
    } else {
        codewhale_localization::tr(locale, mid).to_string()
    }
}

/// Build a compact semantic summary for a tool header from the public tool
/// name and the already-sanitized argument summary.
#[must_use]
pub fn tool_header_summary_for_name(name: &str, input_summary: Option<&str>) -> Option<String> {
    let family = tool_family_for_name(name);
    let summary = input_summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty());

    let preferred_keys = match family {
        ToolFamily::Read | ToolFamily::Patch => ["path", "file", "target", "content"].as_slice(),
        ToolFamily::Run => ["command", "cmd", "script"].as_slice(),
        ToolFamily::Find => ["query", "pattern", "path", "scope"].as_slice(),
        ToolFamily::Delegate | ToolFamily::Fanout | ToolFamily::Rlm => {
            ["prompt", "task", "model"].as_slice()
        }
        ToolFamily::Verify => ["profile", "level", "command", "args", "path"].as_slice(),
        ToolFamily::Think | ToolFamily::Generic => {
            ["query", "path", "command", "prompt"].as_slice()
        }
    };

    let selected_summary = summary.and_then(|summary| {
        for key in preferred_keys {
            if let Some(value) = summary_value(summary, key) {
                return Some(value);
            }
        }

        if summary_is_noisy_control_only(summary) {
            None
        } else {
            Some(summary.to_string())
        }
    });

    if should_show_tool_name_in_header(name, family) {
        let tool_name = name.trim();
        if tool_name.is_empty() {
            return selected_summary;
        }
        return Some(match selected_summary {
            Some(summary) if summary != tool_name => format!("{tool_name} · {summary}"),
            _ => tool_name.to_string(),
        });
    }

    selected_summary
}

fn summary_value(summary: &str, key: &str) -> Option<String> {
    for part in summary.split(", ") {
        let Some((part_key, value)) = part.split_once(':') else {
            continue;
        };
        if part_key.trim() == key {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub(crate) fn should_show_tool_name_in_header(name: &str, family: ToolFamily) -> bool {
    (matches!(family, ToolFamily::Generic) && !is_known_metadata_tool_name(name))
        || matches!(name, "git_log" | "git_show" | "git_blame")
}

fn is_known_metadata_tool_name(name: &str) -> bool {
    matches!(
        name,
        "update_plan"
            | "work_update"
            | "todo_write"
            | "todo_add"
            | "todo_update"
            | "checklist_write"
            | "checklist_add"
            | "checklist_update"
            | "checklist_list"
    )
}

fn summary_is_noisy_control_only(summary: &str) -> bool {
    let mut saw_control = false;
    for part in summary.split(", ") {
        let Some((key, value)) = part.split_once(':') else {
            return false;
        };
        if value.trim().is_empty() {
            continue;
        }
        if !is_noisy_summary_key(key.trim()) {
            return false;
        }
        saw_control = true;
    }
    saw_control
}

fn is_noisy_summary_key(key: &str) -> bool {
    matches!(
        key,
        "limit"
            | "max_count"
            | "max_output_tokens"
            | "offset"
            | "page"
            | "page_size"
            | "per_page"
            | "response_length"
            | "timeout_ms"
            | "yield_time_ms"
    )
}

/// The verb glyph for a family. Single grapheme so the header layout math
/// in `render_tool_header` stays simple (one cell wide).
#[must_use]
pub fn family_glyph(family: ToolFamily) -> &'static str {
    match family {
        ToolFamily::Read => "\u{25B7}",           // ▷
        ToolFamily::Patch => "\u{25C6}",          // ◆
        ToolFamily::Run => "\u{25B6}",            // ▶
        ToolFamily::Find => "\u{2315}",           // ⌕
        ToolFamily::Delegate => "\u{25D0}",       // ◐
        ToolFamily::Fanout => "\u{22EE}\u{22EE}", // ⋮⋮ (two cells)
        ToolFamily::Rlm => "\u{22EE}\u{22EE}",    // ⋮⋮ (two cells)
        ToolFamily::Verify => "\u{2713}",
        ToolFamily::Think => "\u{2026}",   // …
        ToolFamily::Generic => "\u{2022}", // •
    }
}

/// The short verb label for a family — appears in card headers next to the
/// glyph. Lowercased on purpose; the verb-glyph + label is the new card
/// title vocabulary.
#[must_use]
pub fn family_label(family: ToolFamily) -> &'static str {
    match family {
        ToolFamily::Read => "read",
        ToolFamily::Patch => "patch",
        ToolFamily::Run => "run",
        ToolFamily::Find => "find",
        ToolFamily::Delegate => "agent",
        ToolFamily::Fanout => "fanout",
        ToolFamily::Rlm => "rlm",
        ToolFamily::Verify => "verify",
        ToolFamily::Think => "think",
        ToolFamily::Generic => "tool",
    }
}

/// Position of a line within a multi-line card — drives the left-rail
/// glyph so the box reads as a contiguous group from top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardRail {
    /// First line of the card — the header. `╭`.
    Top,
    /// Any middle line — body content. `│`.
    Middle,
    /// Last line of the card. `╰`.
    Bottom,
    /// Single-line card — no rail at all.
    Single,
}

/// Map a [`CardRail`] position to its rail glyph. Returned as a `&str`
/// because callers paste it into a span.
#[must_use]
pub fn rail_glyph(rail: CardRail) -> &'static str {
    match rail {
        CardRail::Top => "\u{256D}",    // ╭
        CardRail::Middle => "\u{2502}", // │
        CardRail::Bottom => "\u{2570}", // ╰
        CardRail::Single => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CardRail, ToolFamily, family_glyph, family_label, rail_glyph, tool_activity_label_for_name,
        tool_display_label_for_name, tool_family_for_call, tool_family_for_name,
        tool_family_for_title, tool_header_summary_for_name,
    };
    use codewhale_localization::{Locale, MessageId, tr};
    use serde_json::json;

    #[test]
    fn legacy_titles_route_to_expected_families() {
        assert_eq!(tool_family_for_title("Shell"), ToolFamily::Run);
        assert_eq!(tool_family_for_title("Patch"), ToolFamily::Patch);
        assert_eq!(tool_family_for_title("Workspace"), ToolFamily::Read);
        assert_eq!(tool_family_for_title("Search"), ToolFamily::Find);
        assert_eq!(tool_family_for_title("Diff"), ToolFamily::Patch);
        assert_eq!(tool_family_for_title("Plan"), ToolFamily::Generic);
        assert_eq!(tool_family_for_title("Legacy plan"), ToolFamily::Generic);
        assert_eq!(tool_family_for_title("unknown title"), ToolFamily::Generic);
    }

    #[test]
    fn tool_names_route_to_families_by_verb() {
        assert_eq!(tool_family_for_name("read_file"), ToolFamily::Read);
        assert_eq!(tool_family_for_name("apply_patch"), ToolFamily::Patch);
        assert_eq!(tool_family_for_name("exec_shell"), ToolFamily::Run);
        assert_eq!(tool_family_for_name("task_shell_start"), ToolFamily::Run);
        assert_eq!(tool_family_for_name("grep_files"), ToolFamily::Find);
        assert_eq!(tool_family_for_name("git_log"), ToolFamily::Read);
        assert_eq!(tool_family_for_name("agent"), ToolFamily::Delegate);
        assert_eq!(tool_family_for_name("rlm_eval"), ToolFamily::Rlm);
        assert_eq!(tool_family_for_name("run_verifiers"), ToolFamily::Verify);
        assert_eq!(
            tool_family_for_name("wait_for_dev_server"),
            ToolFamily::Verify
        );
        assert_eq!(
            tool_family_for_name("totally_new_tool"),
            ToolFamily::Generic
        );
    }

    #[test]
    fn canonical_actions_route_to_the_same_families_as_legacy_aliases() {
        let cases = [
            ("Bash", "run", ToolFamily::Run),
            ("Bash", "wait", ToolFamily::Run),
            ("Bash", "interact", ToolFamily::Run),
            ("Bash", "cancel", ToolFamily::Run),
            ("File", "read", ToolFamily::Read),
            ("File", "list", ToolFamily::Read),
            ("File", "search_name", ToolFamily::Find),
            ("File", "search_content", ToolFamily::Find),
            ("File", "write", ToolFamily::Patch),
            ("File", "edit", ToolFamily::Patch),
            ("File", "patch", ToolFamily::Patch),
            ("Git", "status", ToolFamily::Read),
            ("Git", "diff", ToolFamily::Read),
            ("Git", "log", ToolFamily::Read),
            ("Git", "show", ToolFamily::Read),
            ("Git", "blame", ToolFamily::Read),
            ("Git", "commit_plan", ToolFamily::Read),
            ("Run", "tests", ToolFamily::Verify),
            ("Run", "verifiers", ToolFamily::Verify),
            ("Web", "search", ToolFamily::Find),
            ("Web", "fetch", ToolFamily::Find),
            ("Web", "wait", ToolFamily::Verify),
        ];

        for (family, action, expected) in cases {
            assert_eq!(
                tool_family_for_call(family, &json!({"action": action})),
                expected,
                "{family}.{action}"
            );
        }
    }

    #[test]
    fn tool_display_label_collapses_known_tools_to_user_verbs() {
        assert_eq!(tool_display_label_for_name("exec_shell"), "run");
        assert_eq!(tool_display_label_for_name("run_verifiers"), "verify");
        assert_eq!(tool_display_label_for_name("file_search"), "find");
        assert_eq!(
            tool_display_label_for_name("future_private_tool"),
            "future_private_tool"
        );

        assert_eq!(
            tool_activity_label_for_name("exec_shell", Locale::En),
            "run"
        );
        assert_eq!(
            tool_activity_label_for_name("run_verifiers", Locale::En),
            "verify"
        );
        assert_eq!(
            tool_activity_label_for_name("future_private_tool", Locale::En),
            "tool future_private_tool"
        );
    }

    #[test]
    fn tool_header_summary_prefers_family_specific_arguments() {
        assert_eq!(
            tool_header_summary_for_name("read_file", Some("path: src/main.rs, limit: 20"))
                .as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            tool_header_summary_for_name("exec_shell", Some("command: cargo test, cwd: /repo"))
                .as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            tool_header_summary_for_name("grep_files", Some("pattern: TODO, path: crates"))
                .as_deref(),
            Some("TODO")
        );
        assert_eq!(
            tool_header_summary_for_name("run_verifiers", Some("profile: auto, level: quick"))
                .as_deref(),
            Some("auto")
        );
        assert_eq!(
            tool_header_summary_for_name("unknown", Some("alpha: beta")).as_deref(),
            Some("unknown · alpha: beta")
        );
        assert_eq!(
            tool_header_summary_for_name("git_log", Some("max_count: 15")).as_deref(),
            Some("git_log")
        );
        assert_eq!(
            tool_header_summary_for_name("future_private_tool", Some("max_count: 15")).as_deref(),
            Some("future_private_tool")
        );
        assert_eq!(
            tool_header_summary_for_name("future_private_tool", None).as_deref(),
            Some("future_private_tool")
        );
        assert_eq!(
            tool_header_summary_for_name("todo_write", Some("items: <2 items>")).as_deref(),
            Some("items: <2 items>")
        );
    }

    #[test]
    fn each_family_has_a_glyph_and_label() {
        // Smoke test — surface accidental empties from a future refactor.
        for family in [
            ToolFamily::Read,
            ToolFamily::Patch,
            ToolFamily::Run,
            ToolFamily::Find,
            ToolFamily::Delegate,
            ToolFamily::Fanout,
            ToolFamily::Rlm,
            ToolFamily::Verify,
            ToolFamily::Think,
            ToolFamily::Generic,
        ] {
            assert!(
                !family_glyph(family).is_empty(),
                "family {family:?} has empty glyph",
            );
            assert!(
                !family_label(family).is_empty(),
                "family {family:?} has empty label",
            );
        }
    }

    #[test]
    fn card_rail_glyphs_form_a_box() {
        assert_eq!(rail_glyph(CardRail::Top), "\u{256D}");
        assert_eq!(rail_glyph(CardRail::Middle), "\u{2502}");
        assert_eq!(rail_glyph(CardRail::Bottom), "\u{2570}");
        assert!(rail_glyph(CardRail::Single).is_empty());
    }

    #[test]
    fn tool_family_labels_localized_no_english_leak() {
        let checks: &[(MessageId, &str, &str)] = &[
            (MessageId::ToolFamilyRead, "read", "đọc,读,読,读取,ler,leer"),
            (
                MessageId::ToolFamilyPatch,
                "patch",
                "vá,補,パ,修补,corrigir,parchear",
            ),
            (
                MessageId::ToolFamilyRun,
                "run",
                "chạy,執,実,运行,executar,ejecutar",
            ),
            (
                MessageId::ToolFamilyFind,
                "find",
                "tìm,搜,検,搜索,buscar,buscar",
            ),
            (
                MessageId::ToolFamilyDelegate,
                "agent",
                "ủy,委,委,委,agente,agente",
            ),
            (
                MessageId::ToolFamilyVerify,
                "verify",
                "xác minh,驗,検,验,verificar,verificar",
            ),
            (
                MessageId::ToolFamilyThink,
                "think",
                "suy nghĩ,思,思,思,pensar,pensar",
            ),
            (
                MessageId::ToolFamilyGeneric,
                "tool",
                "công cụ,工具,ツール,工具,ferramenta,herramienta",
            ),
        ];
        for locale in [
            Locale::Ja,
            Locale::ZhHans,
            Locale::ZhHant,
            Locale::PtBr,
            Locale::Es419,
            Locale::Vi,
            Locale::Ca,
            Locale::De,
            Locale::Fr,
            Locale::Id,
            Locale::Hi,
            Locale::Ru,
            Locale::Uk,
        ] {
            for (id, eng, _) in checks {
                if *id == MessageId::ToolFamilyDelegate
                    && matches!(locale, Locale::Ca | Locale::De | Locale::Fr)
                {
                    continue;
                }
                let msg = tr(locale, *id);
                assert!(
                    !msg.eq_ignore_ascii_case(eng),
                    "{} leaked exact English '{}' for '{:?}': {msg}",
                    locale.tag(),
                    eng,
                    id
                );
            }
        }
    }

    #[test]
    fn tool_family_activity_label_localized_no_english_leak() {
        let known = [
            "exec_shell",
            "read_file",
            "apply_patch",
            "grep_files",
            "run_verifiers",
        ];
        let english_labels = ["run", "read", "patch", "find", "verify"];
        for locale in [
            Locale::Ja,
            Locale::ZhHans,
            Locale::ZhHant,
            Locale::PtBr,
            Locale::Es419,
            Locale::Vi,
            Locale::Ca,
            Locale::De,
            Locale::Fr,
            Locale::Id,
            Locale::Hi,
            Locale::Ru,
            Locale::Uk,
        ] {
            for (tool, eng) in known.iter().zip(english_labels.iter()) {
                let label = tool_activity_label_for_name(tool, locale);
                assert!(
                    !label.eq_ignore_ascii_case(eng),
                    "{} leaked English '{}' for tool '{tool}': {label}",
                    locale.tag(),
                    eng,
                );
            }
        }
    }
}
