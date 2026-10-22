//! Row builders for the dock views that are not work rows: files, notepad,
//! context, git, and price. Each returns ordinary [`WorkRow`]s so the one
//! row/hitbox pipeline in `render/` paints, selects, and clicks them exactly
//! like a to-do or a sub-agent — a view is a subset of one row grammar, not
//! a second widget system.

use crate::agent_roster::format_tokens;
use crate::tui::app::{App, SidebarRowAction};

use super::model::{RailPanel, WorkRow, WorkRowId, WorkTone};

/// Views whose tab always shows once the dock is up: the fact views have
/// something to say in every session, so they are always one click away.
pub(super) const fn view_always_has_content(panel: RailPanel) -> bool {
    matches!(
        panel,
        RailPanel::Context | RailPanel::Git | RailPanel::Price | RailPanel::Terminal
    )
}

/// Files this session edited or read, even before TASKS has been projected.
pub(super) fn files_touched_count(app: &mut App) -> usize {
    let activity = super::model::settled_file_activity(app);
    edited_files(activity).len() + activity.read.len()
}

pub(super) fn notepad_has_text(app: &App) -> bool {
    !app.workspace_notes.is_empty()
}

/// Each edited file once, newest receipt last. Direct writes without a
/// mutation receipt have no diff to inspect.
fn edited_files(
    activity: &super::model::SettledFileActivity,
) -> Vec<(String, Option<&crate::tui::history::FileMutationReceipt>)> {
    let mut files = Vec::new();
    for path in &activity.write {
        if !files.iter().any(|(existing, _)| existing == path) {
            files.push((path.clone(), None));
        }
    }
    for receipt in &activity.mutations {
        for file in &receipt.files {
            files.retain(|(path, _)| path != &file.path);
            files.push((file.path.clone(), Some(receipt)));
        }
    }
    files
}

/// The FILES view: what this session changed (with each change's size and
/// its evidence one Enter away), then what it read (#6565).
pub(super) fn files_rows(app: &mut App) -> Vec<WorkRow> {
    let activity = super::model::settled_file_activity(app).clone();
    let mut out = Vec::new();
    let edited = edited_files(&activity);
    if !edited.is_empty() {
        out.push(heading("files:edited", format!("Edited {}", edited.len())));
        for (path, receipt) in &edited {
            let Some(receipt) = receipt else {
                out.push(WorkRow {
                    mark: "✎",
                    tone: WorkTone::Success,
                    ..note_row(&format!("files:edit:{path}"), path)
                });
                continue;
            };
            let across = if receipt.files.len() > 1 {
                format!(" across {} files", receipt.files.len())
            } else {
                String::new()
            };
            out.push(WorkRow {
                id: WorkRowId(format!("files:edit:{path}")),
                mark: "✎",
                label: path.clone(),
                detail: format!("+{} −{}{across}", receipt.added, receipt.deleted),
                tone: WorkTone::Success,
                selectable: true,
                primary_action: Some(SidebarRowAction::InspectWork {
                    title: format!("File · {path}"),
                    body: super::model::settled_mutation_body(
                        std::slice::from_ref(*receipt),
                        activity.inline_diff_mode,
                    ),
                    stop_action: None,
                }),
                agent: None,
            });
        }
    }
    if !activity.read.is_empty() {
        out.push(heading(
            "files:read",
            format!("Read {}", activity.read.len()),
        ));
        for path in &activity.read {
            out.push(WorkRow {
                id: WorkRowId(format!("files:read:{path}")),
                mark: "·",
                label: path.clone(),
                detail: String::new(),
                tone: WorkTone::Muted,
                selectable: false,
                primary_action: None,
                agent: None,
            });
        }
    }
    app.work_surface.latest_rows = out.clone();
    out
}

