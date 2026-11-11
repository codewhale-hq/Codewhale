//! Shared pure receipt rendering, moved from `crate::receipts`.
//! All data classification/redaction and I/O remain in the authoritative builder.

use codewhale_command_contract::facets::debug_receipts::*;

pub fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// One line of totals, verbs first: `Changed 4 files · ran 7 commands · 2
/// approved by you · 9 ran without asking under Full Access · 1 denied by
/// you`.
#[must_use]
pub fn totals_line(receipt: &Receipt) -> String {
    let totals = &receipt.totals;
    let mut parts: Vec<String> = Vec::new();
    if totals.files_changed > 0 {
        let mut part = format!("changed {}", plural(totals.files_changed, "file", "files"));
        if totals.line_counts_complete {
            part.push_str(&format!(
                " (+{} −{})",
                totals.lines_added, totals.lines_removed
            ));
        }
        if totals.files_changed_outside_file_tools > 0 {
            part.push_str(&format!(
                ", {} outside file tools",
                totals.files_changed_outside_file_tools
            ));
        }
        parts.push(part);
    }
    if totals.commands > 0 {
        let mut part = format!("ran {}", plural(totals.commands, "command", "commands"));
        if totals.commands_failed > 0 {
            part.push_str(&format!(" ({} failed)", totals.commands_failed));
        }
        parts.push(part);
    }
    if totals.code_runs > 0 {
        parts.push(format!(
            "ran code {}",
            plural(totals.code_runs, "time", "times")
        ));
    }
    if totals.network > 0 {
        parts.push(format!(
            "made {}",
            plural(totals.network, "web request", "web requests")
        ));
    }
    if totals.mcp_calls > 0 {
        parts.push(format!(
            "made {}",
            plural(totals.mcp_calls, "MCP call", "MCP calls")
        ));
    }
    if totals.subagents > 0 {
        parts.push(format!(
            "started {}",
            plural(totals.subagents, "agent", "agents")
        ));
    }
    let approvals = &totals.approvals;
    let by_decider = |verb: &str, by: &DeciderCounts| -> Vec<String> {
        [
            (by.you, "by you"),
            (by.session_rule, "by session rule"),
            (by.posture, "by posture"),
            (by.not_recorded, "(decider not recorded)"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, who)| format!("{count} {verb} {who}"))
        .collect()
    };
    parts.extend(by_decider("approved", &approvals.approved_by));
    if totals.ran_without_asking > 0 {
        let mut part = format!("{} ran without asking", totals.ran_without_asking);
        if !receipt.postures.is_empty() {
            part.push_str(&format!(" under {}", receipt.postures.join(" and ")));
        }
        parts.push(part);
    }
    parts.extend(by_decider("denied", &approvals.denied_by));
    if approvals.timed_out > 0 {
        parts.push(format!("{} timed out", approvals.timed_out));
    }
    if approvals.not_answered > 0 {
        parts.push(format!("{} not answered", approvals.not_answered));
    }
    if approvals.pending > 0 {
        parts.push(format!("{} waiting", approvals.pending));
    }
    if totals.blocked > 0 {
        parts.push(format!("{} blocked before running", totals.blocked));
    }
    let failures_beyond_commands = totals.failures.saturating_sub(totals.commands_failed);
    if failures_beyond_commands > 0 {
        parts.push(plural(
            failures_beyond_commands,
            "other failure",
            "other failures",
        ));
    }
    if parts.is_empty() {
        return if totals.other_tool_calls > 0 {
            format!(
                "Only read or looked things up ({}).",
                plural(totals.other_tool_calls, "call", "calls")
            )
        } else {
            "No actions recorded.".to_string()
        };
    }
    let mut line = parts.join(" · ");
    if let Some(first) = line.get(0..1) {
        line = first.to_uppercase() + &line[1..];
    }
    line
}

pub fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

fn decider_label(decider: ApprovalDecider) -> &'static str {
    match decider {
        ApprovalDecider::User => "you",
        ApprovalDecider::SessionRule => "session rule",
        ApprovalDecider::Posture => "posture",
        ApprovalDecider::Host => "Codewhale",
    }
}

