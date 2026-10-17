//! Note command: manage persistent workspace notes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;

const USAGE: &str = "/note <text> | /note add <text> | /note list | /note show <n> | /note edit <n> <text> | /note remove <n> | /note clear | /note path";

/// Manage the persistent workspace notes file.
fn note(workspace: &Path, content: Option<&str>) -> CommandResult {
    let input = match content {
        Some(c) => c.trim(),
        None => {
            return CommandResult::error(format!("Usage: {USAGE}"));
        }
    };

    if input.is_empty() {
        return CommandResult::error("Note content cannot be empty");
    }

    let notes_path = notes_path(workspace);
    let (command, rest) = split_command(input);
    let command = command.to_ascii_lowercase();
    if !matches!(command.as_str(), "path" | "help")
        && let Err(error) = ensure_notes_target_in_workspace(&notes_path, workspace)
    {
        return CommandResult::error(error);
    }

    match command.as_str() {
        "add" => append_note_command(workspace, &notes_path, rest),
        "list" => list_notes_command(workspace, &notes_path),
        "show" => show_note_command(workspace, &notes_path, rest),
        "edit" => edit_note_command(workspace, &notes_path, rest),
        "remove" | "rm" | "delete" => remove_note_command(workspace, &notes_path, rest),
        "clear" => clear_notes_command(workspace, &notes_path),
        "path" => CommandResult::message(format!("Notes path: {}", notes_path.display())),
        "help" => CommandResult::message(format!("Usage: {USAGE}")),
        _ => append_note_command(workspace, &notes_path, Some(input)),
    }
}

/// Resolve the notes file. An existing `.codewhale` notes file is preferred;
/// otherwise the `.deepseek` notes path is used (D3 — the fallback stays
/// handler-owned through standard filesystem operations). The dock's NOTES
/// view reads the same file through this resolver (#6565).
pub(crate) fn notes_path(workspace: &Path) -> PathBuf {
    let primary = workspace.join(".codewhale").join("notes.md");
    if primary.symlink_metadata().is_ok() || workspace.join(".codewhale").is_symlink() {
        return primary;
    }
    workspace.join(".deepseek").join("notes.md")
}

/// The notes file lives in the workspace, which may be a cloned repository:
/// a committed `notes.md -> ~/.zshrc` or a symlinked `.codewhale/` must not
/// let `/note clear` empty (or `/note list` print) a file outside it. Same
/// rule as the `note` tool.
fn ensure_notes_target_in_workspace(notes_path: &Path, workspace: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(notes_path)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(format!(
            "Refusing to use {}: the notes path is a symlink or not a regular file.",
            notes_path.display()
        ));
    }
    let (Some(parent), Ok(root)) = (notes_path.parent(), fs::canonicalize(workspace)) else {
        return Ok(());
    };
    // Only existing ancestors can redirect; the rest is created as real
    // directories.
    let mut existing = parent.to_path_buf();
    while !existing.exists() {
        if !existing.pop() {
            return Ok(());
        }
    }
    match fs::canonicalize(&existing) {
        Ok(resolved) if !resolved.starts_with(&root) => Err(format!(
            "Refusing to use {}: its directory resolves outside the workspace.",
            notes_path.display()
        )),
        _ => Ok(()),
    }
}

fn split_command(input: &str) -> (&str, Option<&str>) {
    match input.find(char::is_whitespace) {
        Some(index) => (&input[..index], Some(input[index..].trim())),
        None => (input, None),
    }
}

fn append_note_command(
    workspace: &Path,
    notes_path: &Path,
    content: Option<&str>,
) -> CommandResult {
    let Some(note_content) = content.map(str::trim).filter(|content| !content.is_empty()) else {
        return CommandResult::error("Usage: /note add <text>");
    };

    match append_note(workspace, notes_path, note_content) {
        Ok(()) => CommandResult::message(format!("Note appended to {}", notes_path.display())),
        Err(e) => CommandResult::error(e),
    }
}

