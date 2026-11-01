//! Project context pack: a deterministic, bounded snapshot of the workspace
//! tree (sorted entries, README excerpt, config/source classification) that is
//! injected as `<project_context_pack>` and reused for the ephemeral
//! auto-generated context fallback.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;

use serde::Serialize;

const PACK_README_MAX_CHARS: usize = 4_000;
const PACK_MAX_ENTRIES: usize = 220;
const PACK_MAX_SOURCE_FILES: usize = 60;
const PACK_MAX_CONFIG_FILES: usize = 60;
const PACK_MAX_DEPTH: usize = 4;
const PACK_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".worktrees",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    "target",
    ".idea",
    ".vscode",
    ".pytest_cache",
    ".DS_Store",
];
const PACK_ALLOWED_HIDDEN_DIRS: &[&str] = &[".github"];
const PACK_ALLOWED_HIDDEN_FILES: &[&str] = &[".editorconfig", ".gitattributes", ".gitignore"];
const PACK_IGNORED_FILE_NAMES: &[&str] = &[".DS_Store"];
const PACK_IGNORED_FILE_EXTENSIONS: &[&str] = &[
    "7z", "avif", "db", "gif", "gz", "ico", "jpeg", "jpg", "log", "mov", "mp3", "mp4", "pdf",
    "png", "sqlite", "tar", "tgz", "wav", "webp", "zip",
];

#[derive(Debug, Serialize)]
struct ProjectContextPack {
    project_name: String,
    directory_structure: Vec<String>,
    readme: Option<ReadmePack>,
    config_files: Vec<String>,
    key_source_files: Vec<String>,
    counts: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct ReadmePack {
    path: String,
    excerpt: String,
}

/// Generate a deterministic, cache-friendly project context pack.
///
/// The pack intentionally uses only stable workspace facts: relative paths,
/// sorted entries, bounded README text, and sorted JSON object fields. It does
/// not include timestamps, random ids, absolute temp paths, or live git state.
pub fn generate_project_context_pack(workspace: &Path) -> Option<String> {
    let pack = build_project_context_pack(workspace)?;
    let json = serde_json::to_string_pretty(&pack).ok()?;
    Some(format!(
        "## Project Context Pack\n\n<project_context_pack>\n{json}\n</project_context_pack>"
    ))
}

pub fn generate_bounded_project_overview(workspace: &Path) -> Option<String> {
    let pack = build_project_context_pack(workspace)?;
    let json = serde_json::to_string_pretty(&pack).ok()?;
    Some(format!(
        "## Bounded Project Overview\n\n```json\n{json}\n```"
    ))
}

fn build_project_context_pack(workspace: &Path) -> Option<ProjectContextPack> {
    let mut entries = Vec::new();
    collect_pack_entries(workspace, workspace, 0, &mut entries);
    sort_pack_paths(&mut entries);
    entries.truncate(PACK_MAX_ENTRIES);

    let mut config_files = entries
        .iter()
        .filter(|path| is_config_file(path))
        .take(PACK_MAX_CONFIG_FILES)
        .cloned()
        .collect::<Vec<_>>();
    sort_pack_paths(&mut config_files);

    let mut key_source_files = entries
        .iter()
        .filter(|path| is_source_file(path))
        .take(PACK_MAX_SOURCE_FILES)
        .cloned()
        .collect::<Vec<_>>();
    sort_pack_paths(&mut key_source_files);

    let readme = read_readme_excerpt(workspace, &entries);
    let mut counts = BTreeMap::new();
    counts.insert("config_files".to_string(), config_files.len());
    counts.insert("directory_entries".to_string(), entries.len());
    counts.insert("key_source_files".to_string(), key_source_files.len());

    Some(ProjectContextPack {
        project_name: workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace")
            .to_string(),
        directory_structure: entries,
        readme,
        config_files,
        key_source_files,
        counts,
    })
}

fn collect_pack_entries(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > PACK_MAX_DEPTH || out.len() >= PACK_MAX_ENTRIES {
        return;
    }

    let mut queue = VecDeque::new();
    queue.push_back((dir.to_path_buf(), depth));

    while let Some((current_dir, current_depth)) = queue.pop_front() {
        if current_depth > PACK_MAX_DEPTH || out.len() >= PACK_MAX_ENTRIES {
            continue;
        }

        let Ok(read_dir) = fs::read_dir(&current_dir) else {
            continue;
        };
        let mut children = read_dir.filter_map(Result::ok).collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.path());

        for entry in children {
            if out.len() >= PACK_MAX_ENTRIES {
                break;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && should_ignore_pack_dir(name) {
                continue;
            }
            if file_type.is_file() && should_ignore_pack_file(name) {
                continue;
            }

            if let Some(relative) = relative_slash_path(root, &path) {
                if file_type.is_dir() {
                    out.push(format!("{relative}/"));
                    if current_depth < PACK_MAX_DEPTH {
                        queue.push_back((path, current_depth + 1));
                    }
                } else if file_type.is_file() {
                    out.push(relative);
                }
            }
        }
    }
}

fn should_ignore_pack_dir(name: &str) -> bool {
    PACK_IGNORED_DIRS.contains(&name)
        || (name.starts_with('.') && !PACK_ALLOWED_HIDDEN_DIRS.contains(&name))
}

fn should_ignore_pack_file(name: &str) -> bool {
    if name.starts_with('.') && !PACK_ALLOWED_HIDDEN_FILES.contains(&name) {
        return true;
    }
    if PACK_IGNORED_FILE_NAMES.contains(&name) {
        return true;
    }
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    PACK_IGNORED_FILE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

fn relative_slash_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        parts.push(component.as_os_str().to_string_lossy().to_string());
    }
    normalize_pack_relative_path(&parts.join("/"))
}

