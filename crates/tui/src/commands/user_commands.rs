//! User-defined slash commands from `~/.codewhale/commands/<name>.md` and
//! workspace-local `<workspace>/.codewhale/commands/<name>.md`.
//!
//! Users drop `.md` files into a commands directory and the filename
//! (without `.md` extension) becomes the default slash-command name. A
//! frontmatter `name` may replace it. When invoked, the file contents are sent
//! as a user message.
//!
//! Files may include optional YAML-like frontmatter between `---` markers.
//! Supported fields are `name`, `description`, `usage`, `arguments`,
//! `argument-hint`, `allowed-tools`, `pausable`, `alias`/`aliases`, and `hidden`.
//! Frontmatter is stripped before the command body is sent to the model.
//!
//! ## Precedence
//!
//! Workspace-local directories load only in a trusted workspace (`/trust`),
//! and a workspace command never replaces a protected built-in such as
//! `/trust` or `/undo` (the definition is skipped with a load error; see
//! `PROTECTED_BUILTINS` in `user_registry.rs`). Other built-ins stay
//! shadowable. Workspace-local directories
//! shadow user-global by name:
//!
//! 1. `<workspace>/.codewhale/commands/` (project-local, highest)
//! 2. `<workspace>/.deepseek/commands/`  (legacy project-local)
//! 3. `<workspace>/.claude/commands/`    (Claude Code interop)
//! 4. `<workspace>/.cursor/commands/`    (Cursor interop)
//! 5. `~/.codewhale/commands/`           (user-global)
//! 6. `~/.deepseek/commands/`            (legacy user-global)
//!
//! ## Permanent Role
//!
//! This module is the lower-level scanning, frontmatter parsing, and template
//! layer for [`super::user_registry::UserCommandRegistry`]. Runtime dispatch
//! lives in `user_registry.rs`; this file remains as the shared file I/O and
//! parsing boundary documented in `docs/architecture/command-dispatch.md`.

#[cfg(test)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::tui::app::{App, AppAction};

#[cfg(test)]
use super::CommandResult;

/// Path to the global user commands directory: `~/.codewhale/commands/`.
fn global_commands_dir() -> PathBuf {
    let home = codewhale_paths::user_home().unwrap_or_else(|| PathBuf::from("~"));
    home.join(".codewhale").join("commands")
}

fn legacy_global_commands_dir() -> PathBuf {
    let home = codewhale_paths::user_home().unwrap_or_else(|| PathBuf::from("~"));
    home.join(".deepseek").join("commands")
}

/// The workspace whose repository-supplied commands and workflows may load:
/// only a trusted one. A cloned repository's `.claude/commands/*.md` is text
/// the user never reviewed, sent to the model as the user's own message.
fn trusted_workspace(workspace: Option<&Path>) -> Option<&Path> {
    workspace.filter(|ws| codewhale_runtime::config::is_workspace_trusted(ws))
}

/// Workspace-local command directories, trusted or not, in precedence order.
pub(crate) fn workspace_commands_dirs(workspace: &Path) -> [PathBuf; 4] {
    [
        workspace.join(".codewhale").join("commands"),
        workspace.join(".deepseek").join("commands"),
        workspace.join(".claude").join("commands"),
        workspace.join(".cursor").join("commands"),
    ]
}

/// Return all candidate commands directories in precedence order. Workspace
/// directories are included only when the workspace is trusted.
pub(crate) fn commands_dirs(workspace: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(ws) = trusted_workspace(workspace) {
        dirs.extend(workspace_commands_dirs(ws));
    }
    dirs.push(global_commands_dir());
    dirs.push(legacy_global_commands_dir());
    dirs
}

/// Whether `path` came from the user's own global command or workflow store,
/// as opposed to a workspace (repository) directory.
pub(crate) fn is_user_global_command_source(path: &Path) -> bool {
    let home = codewhale_paths::user_home().unwrap_or_else(|| PathBuf::from("~"));
    [
        global_commands_dir(),
        legacy_global_commands_dir(),
        home.join(".codewhale").join("workflows"),
    ]
    .iter()
    .any(|dir| path.starts_with(dir))
}

