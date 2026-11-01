//! Process-local cache for project context loading.
//!
//! The project-context loader sits on prompt/session hot paths and repeatedly
//! checks the same workspace, parent, global, constitution, and trust files.
//! This cache avoids rereading unchanged context while keeping the signature
//! broad enough for the loader's side effects and authority surfaces.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::project_context::ProjectContext;

const DEFAULT_CAPACITY: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    workspace: PathBuf,
    signature: ContentSignature,
    settings: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct ContentSignature {
    entries: Vec<ContentEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContentEntry {
    path: PathBuf,
    read_root: Option<PathBuf>,
    canonical_path: Option<PathBuf>,
    symlink_components: Vec<PathBuf>,
    fingerprint: Option<String>,
}

#[derive(Debug, Default)]
struct WorkspaceCache {
    by_key: HashMap<CacheKey, ProjectContext>,
    order: VecDeque<CacheKey>,
}

thread_local! {
    static CACHE: RefCell<WorkspaceCache> = RefCell::new(WorkspaceCache::default());
}

pub fn lookup(key: &CacheKey) -> Option<ProjectContext> {
    CACHE.with(|cache| cache.borrow().by_key.get(key).cloned())
}

pub fn store(key: CacheKey, value: ProjectContext) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.by_key.insert(key.clone(), value).is_none() {
            cache.order.push_back(key);
        }
        while cache.by_key.len() > DEFAULT_CAPACITY {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            cache.by_key.remove(&oldest);
        }
    });
}

/// Drop every cached entry.
///
/// Used by tests.
#[cfg(any(test, feature = "test-support"))]
pub fn clear() {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.by_key.clear();
        cache.order.clear();
    });
}

#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn compute_cache_key(workspace: &Path, home_dir: Option<&Path>) -> CacheKey {
    compute_cache_key_with_settings(
        workspace,
        &crate::project_context::InstructionSettings {
            home: home_dir.map(Path::to_path_buf),
            ..Default::default()
        },
    )
}

pub fn compute_cache_key_with_settings(
    workspace: &Path,
    settings: &crate::project_context::InstructionSettings,
) -> CacheKey {
    let workspace = canonicalize_or_keep(workspace);
    CacheKey {
        signature: ContentSignature::for_loader(&workspace, settings.home.as_deref()),
        workspace,
        settings: format!("{settings:?}"),
    }
}

impl ContentSignature {
    fn for_loader(workspace: &Path, home_dir: Option<&Path>) -> Self {
        let mut entries: Vec<ContentEntry> =
            crate::project_context::project_context_cache_candidate_paths(workspace, home_dir)
                .into_iter()
                .map(|(path, read_root)| ContentEntry {
                    canonical_path: std::fs::canonicalize(&path).ok(),
                    symlink_components: path
                        .ancestors()
                        .filter(|component| {
                            std::fs::symlink_metadata(component)
                                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                        })
                        .map(Path::to_path_buf)
                        .collect(),
                    fingerprint: file_fingerprint(&path, read_root.as_deref()),
                    path,
                    read_root,
                })
                .collect();

        entries.sort_by(|a, b| (&a.path, &a.read_root).cmp(&(&b.path, &b.read_root)));
        entries.dedup_by(|a, b| a.path == b.path && a.read_root == b.read_root);

        Self { entries }
    }
}

/// Largest candidate hashed by content. The context loader skips files over
/// its 8 MiB ceiling whatever they hold, so a larger candidate is
/// fingerprinted by length and mtime instead of being read on every refresh.
const FINGERPRINT_MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

fn file_fingerprint(path: &Path, read_root: Option<&Path>) -> Option<String> {
    let file = match read_root.map_or_else(
        || codewhale_util::fs_confined::open_user_read(path),
        |root| codewhale_util::fs_confined::open_read(root, path),
    ) {
        Ok(file) => file,
        Err(error) => return Some(format!("unreadable:{error}")),
    };
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return Some("non-file".to_string());
    }
    let modified = || {
        metadata
            .modified()
            .ok()
            .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| format!("{}:{}", duration.as_secs(), duration.subsec_nanos()))
            .unwrap_or_else(|| "unknown".to_string())
    };
    if metadata.len() > FINGERPRINT_MAX_READ_BYTES {
        return Some(format!("oversize:{}:{}", metadata.len(), modified()));
    }

    let read: std::io::Result<_> = (|| {
        use std::io::Read as _;
        let mut hasher = Sha256::new();
        let mut limited = file.take(FINGERPRINT_MAX_READ_BYTES + 1);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = limited.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.finalize())
    })();
    match read {
        Ok(digest) => Some(format!("sha256:{}", to_hex(&digest))),
        Err(error) => Some(format!(
            "unreadable:{}:{}:{error}",
            metadata.len(),
            modified()
        )),
    }
}