fn normalize_pack_relative_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        parts.push(part);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn sort_pack_paths(paths: &mut [String]) {
    paths.sort_by(|a, b| {
        pack_path_priority(a)
            .cmp(&pack_path_priority(b))
            .then_with(|| pack_path_sort_key(a).cmp(&pack_path_sort_key(b)))
            .then_with(|| a.cmp(b))
    });
}

fn pack_path_sort_key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn pack_path_priority(path: &str) -> u8 {
    let lower = pack_path_sort_key(path);
    let name = lower.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if matches!(name, "readme.md" | "readme.txt" | "readme") {
        0
    } else if is_config_file(&lower) {
        1
    } else if is_source_file(&lower) {
        2
    } else if lower.ends_with('/') {
        3
    } else {
        4
    }
}

fn read_readme_excerpt(workspace: &Path, entries: &[String]) -> Option<ReadmePack> {
    let path = entries
        .iter()
        .find(|path| {
            let lower = path.to_ascii_lowercase();
            lower == "readme.md" || lower == "readme.txt" || lower == "readme"
        })?
        .clone();
    let raw = fs::read_to_string(workspace.join(&path)).ok()?;
    let excerpt = truncate_chars(raw.trim(), PACK_README_MAX_CHARS);
    if excerpt.is_empty() {
        None
    } else {
        Some(ReadmePack { path, excerpt })
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>()
}

fn is_config_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(lower.as_str());
    matches!(
        name,
        "cargo.toml"
            | "package.json"
            | "tsconfig.json"
            | "pyproject.toml"
            | "requirements.txt"
            | "go.mod"
            | "config.toml"
            | "deepseek.toml"
            | "dockerfile"
            | "compose.yaml"
            | "compose.yml"
            | "docker-compose.yaml"
            | "docker-compose.yml"
            | "makefile"
    ) || lower.ends_with(".config.js")
        || lower.ends_with(".config.ts")
        || lower.ends_with(".toml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
}

fn is_source_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    matches!(
        lower.rsplit('.').next(),
        Some(
            "rs" | "py"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "go"
                | "java"
                | "kt"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
                | "cs"
                | "rb"
                | "php"
                | "swift"
                | "sql"
                | "sh"
                | "bash"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn project_context_pack_is_stable_and_sorted() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("README.md"), "# Demo\n\nReadme body").expect("write");
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"demo\"").expect("write");
        fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
        fs::write(tmp.path().join("src").join("z.rs"), "mod z;").expect("write z");
        fs::write(tmp.path().join("src").join("a.rs"), "mod a;").expect("write a");
        fs::create_dir_all(tmp.path().join("node_modules").join("pkg")).expect("mkdir ignored");
        fs::write(
            tmp.path().join("node_modules").join("pkg").join("index.js"),
            "ignored",
        )
        .expect("write ignored");

        let first = generate_project_context_pack(tmp.path()).expect("pack");
        let second = generate_project_context_pack(tmp.path()).expect("pack again");

        assert_eq!(first, second);
        assert!(first.contains("\"project_name\""));
        assert!(first.contains("\"directory_structure\""));
        assert!(first.contains("\"README.md\""));
        assert!(first.contains("\"Cargo.toml\""));
        assert!(first.contains("\"src/a.rs\""));
        assert!(first.contains("\"src/z.rs\""));
        assert!(!first.contains("node_modules"));
        assert!(
            first.find("\"src/a.rs\"").expect("a before z")
                < first.find("\"src/z.rs\"").expect("z")
        );
    }

    #[test]
    fn project_context_pack_ignores_agent_state_and_binary_noise() {
        let tmp = tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
        fs::write(tmp.path().join("src").join("main.rs"), "fn main() {}").expect("write src");
        fs::write(tmp.path().join(".DS_Store"), "noise").expect("write ds store");
        fs::write(tmp.path().join("paper.pdf"), "not a real pdf").expect("write pdf");
        fs::create_dir_all(tmp.path().join(".codewhale").join("state")).expect("mkdir state");
        fs::write(
            tmp.path()
                .join(".codewhale")
                .join("state")
                .join("subagents.v1.json"),
            "{}",
        )
        .expect("write state");
        fs::create_dir_all(tmp.path().join(".playwright-mcp")).expect("mkdir playwright");
        fs::write(
            tmp.path().join(".playwright-mcp").join("trace.log"),
            "noise",
        )
        .expect("write log");
        fs::create_dir_all(tmp.path().join(".agents").join("skills").join("demo"))
            .expect("mkdir skills");
        fs::write(
            tmp.path()
                .join(".agents")
                .join("skills")
                .join("demo")
                .join("SKILL.md"),
            "skill body",
        )
        .expect("write skill");
        fs::create_dir_all(tmp.path().join(".github").join("workflows")).expect("mkdir workflows");
        fs::write(
            tmp.path().join(".github").join("workflows").join("ci.yml"),
            "name: ci",
        )
        .expect("write workflow");

        let pack = generate_project_context_pack(tmp.path()).expect("pack");

        assert!(pack.contains("\"src/main.rs\""), "{pack}");
        assert!(pack.contains("\".github/\""), "{pack}");
        assert!(pack.contains("\".github/workflows/ci.yml\""), "{pack}");
        assert!(!pack.contains(".deepseek"), "{pack}");
        assert!(!pack.contains(".playwright-mcp"), "{pack}");
        assert!(!pack.contains(".agents"), "{pack}");
        assert!(!pack.contains(".DS_Store"), "{pack}");
        assert!(!pack.contains("paper.pdf"), "{pack}");
        assert!(!pack.contains("trace.log"), "{pack}");
    }

    #[test]
    fn project_context_pack_keeps_later_top_level_dirs_under_budget() {
        let tmp = tempdir().expect("tempdir");
        let noisy = tmp.path().join("aaa-many-files");
        fs::create_dir_all(&noisy).expect("mkdir noisy");
        for i in 0..(PACK_MAX_ENTRIES + 20) {
            fs::write(noisy.join(format!("file-{i:03}.rs")), "fn f() {}").expect("write noisy");
        }
        fs::create_dir_all(tmp.path().join("zzz-important")).expect("mkdir important");
        fs::write(
            tmp.path().join("zzz-important").join("main.rs"),
            "fn important() {}",
        )
        .expect("write important");

        let pack = generate_project_context_pack(tmp.path()).expect("pack");

        assert!(
            pack.contains("\"zzz-important/\""),
            "breadth-first packing should keep later top-level directories visible:\n{pack}"
        );
    }

    #[test]
    fn project_context_pack_sort_is_cross_platform_and_priority_aware() {
        let mut unix_paths = vec![
            "src/z.rs".to_string(),
            "docs/".to_string(),
            "README.md".to_string(),
            "Cargo.toml".to_string(),
            "src/a.rs".to_string(),
            "notes.txt".to_string(),
        ];
        let mut windows_paths = vec![
            "src\\z.rs".to_string(),
            "docs\\".to_string(),
            "README.md".to_string(),
            "Cargo.toml".to_string(),
            "src\\a.rs".to_string(),
            "notes.txt".to_string(),
        ];

        sort_pack_paths(&mut unix_paths);
        sort_pack_paths(&mut windows_paths);

        let normalized_windows = windows_paths
            .iter()
            .map(|path| path.replace('\\', "/"))
            .collect::<Vec<_>>();
        assert_eq!(unix_paths, normalized_windows);
        assert_eq!(
            unix_paths,
            vec![
                "README.md",
                "Cargo.toml",
                "src/a.rs",
                "src/z.rs",
                "docs/",
                "notes.txt",
            ]
        );
    }

    #[test]
    fn normalize_pack_relative_path_rejects_parent_segments() {
        assert_eq!(
            normalize_pack_relative_path(".\\src\\main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(normalize_pack_relative_path("../secret.txt"), None);
    }
}