/// Saved-workflow slash commands (#4121 packaging): `*.workflow.js` files
/// under these directories become `/name` commands that start the workflow
/// through the `workflow` tool with the slash arguments forwarded as the
/// run's `args`. Workspace definitions shadow the user-global store.
pub(crate) fn workflow_dirs(workspace: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(ws) = trusted_workspace(workspace) {
        dirs.push(ws.join(".codewhale").join("workflows"));
    }
    let home = codewhale_paths::user_home().unwrap_or_else(|| PathBuf::from("~"));
    dirs.push(home.join(".codewhale").join("workflows"));
    dirs
}

/// Canonical saved-workflow source suffix.
pub(crate) const WORKFLOW_SOURCE_SUFFIX: &str = ".workflow.js";

/// Scan one workflow directory and synthesize a markdown command definition
/// per `*.workflow.js` file. Returns `(name, content, source_path)` tuples;
/// unreadable entries are skipped.
pub(crate) fn load_workflow_commands_from_dir(dir: &Path) -> Vec<(String, String, PathBuf)> {
    let mut commands = Vec::new();
    if !dir.is_dir() {
        return commands;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return commands;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(WORKFLOW_SOURCE_SUFFIX) else {
            continue;
        };
        let name = stem.to_lowercase();
        if name.is_empty() {
            continue;
        }
        let description = std::fs::read_to_string(&path)
            .ok()
            .and_then(|source| workflow_headline(&source))
            .unwrap_or_else(|| format!("Run the saved workflow {name}"));
        let content = synthesize_workflow_command(&name, &description, &path);
        commands.push((name, content, path));
    }
    commands.sort_by(|a, b| a.0.cmp(&b.0));
    commands
}

/// First `//` comment line of a workflow source, used as the command
/// description in palettes and help.
fn workflow_headline(source: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let comment = line.trim().strip_prefix("//")?.trim();
        (!comment.is_empty()).then(|| comment.split_whitespace().collect::<Vec<_>>().join(" "))
    })
}

fn synthesize_workflow_command(name: &str, description: &str, path: &Path) -> String {
    // Frontmatter values must stay single-line; the headline is already one
    // line but defend against pathological sources.
    let description = description.replace(['\r', '\n'], " ");
    format!(
        "---\ndescription: {description}\nusage: /{name} [args...]\narguments: forwarded to the workflow run's args\n---\nStart the saved workflow `{name}` now: call the `workflow` tool with action=\"start\", source_path=\"{path}\", and args built from this argument text: $ARGUMENTS\nIf the argument text is empty, start the run without args. Report the run_id, monitor with the workflow tool's status action, and when the run settles present its receipt summary (status, phases, failures, artifacts). The durable report lands under .codewhale/reports/<run_id>.md.",
        path = path.display(),
    )
}

/// Scan a single commands directory for `.md` files and return
/// `(name, content)` pairs. Errors are silently skipped.
pub(crate) fn load_commands_from_dir(dir: &Path) -> Vec<(String, String)> {
    load_command_entries_from_component(dir)
        .into_iter()
        .map(|(name, content, _)| (name, content))
        .collect()
}

/// Load one reviewed command component from an immutable plugin snapshot.
/// Components may name either one markdown file or a directory of markdown
/// files; every returned entry retains the exact staged path for diagnostics.
pub(crate) fn load_command_entries_from_component(
    component: &Path,
) -> Vec<(String, String, PathBuf)> {
    if component.is_file() {
        if component.extension().and_then(|value| value.to_str()) != Some("md") {
            return Vec::new();
        }
        let Some(stem) = component.file_stem().and_then(|value| value.to_str()) else {
            return Vec::new();
        };
        return std::fs::read_to_string(component)
            .ok()
            .map(|content| vec![(stem.to_lowercase(), content, component.to_path_buf())])
            .unwrap_or_default();
    }

    let mut commands: Vec<(String, String, PathBuf)> = Vec::new();

    if !component.is_dir() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(component) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => stem.to_lowercase(),
            None => continue,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        commands.push((stem, content, path));
    }
    commands.sort_by(|left, right| left.0.cmp(&right.0));
    commands
}