fn list_notes_command(workspace: &Path, notes_path: &Path) -> CommandResult {
    let notes = match read_notes(workspace, notes_path) {
        Ok(notes) => notes,
        Err(e) => return CommandResult::error(e),
    };

    if notes.is_empty() {
        return CommandResult::message(format!("No notes found at {}", notes_path.display()));
    }

    let mut output = format!("Notes in {}:", notes_path.display());
    for (index, note) in notes.iter().enumerate() {
        output.push_str(&format!("\n\n{}. {}", index + 1, note_preview(note)));
    }
    CommandResult::message(output)
}

fn show_note_command(workspace: &Path, notes_path: &Path, rest: Option<&str>) -> CommandResult {
    let notes = match read_notes(workspace, notes_path) {
        Ok(notes) => notes,
        Err(e) => return CommandResult::error(e),
    };
    let index = match parse_note_index(rest, notes.len(), "/note show <n>") {
        Ok(index) => index,
        Err(e) => return CommandResult::error(e),
    };

    CommandResult::message(format!("Note {}:\n\n{}", index + 1, notes[index]))
}

fn edit_note_command(workspace: &Path, notes_path: &Path, rest: Option<&str>) -> CommandResult {
    let Some(rest) = rest else {
        return CommandResult::error("Usage: /note edit <n> <text>");
    };
    let (index_text, new_content) = match split_command(rest) {
        (index_text, Some(new_content)) if !new_content.trim().is_empty() => {
            (index_text, new_content.trim())
        }
        _ => return CommandResult::error("Usage: /note edit <n> <text>"),
    };

    let mut notes = match read_notes(workspace, notes_path) {
        Ok(notes) => notes,
        Err(e) => return CommandResult::error(e),
    };
    let index = match parse_note_index(Some(index_text), notes.len(), "/note edit <n> <text>") {
        Ok(index) => index,
        Err(e) => return CommandResult::error(e),
    };

    notes[index] = new_content.to_string();
    match write_notes(workspace, notes_path, &notes) {
        Ok(()) => CommandResult::message(format!(
            "Note {} updated in {}",
            index + 1,
            notes_path.display()
        )),
        Err(e) => CommandResult::error(e),
    }
}

fn remove_note_command(workspace: &Path, notes_path: &Path, rest: Option<&str>) -> CommandResult {
    let mut notes = match read_notes(workspace, notes_path) {
        Ok(notes) => notes,
        Err(e) => return CommandResult::error(e),
    };
    let index = match parse_note_index(rest, notes.len(), "/note remove <n>") {
        Ok(index) => index,
        Err(e) => return CommandResult::error(e),
    };

    notes.remove(index);
    match write_notes(workspace, notes_path, &notes) {
        Ok(()) => CommandResult::message(format!(
            "Note {} removed from {}",
            index + 1,
            notes_path.display()
        )),
        Err(e) => CommandResult::error(e),
    }
}

fn clear_notes_command(workspace: &Path, notes_path: &Path) -> CommandResult {
    match write_notes(workspace, notes_path, &[]) {
        Ok(()) => CommandResult::message(format!("Notes cleared in {}", notes_path.display())),
        Err(e) => CommandResult::error(e),
    }
}

fn append_note(workspace: &Path, notes_path: &Path, note_content: &str) -> Result<(), String> {
    let mut file = match crate::fs_confined::open_append(workspace, notes_path) {
        Ok(f) => f,
        Err(e) => {
            return Err(format!("Failed to open notes file: {e}"));
        }
    };

    // Write separator and note content
    if let Err(e) = writeln!(file, "\n---\n{note_content}") {
        return Err(format!("Failed to write note: {e}"));
    }

    Ok(())
}