fn canonicalize_or_keep(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn cache_round_trip() {
        clear();
        let key = CacheKey {
            workspace: PathBuf::from("/tmp/context-cache-round-trip"),
            signature: ContentSignature::default(),
            settings: String::new(),
        };
        let ctx = ProjectContext::empty(PathBuf::from("/tmp/context-cache-round-trip"));

        store(key.clone(), ctx.clone());

        let got = lookup(&key).expect("cache hit");
        assert_eq!(got.project_root, ctx.project_root);
    }

    #[test]
    fn store_does_not_grow_unbounded() {
        clear();
        for i in 0..(DEFAULT_CAPACITY + 4) {
            let key = CacheKey {
                workspace: PathBuf::from(format!("/tmp/workspace-{i}")),
                signature: ContentSignature::default(),
                settings: String::new(),
            };
            store(key, ProjectContext::empty(PathBuf::from("/tmp")));
        }

        let count = CACHE.with(|cache| cache.borrow().by_key.len());
        assert!(count <= DEFAULT_CAPACITY, "cache held {count} entries");
    }

    #[test]
    fn cache_key_canonicalizes_equivalent_workspace_paths() {
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let plain = compute_cache_key(workspace.path(), Some(home.path()));
        let dotted = compute_cache_key(&workspace.path().join("."), Some(home.path()));

        assert_eq!(plain.workspace, dotted.workspace);
    }

    #[test]
    fn signature_changes_when_agents_md_is_overwritten_same_length() {
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        fs::write(workspace.path().join("AGENTS.md"), "alpha").expect("write alpha");
        let before = compute_cache_key(workspace.path(), Some(home.path()));

        fs::write(workspace.path().join("AGENTS.md"), "bravo").expect("write bravo");
        let after = compute_cache_key(workspace.path(), Some(home.path()));

        assert_ne!(before, after);
    }

    #[test]
    fn signature_changes_when_constitution_json_changes() {
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        fs::create_dir(workspace.path().join(".git")).expect("mkdir git");
        fs::create_dir(workspace.path().join(".codewhale")).expect("mkdir codewhale");
        let constitution = workspace
            .path()
            .join(".codewhale")
            .join("constitution.json");
        fs::write(&constitution, r#"{"schema_version":1,"authority":["a"]}"#)
            .expect("write constitution a");
        let before = compute_cache_key(workspace.path(), Some(home.path()));

        fs::write(&constitution, r#"{"schema_version":1,"authority":["b"]}"#)
            .expect("write constitution b");
        let after = compute_cache_key(workspace.path(), Some(home.path()));

        assert_ne!(before, after);
    }

    #[test]
    fn signature_changes_when_rules_file_changes() {
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let rules_dir = workspace.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(rules_dir.join("rule.md"), "alpha").expect("write alpha");

        let before = compute_cache_key(workspace.path(), Some(home.path()));

        fs::write(rules_dir.join("rule.md"), "bravo").expect("write bravo");
        let after = compute_cache_key(workspace.path(), Some(home.path()));

        assert_ne!(
            before, after,
            "cache key must change when rules file changes"
        );
    }

    #[test]
    fn signature_changes_when_rules_file_is_added_or_removed() {
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let rules_dir = workspace.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");

        // No rules yet
        let before = compute_cache_key(workspace.path(), Some(home.path()));

        fs::write(rules_dir.join("new.md"), "content").expect("write new.md");
        let after = compute_cache_key(workspace.path(), Some(home.path()));

        assert_ne!(
            before, after,
            "cache key must change when rules file is added"
        );
    }

    #[test]
    fn signature_tracks_foreign_fragment_add_change_and_remove() {
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let cursor_rules = workspace.path().join(".cursor/rules");
        fs::create_dir_all(cursor_rules.join("nested")).expect("mkdir cursor rules");

        let before = compute_cache_key(workspace.path(), Some(home.path()));

        // Files the bounded fragment loader cannot select must not churn the
        // project-context cache.
        fs::write(cursor_rules.join("settings.json"), "{}").expect("write ignored settings");
        let ignored = compute_cache_key(workspace.path(), Some(home.path()));
        assert_eq!(before, ignored, "non-Markdown fragment files are ignored");

        let rule = cursor_rules.join("nested/law.md");
        fs::write(&rule, "alpha").expect("write cursor rule");
        let added = compute_cache_key(workspace.path(), Some(home.path()));
        assert_ne!(ignored, added, "adding a loadable fragment must invalidate");

        fs::write(&rule, "bravo").expect("change cursor rule");
        let changed = compute_cache_key(workspace.path(), Some(home.path()));
        assert_ne!(added, changed, "changing a fragment must invalidate");

        fs::remove_file(&rule).expect("remove cursor rule");
        let removed = compute_cache_key(workspace.path(), Some(home.path()));
        assert_eq!(
            ignored, removed,
            "removing the fragment restores the prior key"
        );
    }

    #[cfg(unix)]
    #[test]
    fn signature_does_not_follow_symlinked_foreign_fragment_directories() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let outside = tempdir().expect("outside");
        fs::write(outside.path().join("law.md"), "outside law").expect("write outside rule");
        fs::create_dir_all(workspace.path().join(".cursor")).expect("mkdir cursor");

        let before = compute_cache_key(workspace.path(), Some(home.path()));
        symlink(outside.path(), workspace.path().join(".cursor/rules"))
            .expect("symlink outside rules");
        let after = compute_cache_key(workspace.path(), Some(home.path()));

        assert_eq!(
            before, after,
            "cache fingerprinting must not read through a rejected fragment-directory symlink"
        );
    }
}