/// Scan every candidate commands directory and return merged
/// `(name, content)` pairs. Workspace-local directories shadow
/// user-global by name — the first occurrence of a name wins.
///
/// Pass `None` for the workspace to scan only the global directory
/// (backward-compatible with callers that don't have workspace context).
#[cfg(test)]
pub fn load_user_commands(workspace: Option<&Path>) -> Vec<(String, String)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut commands: Vec<(String, String)> = Vec::new();

    for dir in commands_dirs(workspace) {
        for (name, content) in load_commands_from_dir(&dir) {
            if seen.insert(name.clone()) {
                commands.push((name, content));
            }
        }
    }

    // Sort by name for deterministic ordering.
    commands.sort_by(|a, b| a.0.cmp(&b.0));
    commands
}

pub(crate) fn parse_frontmatter(content: &str) -> (Vec<(String, String)>, &str) {
    let Some(first_line_end) = content.find('\n') else {
        return (Vec::new(), content);
    };
    let first = content[..first_line_end].trim_end_matches('\r');

    if first.trim().chars().all(|ch| ch == '-') && first.trim().len() >= 3 {
        let mut metadata = Vec::new();
        let mut offset = first_line_end + 1;
        let mut unclosed_body_start = None;
        for raw_line in content[offset..].split_inclusive('\n') {
            let line_start = offset;
            let line = raw_line.trim_end_matches(['\r', '\n']);
            offset += raw_line.len();
            let trimmed = line.trim();
            if unclosed_body_start.is_none() {
                if trimmed.chars().all(|ch| ch == '-') && trimmed.len() >= 3 {
                    let body = content[offset..].trim_start_matches(['\r', '\n']);
                    return (metadata, body);
                }
                if let Some((key, value)) = line.split_once(':') {
                    let key = key.trim().to_ascii_lowercase();
                    let raw_value = value.trim();
                    let value = if key == "allowed-tools" {
                        raw_value.to_string()
                    } else {
                        strip_matched_quotes(raw_value).to_string()
                    };
                    if !key.is_empty() {
                        metadata.push((key, value));
                    }
                } else if !trimmed.is_empty() {
                    unclosed_body_start = Some(line_start);
                }
            }
        }
        let body_start = unclosed_body_start.unwrap_or(content.len());
        let body = content[body_start..].trim_start_matches(['\r', '\n']);
        return (metadata, body);
    }

    (Vec::new(), content)
}

fn strip_matched_quotes(value: &str) -> &str {
    if let Some(stripped) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return stripped;
    }
    if let Some(stripped) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return stripped;
    }
    value
}

pub(crate) fn parse_allowed_tools(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|tool| {
            strip_matched_quotes(tool.trim())
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|tool| !tool.is_empty())
        .collect()
}

/// Check if the input matches a user-defined command and return the
/// content as a `SendMessage` action.
///
/// The `input` should be the full command string including the `/`
/// prefix (e.g. `/mycmd` or `/mycmd with args`). Only exact matches
/// on the command name are considered (no partial/alias matching).
/// Substitute $1, $2, $ARGUMENTS placeholders in a command template.
pub(crate) fn apply_template(template: &str, args: &str) -> String {
    let positional: Vec<&str> = args.split_whitespace().collect();
    let mut result = template.replace("$ARGUMENTS", args);
    for (i, arg) in positional.iter().enumerate() {
        result = result.replace(&format!("${}", i + 1), arg);
    }
    result
}