pub(crate) fn read_notes(workspace: &Path, notes_path: &Path) -> Result<Vec<String>, String> {
    match crate::fs_confined::read_to_string(workspace, notes_path) {
        Ok(content) => Ok(parse_notes(&content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Failed to read notes file: {e}")),
    }
}

fn write_notes(workspace: &Path, notes_path: &Path, notes: &[String]) -> Result<(), String> {
    let content = notes
        .iter()
        .map(|note| format!("---\n{}", note.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    crate::fs_confined::write(workspace, notes_path, content.as_bytes())
        .map_err(|e| format!("Failed to write notes file: {e}"))
}

fn parse_notes(content: &str) -> Vec<String> {
    let mut notes = Vec::new();
    let mut current = Vec::new();
    let mut saw_separator = false;

    for line in content.lines() {
        if line.trim() == "---" {
            if saw_separator || !current.is_empty() {
                push_note(&mut notes, &current);
                current.clear();
            }
            saw_separator = true;
        } else if saw_separator || !line.trim().is_empty() {
            current.push(line);
        }
    }

    if saw_separator {
        push_note(&mut notes, &current);
    } else {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            notes.push(trimmed.to_string());
        }
    }

    notes
}

fn push_note(notes: &mut Vec<String>, lines: &[&str]) {
    let note = lines.join("\n").trim().to_string();
    if !note.is_empty() {
        notes.push(note);
    }
}

fn note_preview(note: &str) -> String {
    let first_line = note
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or("(empty note)");
    if note.lines().filter(|line| !line.trim().is_empty()).count() > 1 {
        format!("{first_line} ...")
    } else {
        first_line.to_string()
    }
}

fn parse_note_index(rest: Option<&str>, note_count: usize, usage: &str) -> Result<usize, String> {
    let Some(index_text) = rest.map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(format!("Usage: {usage}"));
    };
    let index = index_text
        .parse::<usize>()
        .map_err(|_| format!("Invalid note number: {index_text}"))?;
    if index == 0 || index > note_count {
        return Err(format!(
            "Note number {index} out of range; there are {note_count} note(s)"
        ));
    }
    Ok(index - 1)
}

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "note",
    aliases: &[],
    usage: "/note [add|list|show|edit|remove|clear|path]",
    description_key: "cmd_note_description",
};

pub(in crate::commands) struct NoteCmd;

impl RegisterCommand<CommandResult> for NoteCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE,
            handler: note_contextual,
        }
    }
}