/// The NOTES view: the workspace notes `/note` keeps, one row each; Enter
/// shows the whole note.
pub(super) fn notepad_rows(app: &mut App) -> Vec<WorkRow> {
    let out = app
        .workspace_notes
        .iter()
        .enumerate()
        .map(|(index, note)| {
            let number = index + 1;
            let first_line = note.lines().map(str::trim).find(|line| !line.is_empty());
            WorkRow {
                id: WorkRowId(format!("notes:{number}")),
                mark: "▪",
                label: crate::agent_roster::one_line(first_line.unwrap_or_default()),
                detail: format!("/note show {number}"),
                tone: WorkTone::Live,
                selectable: true,
                primary_action: Some(SidebarRowAction::Command(format!("/note show {number}"))),
                agent: None,
            }
        })
        .collect::<Vec<_>>();
    app.work_surface.latest_rows = out.clone();
    out
}

fn heading(id: &str, label: String) -> WorkRow {
    WorkRow {
        id: WorkRowId(id.to_string()),
        mark: "▾",
        label,
        detail: String::new(),
        tone: WorkTone::Heading,
        selectable: false,
        primary_action: None,
        agent: None,
    }
}

/// The context view: the budget, not a fact list. Used/limit and the
/// compaction threshold from the same snapshot the footer meter reads, a
/// gauge, then the breakdown the accounting can already give per frame —
/// system prompt, tool schemas, conversation, tool output, files read — and
/// the one action that exists as a command, `/compact`.
pub(super) fn context_rows(app: &mut App) -> Vec<WorkRow> {
    let Some((used, max, percent)) = crate::tui::ui::context_usage_snapshot(app) else {
        return Vec::new();
    };
    let used = u64::try_from(used).unwrap_or(0);
    let threshold = app.auto_compact_threshold_percent.round().clamp(0.0, 100.0) as u8;
    let fact = |id: &str, label: String, detail: &str| WorkRow {
        id: WorkRowId(format!("context:{id}")),
        mark: "·",
        label,
        detail: detail.to_string(),
        tone: WorkTone::Muted,
        selectable: false,
        primary_action: None,
        agent: None,
    };
    let mut out = vec![WorkRow {
        id: WorkRowId("context:budget".to_string()),
        mark: "◔",
        label: format!(
            "{} of {} · {}% · compacts at {threshold}%",
            format_tokens(used),
            format_tokens(u64::from(max)),
            percent.round() as u8
        ),
        detail: "/context for the full source map".to_string(),
        tone: if percent >= f64::from(threshold) {
            WorkTone::Attention
        } else {
            WorkTone::Live
        },
        selectable: true,
        primary_action: Some(SidebarRowAction::Command("/context".to_string())),
        agent: None,
    }];
    out.push(fact("gauge", gauge(percent, 24), ""));

    // Breakdown from what is already counted per frame: the system prompt
    // estimate the footer meter uses and the per-message token cache. No
    // text is re-scanned here; a message is tool output when every block in
    // it is a tool result.
    let system_tokens =
        crate::compaction::estimate_input_tokens_conservative(&[], app.system_prompt.as_ref());
    let tools = app
        .session
        .last_tool_catalog
        .as_ref()
        .map(|catalog| catalog.len())
        .unwrap_or(0);
    let (conversation, tool_output, messages) = message_split(app);
    out.push(fact(
        "system",
        format!(
            "system + tools · {} · {tools} tools",
            format_tokens(system_tokens as u64)
        ),
        "",
    ));
    out.push(fact(
        "conversation",
        format!(
            "conversation · {messages} · {}",
            format_tokens(conversation)
        ),
        "",
    ));
    out.push(fact(
        "tool-output",
        format!("tool output · {}", format_tokens(tool_output)),
        "",
    ));
    let read = super::model::settled_file_activity(app).read.clone();
    if !read.is_empty() {
        out.push(fact(
            "files",
            format!("files read · {}", read.len()),
            &read.join(", "),
        ));
    }
    out.push(WorkRow {
        id: WorkRowId("context:compact".to_string()),
        mark: "▸",
        label: "compact now".to_string(),
        detail: "/compact".to_string(),
        tone: WorkTone::Live,
        selectable: true,
        primary_action: Some(SidebarRowAction::Command("/compact".to_string())),
        agent: None,
    });
    app.work_surface.latest_rows = out.clone();
    out
}

/// `████████░░░░░░░░` — `width` cells, filled to `percent`.
fn gauge(percent: f64, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f64)
        .round()
        .clamp(0.0, width as f64) as usize;
    let mut bar = String::with_capacity(width * 3);
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in filled..width {
        bar.push('░');
    }
    bar
}