fn approval_phrase(fact: &ApprovalFact) -> String {
    let by = fact
        .decided_by
        .map(|by| format!(" by {}", decider_label(by)))
        .unwrap_or_default();
    match fact.decision {
        ApprovalDecisionLabel::Approved => format!("approved{by}"),
        ApprovalDecisionLabel::ApprovedWithPolicy => format!("approved{by} with a wider sandbox"),
        ApprovalDecisionLabel::Denied => format!("denied{by}"),
        ApprovalDecisionLabel::TimedOut => "approval timed out".to_string(),
        ApprovalDecisionLabel::Cancelled => "turn stopped while waiting".to_string(),
        ApprovalDecisionLabel::Unavailable => "nobody could be asked".to_string(),
        ApprovalDecisionLabel::Pending => "waiting for approval".to_string(),
    }
}

fn counts(added: Option<u64>, removed: Option<u64>) -> String {
    match (added, removed) {
        (Some(added), Some(removed)) => format!(" (+{added} −{removed})"),
        _ => String::new(),
    }
}

fn file_phrase(file: &FileTouch) -> String {
    let verb = match file.change {
        FileChangeKind::Edited => "edited",
        FileChangeKind::Created => "created",
        FileChangeKind::Deleted => "deleted",
        FileChangeKind::Written => "wrote",
    };
    let counts = if file.change == FileChangeKind::Deleted {
        String::new()
    } else {
        counts(file.lines_added, file.lines_removed)
    };
    format!("{verb} {}{counts}", code_span(&file.path))
}