fn note_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(workspace) = parts.workspace.as_deref() else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    note(&workspace.workspace(), arg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    use codewhale_command_contract::facets::CommandWorkspaceContext;

    struct FakeWorkspace {
        path: PathBuf,
    }

    impl CommandWorkspaceContext for FakeWorkspace {
        fn effective_home_dir(&self) -> Option<std::path::PathBuf> {
            unreachable!("navigation is not used by note")
        }
        fn inspect_workspace_target(
            &self,
            _: &std::path::Path,
        ) -> codewhale_command_contract::navigation::WorkspaceTarget {
            unreachable!("navigation is not used by note")
        }
        fn read_anchor_document(&self) -> Result<Option<String>, String> {
            unreachable!("anchors are not used by note")
        }
        fn append_anchor_bytes(
            &mut self,
            _: &[&[u8]],
        ) -> Result<std::path::PathBuf, codewhale_command_contract::navigation::AnchorWriteError>
        {
            unreachable!("anchors are not used by note")
        }
        fn replace_anchor_document(&mut self, _: &[u8]) -> Result<(), String> {
            unreachable!("anchors are not used by note")
        }
        fn workspace(&self) -> PathBuf {
            self.path.clone()
        }

        fn work_state_snapshot(&self) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn operation_digest(&mut self) -> Result<String, String> {
            Ok("No active operations or to-do items.".to_string())
        }
    }

    fn fake_workspace(tmpdir: &TempDir) -> FakeWorkspace {
        FakeWorkspace {
            path: tmpdir.path().to_path_buf(),
        }
    }

    fn notes_path(tmpdir: &TempDir) -> PathBuf {
        tmpdir.path().join(".deepseek").join("notes.md")
    }

    fn message(result: CommandResult) -> String {
        result.message.expect("command message")
    }

    #[cfg(unix)]
    #[test]
    fn confined_notes_refuse_linked_files_and_directories() {
        use std::os::unix::fs::symlink;

        for directory in [".codewhale", ".deepseek"] {
            for linked_directory in [false, true] {
                let workspace = TempDir::new().unwrap();
                let outside = TempDir::new().unwrap();
                let original = "---\nfirst note\n\n---\nsecond note";
                let target = outside.path().join("notes.md");
                std::fs::write(&target, original).unwrap();
                let parent = workspace.path().join(directory);
                if linked_directory {
                    symlink(outside.path(), &parent).unwrap();
                } else {
                    std::fs::create_dir(&parent).unwrap();
                    symlink(&target, parent.join("notes.md")).unwrap();
                }
                for command in [
                    "list",
                    "show 1",
                    "add next",
                    "edit 1 next",
                    "remove 1",
                    "clear",
                ] {
                    let result = note(workspace.path(), Some(command));
                    assert!(
                        result.is_error,
                        "{directory}, {linked_directory}, {command}"
                    );
                    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn confined_notes_refuse_dangling_links() {
        for directory in [".codewhale", ".deepseek"] {
            let workspace = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            let target = outside.path().join("missing.md");
            let parent = workspace.path().join(directory);
            std::fs::create_dir(&parent).unwrap();
            std::os::unix::fs::symlink(&target, parent.join("notes.md")).unwrap();
            for command in ["list", "add next", "clear"] {
                assert!(note(workspace.path(), Some(command)).is_error);
                assert!(!target.exists());
            }
            if directory == ".codewhale" {
                assert!(!workspace.path().join(".deepseek").exists());
            }
        }
    }

    #[test]
    fn test_note_without_content_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        let result = note(&workspace.path, None);
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("Usage: /note"));
    }

    #[test]
    fn test_note_with_empty_content_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        let result = note(&workspace.path, Some("   "));
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("cannot be empty"));
    }

    #[test]
    fn test_note_appends_to_file() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        let result = note(&workspace.path, Some("Test note content"));
        assert!(result.message.is_some());
        let msg = message(result);
        assert!(msg.contains("Note appended to"));

        let notes_path = notes_path(&tmpdir);
        assert!(notes_path.exists());
        let content = std::fs::read_to_string(&notes_path).unwrap();
        assert!(content.contains("Test note content"));
    }

    #[test]
    fn test_note_multiple_appends() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("First note"));
        note(&workspace.path, Some("Second note"));

        let notes_path = notes_path(&tmpdir);
        let content = std::fs::read_to_string(&notes_path).unwrap();
        assert!(content.contains("First note"));
        assert!(content.contains("Second note"));
        // Should have two separators
        assert_eq!(content.matches("---").count(), 2);
    }

    #[test]
    fn test_note_list_numbers_entries_without_storing_numbers() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("Alpha note"));
        note(&workspace.path, Some("Beta note"));

        let listed = message(note(&workspace.path, Some("list")));
        assert!(listed.contains("1. Alpha note"));
        assert!(listed.contains("2. Beta note"));

        let content = std::fs::read_to_string(notes_path(&tmpdir)).unwrap();
        assert!(content.contains("Alpha note"));
        assert!(!content.contains("1. Alpha note"));
    }

    #[test]
    fn test_note_show_displays_full_multiline_note() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("add first line\nsecond line"));

        let shown = message(note(&workspace.path, Some("show 1")));
        assert!(shown.contains("Note 1:"));
        assert!(shown.contains("first line\nsecond line"));
    }

    #[test]
    fn test_note_edit_updates_numbered_entry() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("First note"));
        note(&workspace.path, Some("Second note"));

        let edited = message(note(&workspace.path, Some("edit 2 Updated second note")));
        assert!(edited.contains("Note 2 updated"));

        let content = std::fs::read_to_string(notes_path(&tmpdir)).unwrap();
        assert!(content.contains("First note"));
        assert!(content.contains("Updated second note"));
        assert!(!content.contains("Second note"));
    }

    #[test]
    fn test_note_remove_renumbers_remaining_entries() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("First note"));
        note(&workspace.path, Some("Second note"));
        note(&workspace.path, Some("Third note"));

        let removed = message(note(&workspace.path, Some("remove 2")));
        assert!(removed.contains("Note 2 removed"));

        let listed = message(note(&workspace.path, Some("list")));
        assert!(listed.contains("1. First note"));
        assert!(listed.contains("2. Third note"));
        assert!(!listed.contains("Second note"));
    }

    #[cfg(unix)]
    #[test]
    fn note_commands_refuse_notes_that_leave_the_workspace() {
        let tmpdir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let rc = outside.path().join("zshrc");
        std::fs::write(&rc, "export PATH=keep\n").unwrap();
        std::fs::create_dir_all(tmpdir.path().join(".deepseek")).unwrap();
        std::os::unix::fs::symlink(&rc, notes_path(&tmpdir)).unwrap();
        for command in ["clear", "remove 1", "edit 1 x", "list", "hello"] {
            let result = note(tmpdir.path(), Some(command));
            assert!(result.is_error, "/note {command} must be refused");
        }
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export PATH=keep\n");

        let linked = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), linked.path().join(".deepseek")).unwrap();
        assert!(note(linked.path(), Some("clear")).is_error);
        assert!(!outside.path().join("notes.md").exists());
    }

    #[test]
    fn test_note_clear_empties_file() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("First note"));

        let cleared = message(note(&workspace.path, Some("clear")));
        assert!(cleared.contains("Notes cleared"));
        assert_eq!(std::fs::read_to_string(notes_path(&tmpdir)).unwrap(), "");
    }

    #[test]
    fn test_note_path_prints_workspace_notes_file() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);

        let path = message(note(&workspace.path, Some("path")));
        assert!(path.contains(".deepseek"));
        assert!(path.contains("notes.md"));
    }

    #[test]
    fn test_note_prefers_existing_codewhale_notes_file() {
        let tmpdir = TempDir::new().unwrap();
        let codewhale_dir = tmpdir.path().join(".codewhale");
        std::fs::create_dir_all(&codewhale_dir).unwrap();
        let codewhale_notes = codewhale_dir.join("notes.md");
        std::fs::write(&codewhale_notes, "---\nexisting codewhale note").unwrap();

        let workspace = fake_workspace(&tmpdir);
        let path = message(note(&workspace.path, Some("path")));
        assert!(path.contains(".codewhale"));
        assert!(path.contains("notes.md"));
        assert!(!path.contains(".deepseek"));
    }

    #[test]
    fn test_note_rejects_out_of_range_index() {
        let tmpdir = TempDir::new().unwrap();
        let workspace = fake_workspace(&tmpdir);
        note(&workspace.path, Some("Only note"));

        let result = note(&workspace.path, Some("show 2"));
        assert!(result.message.unwrap().contains("out of range"));
    }

    #[test]
    fn test_parse_notes_handles_plain_text_before_separator() {
        let parsed = parse_notes("plain note\n---\nseparated note");
        assert_eq!(parsed, vec!["plain note", "separated note"]);
    }

    #[test]
    fn note_registration_declares_exactly_workspace() {
        let CommandHandler::Contextual {
            capabilities,
            handler,
        } = NoteCmd::handler()
        else {
            panic!("note must be contextual");
        };
        assert_eq!(capabilities, CommandCapabilities::WORKSPACE);
        assert!(!capabilities.contains(CommandCapabilities::MEMORY));
        assert!(!capabilities.contains(CommandCapabilities::PRESENTATION));
        assert!(!capabilities.contains(CommandCapabilities::MEDIA));

        // Missing WORKSPACE fails safely instead of panicking.
        let missing = handler(CommandContexts::empty(), Some("list"));
        assert!(missing.is_error);
        assert_eq!(
            missing.message.as_deref(),
            Some("Error: Command capability unavailable: workspace")
        );
        assert_eq!(NoteCmd::info().description_key, "cmd_note_description");
        assert_eq!(NoteCmd::info().name, "note");
        assert_eq!(NoteCmd::info().aliases, &[] as &[&str]);
    }
}