/// `(conversation tokens, tool-output tokens, message count)` from the
/// per-message estimate cache the meter already maintains. Messages the
/// cache has not seen yet count as zero rather than being re-estimated on
/// the render path.
fn message_split(app: &App) -> (u64, u64, usize) {
    let cache = app.context_token_cache.borrow();
    let mut conversation = 0u64;
    let mut tool_output = 0u64;
    for (index, message) in app.api_messages.iter().enumerate() {
        let tokens = cache
            .message_tokens
            .get(index)
            .map(|tokens| (*tokens as u64).saturating_mul(3).div_ceil(2))
            .unwrap_or(0);
        let all_tool_results = !message.content.is_empty()
            && message.content.iter().all(|block| {
                matches!(
                    block,
                    codewhale_models::ContentBlock::ToolResult { .. }
                        | codewhale_models::ContentBlock::ToolSearchToolResult { .. }
                        | codewhale_models::ContentBlock::CodeExecutionToolResult { .. }
                )
            });
        if all_tool_results {
            tool_output = tool_output.saturating_add(tokens);
        } else {
            conversation = conversation.saturating_add(tokens);
        }
    }
    (conversation, tool_output, app.api_messages.len())
}

/// The GIT view, from the cached repository probe (never git on the render
/// path): the branch and where it stands against its upstream, the changes
/// with their paths one Enter away, linked worktrees, and the last commits.
/// Before the first probe it says so; "not a git repository" only when the
/// probe found none, and a git failure is named (#6565).
pub(super) fn git_rows(app: &mut App) -> Vec<WorkRow> {
    let out = git_rows_for(&crate::git_status::cached_status(), &app.workspace);
    app.work_surface.latest_rows = out.clone();
    out
}

fn git_rows_for(
    snap: &crate::git_status::GitStatusSnapshot,
    workspace: &std::path::Path,
) -> Vec<WorkRow> {
    let probed_here = snap.probed_workspace.as_deref() == Some(workspace);
    if !probed_here || snap.fetched_at.is_none() {
        vec![note_row("git:state", "reading git status…")]
    } else if let Some(error) = snap.error.as_deref() {
        vec![note_row("git:state", error)]
    } else {
        git_state_rows(snap)
    }
}

fn note_row(id: &str, text: &str) -> WorkRow {
    WorkRow {
        id: WorkRowId(id.to_string()),
        mark: "·",
        label: text.to_string(),
        detail: String::new(),
        tone: WorkTone::Muted,
        selectable: false,
        primary_action: None,
        agent: None,
    }
}