/// `text` on one line with nothing a terminal acts on: control characters
/// (newline, carriage return, escape), Unicode line separators, and bidi
/// overrides show as `\u{…}`-style escapes. Paths, commands, and error text
/// come from the workspace and from tools, so a file a command named
/// `x\n- Ran …` cannot forge a receipt line, and one holding `ESC ]` cannot
/// drive the terminal that prints the receipt.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let hidden = ch.is_control()
            || matches!(
                ch,
                '\u{2028}'
                    | '\u{2029}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            );
        if hidden {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Inline code that survives backticks in the text: the fence is one
/// backtick longer than the longest run inside it.
pub fn code_span(text: &str) -> String {
    let longest = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// What the action did (or would have done), past tense and verb first.
/// Every phrase starts with one of [`PHRASE_VERBS`], so a call that did not
/// run can say so in the same words.
fn action_phrase(action: &ReceiptAction) -> String {
    match &action.what {
        ActionKind::FileChange { files } => match files.as_slice() {
            [file] => file_phrase(file),
            files if action.status == ActionStatus::Ok => format!(
                "changed {} files: {}",
                files.len(),
                files.iter().map(file_phrase).collect::<Vec<_>>().join(", ")
            ),
            files => format!(
                "changed {} files: {}",
                files.len(),
                files
                    .iter()
                    .map(|file| code_span(&file.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        ActionKind::WorkspaceChange { files, truncated } => {
            let mut text = format!(
                "changed outside file tools (a command or another process): {}",
                files.iter().map(file_phrase).collect::<Vec<_>>().join(", ")
            );
            if *truncated {
                text.push_str(", and more");
            }
            text
        }
        ActionKind::Command {
            command,
            cwd,
            exit_code,
        } => {
            let mut text = format!("ran {}", code_span(command));
            if let Some(cwd) = cwd {
                text.push_str(&format!(" in {cwd}"));
            }
            if let Some(code) = exit_code {
                text.push_str(&format!(" — exit {code}"));
            }
            text
        }
        ActionKind::Code { exit_code, nested } => {
            let mut text = format!("ran code ({})", action.tool);
            if let Some(code) = exit_code {
                text.push_str(&format!(" — exit {code}"));
            }
            if !nested.is_empty() {
                let calls = nested
                    .iter()
                    .map(|call| format!("{}{}", call.tool, if call.ok { "" } else { " ✗" }))
                    .collect::<Vec<_>>()
                    .join(", ");
                text.push_str(&format!(" — called {calls}"));
            }
            text
        }
        ActionKind::Network {
            action: kind,
            host,
            query,
        } => match (kind.as_str(), host, query) {
            ("search", _, Some(query)) => format!("searched the web for “{query}”"),
            ("search", _, None) => "searched the web".to_string(),
            ("fetch", Some(host), _) => format!("fetched {host}"),
            ("git_fetch", Some(remote), _) => format!("fetched git remote {remote}"),
            (other, Some(host), _) => format!("called {host}: {}", other.replace('_', " ")),
            (other, None, _) => format!("made a web request ({other})"),
        },
        ActionKind::Mcp { server, .. } => {
            let tool = server
                .as_deref()
                .and_then(|server| {
                    action
                        .tool
                        .strip_prefix("mcp_")
                        .and_then(|rest| rest.strip_prefix(server))
                        .map(|rest| rest.trim_start_matches('_'))
                })
                .filter(|tool| !tool.is_empty());
            match (server, tool) {
                (Some(server), Some(tool)) => format!("called {server} · {tool}"),
                _ => format!("called {}", action.tool),
            }
        }
        ActionKind::Subagent {
            name,
            agent_id,
            outcome,
        } => {
            let who = name
                .as_deref()
                .or(agent_id.as_deref())
                .unwrap_or("an agent");
            match outcome {
                Some(outcome) => format!("started agent {who} — {outcome}"),
                None => format!("started agent {who}"),
            }
        }
        ActionKind::Approval => format!("asked to use {}", action.tool),
        ActionKind::Tool => format!("called {}", action.tool),
        ActionKind::TurnFailed => "turn failed".to_string(),
    }
}

/// The past-tense verbs [`action_phrase`] starts with, and their base form.
const PHRASE_VERBS: [(&str, &str); 11] = [
    ("ran ", "run "),
    ("edited ", "edit "),
    ("created ", "create "),
    ("deleted ", "delete "),
    ("wrote ", "write "),
    ("changed ", "change "),
    ("searched ", "search "),
    ("fetched ", "fetch "),
    ("called ", "call "),
    ("started ", "start "),
    ("made ", "make "),
];

/// `ran `x`` becomes `did not run `x`` (lead `did not`) or `tried to run
/// `x`` (lead `tried to`).
fn with_base_verb(lead: &str, phrase: &str) -> String {
    PHRASE_VERBS
        .iter()
        .find_map(|(past, base)| {
            phrase
                .strip_prefix(past)
                .map(|rest| format!("{lead} {base}{rest}"))
        })
        .unwrap_or_else(|| format!("{lead} run: {phrase}"))
}

fn duration_label(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

/// One line per action. Plain text that also reads as Markdown; whatever
/// the record holds cannot break it onto a second line (`one_line`).
#[must_use]
pub fn action_line(action: &ReceiptAction) -> String {
    let mut line = action_phrase(action);
    match action.status {
        // An approval line already reads as a request, not a run.
        ActionStatus::NotRun if action.what == ActionKind::Approval => {}
        ActionStatus::NotRun => line = with_base_verb("did not", &line),
        ActionStatus::Blocked => {
            line = with_base_verb("did not", &line);
            line.push_str(" — blocked");
            if let Some(error) = &action.error {
                line.push_str(&format!(": {error}"));
            }
        }
        ActionStatus::Failed => {
            line.push_str(" — failed");
            if let Some(error) = &action.error {
                line.push_str(&format!(": {error}"));
            }
        }
        ActionStatus::Interrupted => line.push_str(" — interrupted"),
        ActionStatus::Running if action.what != ActionKind::Approval => {
            line.push_str(" — still running")
        }
        ActionStatus::Unknown => {
            line = with_base_verb("tried to", &line);
            match &action.error {
                Some(error) => line.push_str(&format!(" — error, no exit code: {error}")),
                None => line.push_str(" — no result recorded"),
            }
        }
        _ => {}
    }
    if let Some(ms) = action.duration_ms
        && !matches!(action.status, ActionStatus::NotRun | ActionStatus::Blocked)
    {
        line.push_str(&format!(" · {}", duration_label(ms)));
    }
    if let Some(fact) = &action.approval {
        line.push_str(&format!(" · {}", approval_phrase(fact)));
    }
    one_line(&line)
}

fn tokens_line(tokens: &ReceiptTokens) -> String {
    let mut line = match (tokens.input, tokens.output) {
        (Some(input), Some(output)) => {
            format!("Tokens: {input} in · {output} out · {} total", tokens.total)
        }
        _ => format!("Tokens: {} total", tokens.total),
    };
    if let Some(calls) = tokens.calls {
        line.push_str(&format!(
            " · {}",
            plural(calls, "provider call", "provider calls")
        ));
    }
    let detail: Vec<String> = [
        ("cache read", tokens.cache_read),
        ("cache write", tokens.cache_write),
        ("reasoning", tokens.reasoning),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.map(|value| format!("{value} {label}")))
    .collect();
    if !detail.is_empty() {
        line.push_str(&format!(" ({})", detail.join(", ")));
    }
    line
}

fn usage_block(usage: &ReceiptUsage) -> String {
    let mut out = String::new();
    if let Some(tokens) = &usage.tokens {
        out.push_str(&one_line(&tokens_line(tokens)));
        out.push('\n');
    }
    if let Some(cost) = &usage.cost {
        out.push_str("Cost, as /cost reports it:\n");
        for line in cost
            .report
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            out.push_str(&format!("  {}\n", one_line(line)));
        }
    }
    out
}

/// The readable receipt: header, totals, one line per action, then what the
/// record does not hold. Valid Markdown and plain enough for a terminal.
#[must_use]
pub fn render_markdown(receipt: &Receipt) -> String {
    let source = &receipt.source;
    let noun = match source.kind {
        SourceKind::Session => "session",
        SourceKind::Thread => "thread",
    };
    let mut out = String::new();
    let title = source
        .title
        .as_deref()
        .map(|title| bounded(title.trim(), 80))
        .filter(|title| !title.is_empty());
    match title {
        Some(title) => out.push_str(&format!("# Receipt: {}\n\n", one_line(&title))),
        None => out.push_str(&format!("# Receipt: {noun} {}\n\n", one_line(&source.id))),
    }
    let mut facts = vec![format!("{noun} {}", source.id)];
    if let Some(turn) = &receipt.turn {
        facts.push(format!("turn {turn} only"));
    }
    if let Some(workspace) = &source.workspace {
        facts.push(workspace.clone());
    }
    if let Some(model) = &source.model {
        facts.push(model.clone());
    }
    if !receipt.postures.is_empty() {
        facts.push(receipt.postures.join(", "));
    }
    if let (Some(start), Some(end)) = (source.started_at, source.updated_at) {
        facts.push(format!(
            "{} → {}",
            start.format("%Y-%m-%d %H:%M UTC"),
            end.format("%Y-%m-%d %H:%M UTC")
        ));
    }
    out.push_str(&one_line(&facts.join(" · ")));
    out.push_str("\n\n");
    out.push_str(&one_line(&totals_line(receipt)));
    out.push_str("\n\n");
    if let Some(usage) = &receipt.usage {
        out.push_str(&usage_block(usage));
        out.push('\n');
    }
    let width = receipt.actions.len().to_string().len();
    for action in &receipt.actions {
        out.push_str(&format!(
            "{:>width$}. {}\n",
            action.seq,
            action_line(action),
            width = width
        ));
    }
    if receipt.omitted_actions > 0 {
        out.push_str(&format!(
            "\n{} more actions are counted above but not listed.\n",
            receipt.omitted_actions
        ));
    }
    if !receipt.not_recorded.is_empty() {
        out.push_str("\nNot recorded:\n");
        for note in &receipt.not_recorded {
            out.push_str(&format!("- {}\n", one_line(note)));
        }
    }
    out
}

#[must_use]
pub fn render_json(receipt: &Receipt) -> String {
    serde_json::to_string_pretty(receipt).unwrap_or_else(|_| "{}".to_string())
}

/// [`render_json`] inside a fenced code block, for a surface that renders
/// Markdown (the terminal's note cell): the fence keeps `$`, `*`, and `_` in
/// commands from being read as math or emphasis, and it is longer than any
/// backtick run a command holds, so the JSON copies out whole.
#[must_use]
pub fn render_json_block(receipt: &Receipt) -> String {
    let json = render_json(receipt);
    let longest = json.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}json\n{json}\n{fence}")
}