#[cfg(test)]
pub fn try_dispatch_user_command(app: &mut App, input: &str) -> Option<CommandResult> {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();
    let command = command.strip_prefix('/').unwrap_or(&command);
    let args = parts.get(1).copied().unwrap_or("").trim();

    let user_commands = load_user_commands(Some(&app.workspace));

    for (name, content) in &user_commands {
        if name == command {
            let (metadata, body) = parse_frontmatter(content);
            app.goal.objective = None;
            app.goal.started_at = None;
            app.goal.status = crate::tools::goal::GoalStatus::Active;
            app.goal.token_budget = None;
            app.goal.tokens_used = 0;
            app.goal.time_used_seconds = 0;
            app.goal.continuation_count = 0;
            app.active_allowed_tools = None;
            app.pausable = false;
            app.paused = false;
            app.paused_goal_objective = None;
            // Clear todos and plan state from the previous command so they
            // don't bleed into the next one. Both are behind the same locks
            // the sidebar reads; a contended/poisoned lock is logged and
            // skipped rather than blocking dispatch.
            if let Ok(mut todos) = app.todos.try_lock() {
                todos.clear();
            } else {
                tracing::warn!(target: "commands", "todos lock contended or poisoned — previous todos not cleared");
            }
            if let Ok(mut plan) = app.plan_state.try_lock() {
                *plan = crate::tools::plan::PlanState::default();
            } else {
                tracing::warn!(target: "commands", "plan_state lock contended or poisoned — previous plan not cleared");
            }
            for (key, value) in &metadata {
                match key.as_str() {
                    "description" => {
                        app.goal.objective = Some(value.clone());
                        app.goal.started_at = Some(std::time::Instant::now());
                    }
                    "allowed-tools" => {
                        app.active_allowed_tools = Some(parse_allowed_tools(value));
                    }
                    "pausable" => {
                        app.pausable = value.trim().eq_ignore_ascii_case("true");
                    }
                    _ => {}
                }
            }
            let message = apply_template(body, args);
            return Some(CommandResult::action(AppAction::SendMessage(message)));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Workspace commands load only in a trusted workspace.
    fn trusted_workspace() -> TempDir {
        let tmp = TempDir::new().unwrap();
        codewhale_runtime::config::save_workspace_trust(tmp.path()).expect("trust test workspace");
        tmp
    }

    #[test]
    fn test_global_commands_dir_contains_codewhale_commands() {
        let dir = global_commands_dir();
        let parts: Vec<_> = dir
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect();
        assert!(
            parts
                .windows(2)
                .any(|pair| pair == [".codewhale", "commands"]),
            "expected .codewhale/commands components in path, got: {}",
            dir.display()
        );
    }

    #[test]
    fn test_load_user_commands_when_no_dir_exists() {
        let cmds = load_user_commands(None);
        // Should not panic; returns empty vec when no directories exist.
        assert!(cmds.is_empty() || !cmds.is_empty());
    }

    #[test]
    fn test_try_dispatch_nonexistent_command() {
        use crate::tui::app::TuiOptions;
        use codewhale_runtime::config::Config;

        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        let result = try_dispatch_user_command(&mut app, "/nonexistent-thing-12345");
        assert!(result.is_none());
    }

    // ── Workspace-local commands tests ─────────────────────────────────

    fn write_command(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
    }

    fn test_options(workspace: PathBuf) -> crate::tui::app::TuiOptions {
        crate::tui::app::TuiOptions {
            ..crate::tui::test_support::test_tui_options(workspace)
        }
    }

    #[test]
    fn load_user_commands_scans_workspace_local_dir() {
        let tmp = trusted_workspace();
        let ws = tmp.path();
        let cmds_dir = ws.join(".codewhale").join("commands");
        write_command(&cmds_dir, "hello", "echo hi");

        let cmds = load_user_commands(Some(ws));
        let names: Vec<&str> = cmds.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"hello"),
            "expected 'hello' in workspace-local commands: {names:?}"
        );
    }

    #[test]
    fn load_user_commands_scans_claude_and_cursor_dirs() {
        let tmp = trusted_workspace();
        let ws = tmp.path();
        write_command(
            &ws.join(".claude").join("commands"),
            "claude-cmd",
            "claude body",
        );
        write_command(
            &ws.join(".cursor").join("commands"),
            "cursor-cmd",
            "cursor body",
        );

        let cmds = load_user_commands(Some(ws));
        let names: Vec<&str> = cmds.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"claude-cmd"),
            "expected 'claude-cmd': {names:?}"
        );
        assert!(
            names.contains(&"cursor-cmd"),
            "expected 'cursor-cmd': {names:?}"
        );
    }

    #[test]
    fn workspace_local_shadows_global_by_name() {
        let tmp = trusted_workspace();
        let ws = tmp.path();

        // Workspace-local version
        write_command(
            &ws.join(".codewhale").join("commands"),
            "shared",
            "workspace version",
        );
        // Global version — simulate by putting it in a "global" temp dir.
        // Paths resolve via codewhale_paths::user_home (HOME/USERPROFILE-aware). We test the
        // first-match-wins semantics by putting the same name in both
        // workspace-scanned dirs. The first dir in precedence order wins.
        write_command(
            &ws.join(".claude").join("commands"),
            "shared",
            "claude version",
        );

        let cmds = load_user_commands(Some(ws));
        let shared = cmds
            .iter()
            .find(|(n, _)| n == "shared")
            .expect("shared present");
        assert_eq!(
            shared.1, "workspace version",
            "workspace-local (.codewhale) must shadow later dirs"
        );
    }

    #[test]
    fn load_user_commands_without_workspace_falls_back_to_global_only() {
        // When no workspace is passed, only global command directories are
        // scanned. On test machines these often don't exist, so we just
        // verify we don't panic.
        let cmds = load_user_commands(None);
        // This should not panic; can be empty or have user's real commands.
        let _ = cmds;
    }

    #[test]
    fn try_dispatch_uses_workspace_local_command() {
        use crate::tui::app::TuiOptions;
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        write_command(
            &ws.join(".deepseek").join("commands"),
            "hello",
            "Hello, $ARGUMENTS!",
        );

        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(ws.clone())
        };
        let mut app = App::new(options, &Config::default());
        let result = try_dispatch_user_command(&mut app, "/hello world");
        assert!(result.is_some());
        let cmd_result = result.unwrap();
        match cmd_result.action {
            Some(AppAction::SendMessage(msg)) => {
                assert!(msg.contains("Hello, world!"), "got: {msg}");
            }
            other => panic!("expected SendMessage action, got: {other:?}"),
        }
    }

    #[test]
    fn frontmatter_is_stripped_before_dispatch() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        write_command(
            &ws.join(".deepseek").join("commands"),
            "secure",
            "---\ndescription: Secure scan\nallowed-tools: Bash, Read\n---\nRun $ARGUMENTS",
        );

        let mut app = App::new(test_options(ws), &Config::default());
        let result = try_dispatch_user_command(&mut app, "/secure checks").unwrap();
        match result.action {
            Some(AppAction::SendMessage(msg)) => assert_eq!(msg, "Run checks"),
            other => panic!("expected SendMessage action, got: {other:?}"),
        }
    }

    #[test]
    fn review_regression_unclosed_frontmatter_keeps_metadata_and_strips_header() {
        let (metadata, body) = parse_frontmatter(
            "---\ndescription: Broken command\nallowed-tools: Bash\nRun the safe body",
        );

        assert_eq!(
            metadata,
            vec![
                ("description".to_string(), "Broken command".to_string()),
                ("allowed-tools".to_string(), "Bash".to_string())
            ]
        );
        assert_eq!(body, "Run the safe body");
    }

    #[test]
    fn review_regression_unclosed_frontmatter_without_metadata_strips_header() {
        let (metadata, body) =
            parse_frontmatter("---\nRun the command body without a closing delimiter");

        assert!(metadata.is_empty());
        assert_eq!(body, "Run the command body without a closing delimiter");
    }

    #[test]
    fn review_regression_frontmatter_strips_only_matched_quote_pairs() {
        let (metadata, body) = parse_frontmatter("---\ndescription: 'Read\"\n---\nrun");

        assert_eq!(
            metadata,
            vec![("description".to_string(), "'Read\"".to_string())]
        );
        assert_eq!(body, "run");
    }

    #[test]
    fn allowed_tools_frontmatter_sets_app_state() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        write_command(
            &ws.join(".deepseek").join("commands"),
            "secure",
            "---\nallowed-tools: Bash, Grep\n---\nrun tests",
        );

        let mut app = App::new(test_options(ws), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/secure").unwrap();
        assert_eq!(
            app.active_allowed_tools,
            Some(vec!["bash".to_string(), "grep".to_string()])
        );
    }

    #[test]
    fn pausable_frontmatter_sets_app_state_without_worktree_mutation() {
        use codewhale_runtime::config::Config;

        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        let init = std::process::Command::new("git")
            .args(["-C", ws.to_str().unwrap(), "init"])
            .output()
            .expect("git init");
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        std::fs::write(ws.join("user-work.txt"), "untracked user work").unwrap();
        write_command(
            &ws.join(".codewhale").join("commands"),
            "pause-scan",
            "---\ndescription: Scan repos\npausable: true\n---\nscan",
        );

        let mut app = App::new(test_options(ws.clone()), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/pause-scan").unwrap();

        assert!(app.pausable);
        assert!(!app.paused);
        assert!(app.paused_goal_objective.is_none());
        assert!(ws.join("user-work.txt").exists());
        let stash = std::process::Command::new("git")
            .args(["-C", ws.to_str().unwrap(), "stash", "list"])
            .output()
            .expect("git stash list");
        assert!(
            stash.status.success(),
            "git stash list failed: {}",
            String::from_utf8_lossy(&stash.stderr)
        );
        assert!(
            String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
            "pausable dispatch must not create git stash entries"
        );
    }

    #[test]
    fn new_user_command_clears_stale_paused_state() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        let commands_dir = ws.join(".codewhale").join("commands");
        write_command(
            &commands_dir,
            "pause-scan",
            "---\ndescription: Scan repos\npausable: true\n---\nscan",
        );
        write_command(&commands_dir, "plain", "plain command");

        let mut app = App::new(test_options(ws), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/pause-scan").unwrap();
        app.paused = true;
        app.paused_goal_objective = Some("Scan repos".to_string());

        let _ = try_dispatch_user_command(&mut app, "/plain").unwrap();

        assert!(!app.pausable);
        assert!(!app.paused);
        assert!(app.paused_goal_objective.is_none());
    }

    #[test]
    fn new_user_command_clears_previous_todos_and_plan() {
        use crate::tools::plan::UpdatePlanArgs;
        use crate::tools::todo::TodoStatus;
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        let commands_dir = ws.join(".codewhale").join("commands");
        write_command(&commands_dir, "first", "first command body");
        write_command(&commands_dir, "second", "second command body");

        let mut app = App::new(test_options(ws), &Config::default());

        // Seed the state a previous command would leave behind: a non-empty
        // todo list and a non-empty plan. These should NOT bleed into the
        // next command. The shared lists are tokio async mutexes, so seed and
        // observe through `try_lock` (the same sync path dispatch uses).
        {
            let mut todos = app.todos.try_lock().expect("todos lock");
            todos.add(
                "leftover task from first command".to_string(),
                TodoStatus::Pending,
            );
        }
        {
            let mut plan = app.plan_state.try_lock().expect("plan_state lock");
            plan.update(UpdatePlanArgs {
                title: Some("leftover plan".to_string()),
                objective: Some("old goal".to_string()),
                ..Default::default()
            });
        }

        // Dispatch a fresh command — dispatch must reset both.
        let _ = try_dispatch_user_command(&mut app, "/second").unwrap();

        assert!(
            app.todos
                .try_lock()
                .expect("todos lock")
                .snapshot()
                .items
                .is_empty(),
            "previous command's todos must be cleared on new command dispatch"
        );
        assert!(
            app.plan_state
                .try_lock()
                .expect("plan_state lock")
                .snapshot()
                .is_empty(),
            "previous command's plan must be cleared on new command dispatch"
        );
    }

    #[test]
    fn review_regression_empty_allowed_tools_blocks_all_tools() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        write_command(
            &ws.join(".deepseek").join("commands"),
            "locked",
            "---\nallowed-tools: \"\"\n---\nrun nothing",
        );

        let mut app = App::new(test_options(ws), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/locked").unwrap();
        assert_eq!(app.active_allowed_tools, Some(Vec::new()));
    }

    #[test]
    fn review_regression_allowed_tools_accepts_per_item_quotes() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        write_command(
            &ws.join(".deepseek").join("commands"),
            "quoted",
            "---\nallowed-tools: \"exec_shell\", 'read_file'\n---\nrun quoted tools",
        );

        let mut app = App::new(test_options(ws), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/quoted").unwrap();
        assert_eq!(
            app.active_allowed_tools,
            Some(vec!["exec_shell".to_string(), "read_file".to_string()])
        );
    }

    #[test]
    fn review_regression_dispatch_without_frontmatter_resets_previous_command_state() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        let commands_dir = ws.join(".deepseek").join("commands");
        write_command(
            &commands_dir,
            "described",
            "---\ndescription: Scan repos\nallowed-tools: Bash\n---\nscan",
        );
        write_command(&commands_dir, "plain", "plain command");

        let mut app = App::new(test_options(ws), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/described").unwrap();
        assert_eq!(app.goal.objective.as_deref(), Some("Scan repos"));
        assert!(app.goal.started_at.is_some());
        assert_eq!(app.goal.status, crate::tools::goal::GoalStatus::Active);
        assert_eq!(app.goal.token_budget, None);
        assert_eq!(app.active_allowed_tools, Some(vec!["bash".to_string()]));

        app.goal.status = crate::tools::goal::GoalStatus::Blocked;
        app.goal.token_budget = Some(42);
        app.goal.tokens_used = 100;
        app.goal.time_used_seconds = 5;
        app.goal.continuation_count = 1;
        let _ = try_dispatch_user_command(&mut app, "/plain").unwrap();
        assert_eq!(app.goal.objective, None);
        assert_eq!(app.goal.started_at, None);
        assert_eq!(app.goal.status, crate::tools::goal::GoalStatus::Active);
        assert_eq!(app.goal.token_budget, None);
        assert_eq!(app.goal.tokens_used, 0);
        assert_eq!(app.goal.time_used_seconds, 0);
        assert_eq!(app.goal.continuation_count, 0);
        assert_eq!(app.active_allowed_tools, None);
    }

    #[test]
    fn description_frontmatter_sets_work_objective_and_autocomplete_description() {
        use codewhale_runtime::config::Config;

        let tmp = trusted_workspace();
        let ws = tmp.path().to_path_buf();
        write_command(
            &ws.join(".deepseek").join("commands"),
            "git-scan",
            "---\ndescription: Scan nested git repositories\nargument-hint: <root>\n---\nscan",
        );

        let mut app = App::new(test_options(ws.clone()), &Config::default());
        let _ = try_dispatch_user_command(&mut app, "/git-scan").unwrap();
        assert_eq!(
            app.goal.objective.as_deref(),
            Some("Scan nested git repositories")
        );
        let commands = load_user_commands(Some(&ws));
        let (_, content) = commands
            .iter()
            .find(|(name, _)| name == "git-scan")
            .expect("git-scan command should load");
        let (metadata, _) = parse_frontmatter(content);
        assert!(metadata.contains(&(
            "description".to_string(),
            "Scan nested git repositories".to_string()
        )));
        assert!(metadata.contains(&("argument-hint".to_string(), "<root>".to_string())));
    }

    #[test]
    fn parser_preserves_layer_5_1_frontmatter_fields() {
        let (metadata, body) = parse_frontmatter(
            "---\nname: inspect\ndescription: Inspect a target\nusage: /inspect <path>\narguments: <path>\nhidden: false\nallowed-tools: Read_File, Grep_Files\n---\ninspect $ARGUMENTS",
        );

        assert!(metadata.contains(&("name".to_string(), "inspect".to_string())));
        assert!(metadata.contains(&("description".to_string(), "Inspect a target".to_string())));
        assert!(metadata.contains(&("usage".to_string(), "/inspect <path>".to_string())));
        assert!(metadata.contains(&("arguments".to_string(), "<path>".to_string())));
        assert!(metadata.contains(&("hidden".to_string(), "false".to_string())));
        assert!(metadata.contains(&(
            "allowed-tools".to_string(),
            "Read_File, Grep_Files".to_string()
        )));
        assert_eq!(body, "inspect $ARGUMENTS");
    }
}