fn git_state_rows(snap: &crate::git_status::GitStatusSnapshot) -> Vec<WorkRow> {
    let mut out = Vec::new();
    let branch = match (snap.branch.as_deref(), snap.detached) {
        (Some(id), true) => format!("detached at {id}"),
        (Some(branch), false) => branch.to_string(),
        (None, _) => "no commits yet".to_string(),
    };
    let upstream = if snap.detached {
        String::new()
    } else if !snap.has_upstream {
        " · no upstream".to_string()
    } else if snap.ahead == 0 && snap.behind == 0 {
        " · up to date".to_string()
    } else {
        format!(" · ↑{} ↓{}", snap.ahead, snap.behind)
    };
    let location = snap
        .remote_slug
        .clone()
        .or_else(|| snap.repository_name.clone())
        .unwrap_or_default();
    out.push(WorkRow {
        id: WorkRowId("git:branch".to_string()),
        mark: "⎇",
        label: format!("{branch}{upstream}"),
        detail: location,
        tone: WorkTone::Live,
        selectable: true,
        primary_action: Some(SidebarRowAction::Command("/diff".to_string())),
        agent: None,
    });
    let changes = snap.changes.summary();
    let body = if snap.changed_paths.is_empty() {
        "Working tree clean.".to_string()
    } else {
        let listed = snap
            .changed_paths
            .iter()
            .map(|path| format!("{} {}", path.code, path.path))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = if snap.changed_path_count > snap.changed_paths.len() {
            "\n… and more; /diff for everything".to_string()
        } else {
            String::new()
        };
        format!("{listed}{tail}")
    };
    out.push(WorkRow {
        id: WorkRowId("git:changes".to_string()),
        mark: if snap.changes.conflicts > 0 {
            "!"
        } else {
            "±"
        },
        label: changes,
        detail: String::new(),
        tone: if snap.changes.conflicts > 0 {
            WorkTone::Attention
        } else if snap.dirty {
            WorkTone::Live
        } else {
            WorkTone::Muted
        },
        selectable: true,
        primary_action: Some(SidebarRowAction::InspectWork {
            title: "Git · changes".to_string(),
            body,
            stop_action: None,
        }),
        agent: None,
    });
    let linked: Vec<_> = snap
        .worktrees
        .iter()
        .filter(|worktree| !worktree.bare && Some(&worktree.path) != snap.root.as_ref())
        .collect();
    if !linked.is_empty() {
        out.push(heading(
            "git:worktrees",
            format!("Worktrees {}", linked.len()),
        ));
        for worktree in linked {
            out.push(WorkRow {
                id: WorkRowId(format!("git:worktree:{}", worktree.path.display())),
                mark: "·",
                label: worktree
                    .branch
                    .clone()
                    .unwrap_or_else(|| "detached".to_string()),
                detail: format!(
                    "{}{}",
                    worktree.path.display(),
                    if worktree.locked { " · locked" } else { "" }
                ),
                tone: WorkTone::Muted,
                selectable: false,
                primary_action: None,
                agent: None,
            });
        }
    }
    if !snap.recent_commits.is_empty() {
        out.push(heading("git:commits", "Recent commits".to_string()));
        for commit in &snap.recent_commits {
            out.push(WorkRow {
                id: WorkRowId(format!("git:commit:{}", commit.hash)),
                mark: "·",
                label: format!("{} {}", commit.hash, commit.subject),
                detail: commit.when.clone(),
                tone: WorkTone::Muted,
                selectable: false,
                primary_action: None,
                agent: None,
            });
        }
    }
    out
}

/// The price view. One number everywhere: the session total is the same
/// `displayed_session_cost_for_currency` the footer chip prints, through the
/// same `App::format_cost_amount`; per-agent rows come from the roster's
/// usage receipts (`cost_microusd`, absent = no receipt, never `0`).
pub(super) fn price_rows(app: &mut App) -> Vec<WorkRow> {
    let mut out = Vec::new();
    let session = app.session_cost_label();
    out.push(WorkRow {
        id: WorkRowId("price:session".to_string()),
        mark: "$",
        label: format!("session · {session}"),
        detail: "/cost for the full ledger".to_string(),
        tone: WorkTone::Live,
        selectable: true,
        primary_action: Some(SidebarRowAction::Command("/cost".to_string())),
        agent: None,
    });
    let roster = app.current_agent_roster().to_vec();
    let priced: Vec<_> = roster
        .iter()
        .filter(|row| row.cost_microusd.is_some())
        .collect();
    if !priced.is_empty() {
        let total = priced
            .iter()
            .filter_map(|row| row.cost_microusd)
            .fold(0u64, u64::saturating_add);
        out.push(WorkRow {
            id: WorkRowId("price:agents".to_string()),
            mark: "·",
            label: format!(
                "agents · {}",
                app.format_cost_amount(total as f64 / 1_000_000.0)
            ),
            detail: format!("{} of {} agents priced", priced.len(), roster.len()),
            tone: WorkTone::Muted,
            selectable: false,
            primary_action: None,
            agent: None,
        });
        for row in priced {
            let cost = row.cost_microusd.unwrap_or(0) as f64 / 1_000_000.0;
            out.push(WorkRow {
                id: WorkRowId(format!("price:agent:{}", row.worker_id)),
                mark: row.state.glyph(),
                label: format!("  {} · {}", row.display_name, app.format_cost_amount(cost)),
                detail: row.model.clone(),
                tone: WorkTone::Muted,
                selectable: true,
                primary_action: Some(SidebarRowAction::OpenAgentDetail {
                    agent_id: row.worker_id.clone(),
                }),
                agent: None,
            });
        }
    }
    let rates = crate::tui::session_metrics::cache_rates(app);
    if let Some(rates) = rates.labelled("parent", "agents", "combined") {
        out.push(WorkRow {
            id: WorkRowId("price:cache".to_string()),
            mark: "·",
            label: format!("cache hit · {rates}"),
            detail: "/cache for per-turn cache telemetry".to_string(),
            tone: WorkTone::Muted,
            selectable: true,
            primary_action: Some(SidebarRowAction::Command("/cache".to_string())),
            agent: None,
        });
    }
    if let Some(rate) = crate::pricing::model_rate_label(
        app.api_provider,
        &app.model,
        app.cost_display_currency(app.cost_currency),
    ) {
        out.push(WorkRow {
            id: WorkRowId("price:rate".to_string()),
            mark: "·",
            label: format!("{} · {rate}", app.model),
            detail: "per million tokens, in / out".to_string(),
            tone: WorkTone::Muted,
            selectable: false,
            primary_action: None,
            agent: None,
        });
    }
    app.work_surface.latest_rows = out.clone();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_status::{
        ChangeCounts, ChangedPath, GitStatusSnapshot, RecentCommit, WorktreeEntry,
    };
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    fn app() -> App {
        crate::tui::app::App::new(
            crate::tui::test_support::test_tui_options(PathBuf::from("/repo")),
            &crate::config::Config::default(),
        )
    }

    #[test]
    fn the_git_view_names_its_state_honestly() {
        let workspace = Path::new("/repo");
        // Before the first probe for this workspace.
        let rows = git_rows_for(&GitStatusSnapshot::default(), workspace);
        assert_eq!(rows[0].label, "reading git status…");
        let other = GitStatusSnapshot {
            probed_workspace: Some(PathBuf::from("/elsewhere")),
            fetched_at: Some(Instant::now()),
            ..GitStatusSnapshot::default()
        };
        assert_eq!(
            git_rows_for(&other, workspace)[0].label,
            "reading git status…"
        );
        // Only a probe that found no repository says so.
        let outside = GitStatusSnapshot {
            probed_workspace: Some(workspace.to_path_buf()),
            fetched_at: Some(Instant::now()),
            error: Some("not a git repository".to_string()),
            ..GitStatusSnapshot::default()
        };
        assert_eq!(
            git_rows_for(&outside, workspace)[0].label,
            "not a git repository"
        );
        let broken = GitStatusSnapshot {
            error: Some("git unavailable: No such file".to_string()),
            ..outside
        };
        assert_eq!(
            git_rows_for(&broken, workspace)[0].label,
            "git unavailable: No such file"
        );
    }

    #[test]
    fn the_git_view_shows_branch_changes_worktrees_and_commits() {
        let workspace = Path::new("/repo");
        let snap = GitStatusSnapshot {
            probed_workspace: Some(workspace.to_path_buf()),
            fetched_at: Some(Instant::now()),
            root: Some(PathBuf::from("/repo")),
            repository_name: Some("repo".to_string()),
            remote_slug: Some("owner/repo".to_string()),
            branch: Some("main".to_string()),
            has_upstream: true,
            ahead: 2,
            behind: 1,
            dirty: true,
            changes: ChangeCounts {
                staged: 1,
                modified: 1,
                untracked: 0,
                conflicts: 0,
            },
            changed_path_count: 2,
            changed_paths: vec![
                ChangedPath {
                    code: "M ".to_string(),
                    path: "src/lib.rs".to_string(),
                },
                ChangedPath {
                    code: " M".to_string(),
                    path: "README.md".to_string(),
                },
            ],
            worktrees: vec![
                WorktreeEntry {
                    path: PathBuf::from("/repo"),
                    branch: Some("main".to_string()),
                    bare: false,
                    locked: false,
                },
                WorktreeEntry {
                    path: PathBuf::from("/repo/.cw-worktrees/agent-a"),
                    branch: Some("agent-a".to_string()),
                    bare: false,
                    locked: true,
                },
            ],
            recent_commits: vec![RecentCommit {
                hash: "abc1234".to_string(),
                subject: "fix: a thing".to_string(),
                when: "3 hours ago".to_string(),
            }],
            ..GitStatusSnapshot::default()
        };
        let rows = git_rows_for(&snap, workspace);
        let labels = rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "main · ↑2 ↓1",
                "1 staged, 1 modified",
                "Worktrees 1",
                "agent-a",
                "Recent commits",
                "abc1234 fix: a thing",
            ]
        );
        assert_eq!(rows[0].detail, "owner/repo");
        let Some(SidebarRowAction::InspectWork { body, .. }) = rows[1].primary_action.as_ref()
        else {
            panic!("changes row opens its paths");
        };
        assert_eq!(body, "M  src/lib.rs\n M README.md");
        assert!(
            rows[3].detail.ends_with("agent-a · locked"),
            "{}",
            rows[3].detail
        );

        let no_upstream = GitStatusSnapshot {
            has_upstream: false,
            ..snap.clone()
        };
        assert_eq!(
            git_rows_for(&no_upstream, workspace)[0].label,
            "main · no upstream"
        );
        let detached = GitStatusSnapshot {
            branch: Some("abc1234".to_string()),
            detached: true,
            ..snap
        };
        assert_eq!(
            git_rows_for(&detached, workspace)[0].label,
            "detached at abc1234"
        );
    }

    #[test]
    fn git_more_means_unlisted_paths() {
        use crate::git_status::{MAX_CHANGED_PATHS, parse_porcelain_v2};
        for count in [1, MAX_CHANGED_PATHS, MAX_CHANGED_PATHS + 1] {
            let mut raw = "# branch.head main\0".to_string();
            for index in 0..count {
                raw.push_str(&format!(
                    "1 MM N... 100644 100644 100644 aaa bbb file-{index}\0"
                ));
            }
            let status = parse_porcelain_v2(&raw).unwrap();
            let rows = git_state_rows(&GitStatusSnapshot {
                changes: status.changes,
                changed_paths: status.changed_paths,
                changed_path_count: status.changed_path_count,
                ..GitStatusSnapshot::default()
            });
            let Some(SidebarRowAction::InspectWork { body, .. }) = &rows[1].primary_action else {
                panic!("changes opens path details");
            };
            assert_eq!(
                body.contains("and more"),
                count > MAX_CHANGED_PATHS,
                "{count}"
            );
        }
    }

    fn record_file(app: &mut App, id: &str, action: &str, path: &str, diff: Option<String>) {
        use crate::tools::spec::ToolResult;
        use crate::tui::tool_routing::{handle_tool_call_complete, handle_tool_call_started};
        let input = serde_json::json!({"action": action, "path": path, "content": "new"});
        handle_tool_call_started(app, id, "File", &input);
        let mut result = ToolResult::success("ok");
        if let Some(diff) = diff {
            result = result.with_metadata(serde_json::json!({"mutation": {
                "diff": diff, "files": [{"path": path, "outcome": "updated"}],
            }}));
        }
        handle_tool_call_complete(app, id, "File", &Ok(result));
        app.flush_active_cell();
    }

    #[test]
    fn files_and_badge_follow_current_history_without_opening_tasks() {
        for badge_first in [false, true] {
            let mut app = app();
            app.current_session_id = Some("session-a".into());
            record_file(&mut app, "first", "write", "first.rs", None);
            if badge_first {
                assert_eq!(files_touched_count(&mut app), 1);
            } else {
                assert!(
                    files_rows(&mut app)
                        .iter()
                        .any(|row| row.label == "first.rs")
                );
            }
            record_file(&mut app, "second", "read", "second.rs", None);
            if badge_first {
                assert_eq!(files_touched_count(&mut app), 2);
            } else {
                assert!(
                    files_rows(&mut app)
                        .iter()
                        .any(|row| row.label == "second.rs")
                );
            }
            // Keep the old projection, as a session switch can, but replace
            // the transcript that owns the next session's activity. Go
            // through `clear_history` — a raw `history.clear()` would skip
            // the revision epochs the caches key on.
            app.clear_history();
            app.current_session_id = Some("session-b".into());
            if badge_first {
                assert_eq!(files_touched_count(&mut app), 0);
                assert!(files_rows(&mut app).is_empty());
            } else {
                assert!(files_rows(&mut app).is_empty());
                assert_eq!(files_touched_count(&mut app), 0);
            }
        }
    }

    #[test]
    fn failed_git_status_reports_the_error_in_the_view_and_composer() {
        use crate::git_status::{context_line, probe_status};
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let init = std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(workspace)
            .output()
            .unwrap();
        assert!(init.status.success());
        let clean = probe_status(workspace);
        assert!(clean.error.is_none(), "{clean:?}");
        assert!(context_line(&clean).unwrap().contains("clean"));
        std::fs::write(workspace.join(".git/index"), b"broken index").unwrap();
        let failed = probe_status(workspace);
        assert!(failed.root.is_some(), "root discovery still succeeds");
        let error = failed
            .error
            .as_deref()
            .expect("status failure must be recorded");
        assert!(error.contains("git status failed"), "{error}");
        assert!(error.contains("index"), "{error}");
        assert_eq!(context_line(&failed).as_deref(), Some(error));
        let rows = git_rows_for(&failed, workspace);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, error);
        assert!(!rows[0].label.contains("clean"));
    }

    #[test]
    fn files_include_successful_writes_without_receipts() {
        use crate::tools::spec::ToolResult;
        use crate::tui::tool_routing::{handle_tool_call_complete, handle_tool_call_started};
        let mut app = app();
        for (id, action, path) in [("write", "write", "new.rs"), ("edit", "edit", "edited.rs")] {
            let input = serde_json::json!({"action": action, "path": path, "content": "new"});
            handle_tool_call_started(&mut app, id, "File", &input);
            handle_tool_call_complete(&mut app, id, "File", &Ok(ToolResult::success("ok")));
            app.flush_active_cell();
        }
        assert_eq!(files_touched_count(&mut app), 2);
        let rows = files_rows(&mut app);
        for path in ["new.rs", "edited.rs"] {
            let row = rows
                .iter()
                .find(|row| row.label == path)
                .expect("write row");
            assert!(!row.selectable);
            assert!(row.primary_action.is_none());
            assert!(row.detail.is_empty());
        }
        // A receipt for an already-listed write adds its diff, not a second row.
        record_file(
            &mut app,
            "receipt",
            "edit",
            "edited.rs",
            Some("--- a/edited.rs\n+++ b/edited.rs\n@@ -0,0 +1 @@\n+new\n".into()),
        );
        let rows = files_rows(&mut app);
        assert_eq!(files_touched_count(&mut app), 2);
        assert_eq!(
            rows.iter().filter(|row| row.label == "edited.rs").count(),
            1
        );
        assert!(
            rows.iter()
                .find(|row| row.label == "edited.rs")
                .unwrap()
                .selectable
        );
    }

    #[test]
    fn the_files_view_lists_edits_with_their_size_then_reads() {
        let mut app = app();
        assert!(files_rows(&mut app).is_empty());
        assert_eq!(files_touched_count(&mut app), 0);
        let diff = format!(
            "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,12 @@\n{}{}",
            "-old\n".repeat(3),
            "+new\n".repeat(12),
        );
        record_file(&mut app, "edit", "edit", "src/lib.rs", Some(diff));
        record_file(&mut app, "read", "read", "Cargo.toml", None);
        let rows = files_rows(&mut app);
        let labels = rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["Edited 1", "src/lib.rs", "Read 1", "Cargo.toml"]);
        assert_eq!(rows[1].detail, "+12 −3");
        assert!(rows[1].selectable);
        assert_eq!(files_touched_count(&mut app), 2);
    }

    #[test]
    fn the_notes_view_lists_each_note_and_opens_it() {
        let mut app = app();
        assert!(!notepad_has_text(&app));
        assert!(notepad_rows(&mut app).is_empty());
        app.workspace_notes = vec![
            "Ship the docs fix\nwith the link audit".to_string(),
            "Ask about the flaky test".to_string(),
        ];
        assert!(notepad_has_text(&app));
        let rows = notepad_rows(&mut app);
        assert_eq!(rows[0].label, "Ship the docs fix");
        assert!(matches!(
            rows[1].primary_action.as_ref(),
            Some(SidebarRowAction::Command(command)) if command == "/note show 2"
        ));
    }
}
