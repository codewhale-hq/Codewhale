//! Single source of truth for skill root enumeration, ownership, scope, and
//! runtime precedence.
//!
//! Runtime discovery and (later) audit/mutation share this catalog so
//! precedence cannot drift between modules. Discovery directories are not
//! write targets: only explicitly owned CodeWhale roots are writable.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Stable identifier for a skill root within a catalog snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillRootId(String);

impl std::fmt::Display for SkillRootId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// External harness layouts that CodeWhale can discover/audit but never owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompatibleHarness {
    Agents,
    Claude,
    Cursor,
    OpenCode,
    Codex,
    DeepSeekLegacy,
    /// Flat `<workspace>/skills` layout.
    FlatProjectSkills,
}

impl CompatibleHarness {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Codex => "codex",
            Self::DeepSeekLegacy => "deepseek",
            Self::FlatProjectSkills => "flat-skills",
        }
    }
}

/// Kind of skill root on disk (or logical source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillRootKind {
    CodeWhaleProject,
    CodeWhaleGlobal,
    CompatibleProject(CompatibleHarness),
    CompatibleGlobal(CompatibleHarness),
    /// Explicitly configured `skills_dir` that is not one of the owned roots.
    Configured,
    // Matched by the extensions UI + audit provenance, never constructed:
    // no discovery path produces these roots yet (#4651 follow-up never came).
    #[allow(dead_code)]
    BuiltIn,
    #[allow(dead_code)]
    ReviewedPluginSnapshot,
    RegistryCache,
}

/// Whether CodeWhale may mutate files under this root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillRootAccess {
    /// CodeWhale-owned project/global install targets.
    WritableOwned,
    /// Compatible harness roots and unclassified configured dirs — read only.
    ReadOnlyExternal,
    /// Built-in / reviewed plugin snapshot content.
    // Never constructed: no discovery path assigns it yet (#4651 follow-up
    // never came). Kept because the access taxonomy is meaningless without it.
    #[allow(dead_code)]
    Immutable,
    /// Registry download cache — not an active install target.
    CacheOnly,
}

/// Project vs global scope for owned and compatible roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillScope {
    Project,
    Global,
    /// Logical / non-filesystem sources (built-in, plugin snapshot, cache).
    Logical,
}

/// One enumerated skill root with ownership and precedence metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRootDescriptor {
    pub id: SkillRootId,
    pub kind: SkillRootKind,
    pub access: SkillRootAccess,
    pub scope: SkillScope,
    pub path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    /// Lower value = higher precedence for first-wins runtime merge.
    pub precedence: Option<usize>,
    /// When true, runtime skill discovery includes this root.
    pub active_for_runtime: bool,
    /// When true, owned-only / compatible audit may include this root.
    pub active_for_audit: bool,
}

impl SkillRootDescriptor {
    #[must_use]
    pub fn is_writable_owned(&self) -> bool {
        self.access == SkillRootAccess::WritableOwned
    }
}

/// Catalog of skill roots for a workspace (+ optional HOME override for tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRootCatalog {
    roots: Vec<SkillRootDescriptor>,
}

impl SkillRootCatalog {
    /// Build the full catalog: owned + compatible (including Codex audit-only)
    /// plus optional configured dir and logical sources.
    #[must_use]
    pub fn build(
        workspace: &Path,
        home_dir: Option<&Path>,
        configured_skills_dir: Option<&Path>,
    ) -> Self {
        let mut roots = Vec::new();
        let mut precedence = 0usize;

        // Codewhale-owned roots win within each scope: installing or
        // updating a skill must change the copy runtime discovery selects.
        push_descriptor(
            &mut roots,
            &mut precedence,
            SkillRootKind::CodeWhaleProject,
            SkillRootAccess::WritableOwned,
            SkillScope::Project,
            workspace.join(".codewhale/skills"),
            true,
            true,
            "project-codewhale",
            true,
        );
        for (harness, directory, id) in [
            (CompatibleHarness::Agents, ".agents", "project-agents"),
            (CompatibleHarness::Claude, ".claude", "project-claude"),
            (CompatibleHarness::OpenCode, ".opencode", "project-opencode"),
            (CompatibleHarness::Cursor, ".cursor", "project-cursor"),
        ] {
            push_existing(
                &mut roots,
                &mut precedence,
                SkillRootKind::CompatibleProject(harness),
                SkillRootAccess::ReadOnlyExternal,
                SkillScope::Project,
                workspace.join(directory).join("skills"),
                true,
                true,
                id,
            );
        }
        // Product repositories often keep ordinary content in skills/.
        // It is an audit candidate until session configuration opts in.
        push_existing(
            &mut roots,
            &mut precedence,
            SkillRootKind::CompatibleProject(CompatibleHarness::FlatProjectSkills),
            SkillRootAccess::ReadOnlyExternal,
            SkillScope::Project,
            workspace.join("skills"),
            false,
            true,
            "project-flat-skills",
        );

        // Codex project: audit-compatible only; never active for runtime in #4651.
        push_existing(
            &mut roots,
            &mut precedence,
            SkillRootKind::CompatibleProject(CompatibleHarness::Codex),
            SkillRootAccess::ReadOnlyExternal,
            SkillScope::Project,
            workspace.join(".codex").join("skills"),
            false,
            true,
            "project-codex",
        );

        if let Some(global_owned) = owned_global_skills_dir(home_dir) {
            push_descriptor(
                &mut roots,
                &mut precedence,
                SkillRootKind::CodeWhaleGlobal,
                SkillRootAccess::WritableOwned,
                SkillScope::Global,
                global_owned,
                true,
                true,
                "global-codewhale",
                true,
            );
        }
        if let Some(home) = home_dir {
            for (harness, directory, id) in [
                (CompatibleHarness::Agents, ".agents", "global-agents"),
                (CompatibleHarness::Claude, ".claude", "global-claude"),
                (
                    CompatibleHarness::DeepSeekLegacy,
                    ".deepseek",
                    "global-deepseek",
                ),
            ] {
                push_existing(
                    &mut roots,
                    &mut precedence,
                    SkillRootKind::CompatibleGlobal(harness),
                    SkillRootAccess::ReadOnlyExternal,
                    SkillScope::Global,
                    home.join(directory).join("skills"),
                    true,
                    true,
                    id,
                );
            }

            // Codex global: audit-compatible only.
            push_existing(
                &mut roots,
                &mut precedence,
                SkillRootKind::CompatibleGlobal(CompatibleHarness::Codex),
                SkillRootAccess::ReadOnlyExternal,
                SkillScope::Global,
                home.join(".codex").join("skills"),
                false,
                true,
                "global-codex",
            );

            // Registry cache is never an active skill root.
            let cache = home.join(".codewhale").join("cache").join("skills");
            push_descriptor(
                &mut roots,
                &mut precedence,
                SkillRootKind::RegistryCache,
                SkillRootAccess::CacheOnly,
                SkillScope::Logical,
                cache,
                false,
                false,
                "registry-cache",
                false,
            );
        }

        if let Some(configured) = configured_skills_dir {
            insert_configured_root(&mut roots, workspace, home_dir, configured, &mut precedence);
        }

        Self { roots }
    }

    /// Apply session policy without changing ownership or audit visibility.
    pub fn with_flat_workspace_root(mut self, enabled: bool) -> Self {
        for root in &mut self.roots {
            if root.kind == SkillRootKind::CompatibleProject(CompatibleHarness::FlatProjectSkills) {
                root.active_for_runtime = enabled;
            }
        }
        self
    }

    /// Paths used by runtime discovery for the given mode (existing dirs only,
    /// first-wins order preserved). CodeWhale-only applies the workspace
    /// containment check for the project owned root.
    #[must_use]
    pub fn runtime_directories(
        &self,
        workspace: &Path,
        mode: super::SkillDiscoveryMode,
    ) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        // Repository-supplied skills are instructions the user never
        // reviewed; they load only once the workspace is trusted. Resolved
        // lazily so a workspace without project skill dirs never reads config.
        let mut workspace_trusted = None;

        for root in &self.roots {
            if !root.active_for_runtime {
                continue;
            }
            if root.scope == SkillScope::Project
                && path_is_existing_dir(&root.path)
                && !*workspace_trusted.get_or_insert_with(|| {
                    codewhale_runtime::config::is_workspace_trusted(workspace)
                })
            {
                continue;
            }
            match mode {
                super::SkillDiscoveryMode::Compatible
                | super::SkillDiscoveryMode::CompatibleWithFlatWorkspace => {}
                super::SkillDiscoveryMode::CodeWhaleOnly => {
                    if !matches!(
                        root.kind,
                        SkillRootKind::CodeWhaleProject
                            | SkillRootKind::CodeWhaleGlobal
                            | SkillRootKind::Configured
                    ) {
                        continue;
                    }
                    if root.kind == SkillRootKind::CodeWhaleProject
                        && !codewhale_project_root_is_inside_workspace(workspace, &root.path)
                    {
                        continue;
                    }
                }
            }

            if !path_is_existing_dir(&root.path) {
                continue;
            }
            let Ok(canonical) = fs::canonicalize(&root.path) else {
                continue;
            };
            if !canonical.is_dir() || !seen.insert(canonical) {
                continue;
            }
            out.push(root.path.clone());
        }
        out
    }

    /// Owned CodeWhale project + global roots (may not exist yet).
    #[must_use]
    pub fn owned_writable_roots(&self) -> Vec<&SkillRootDescriptor> {
        self.roots
            .iter()
            .filter(|r| r.is_writable_owned())
            .collect()
    }

    /// Roots eligible for owned-only audit (writable owned roots that exist).
    #[must_use]
    pub fn audit_owned_directories(&self) -> Vec<&SkillRootDescriptor> {
        self.roots
            .iter()
            .filter(|r| {
                r.is_writable_owned() && r.active_for_audit && path_is_existing_dir(&r.path)
            })
            .collect()
    }

    /// Owned + compatible roots for explicit `--compatible` audit, including
    /// Codex. Does not change runtime activation.
    #[must_use]
    pub fn audit_compatible_directories(&self) -> Vec<&SkillRootDescriptor> {
        self.roots
            .iter()
            .filter(|r| {
                r.active_for_audit
                    && !matches!(
                        r.kind,
                        SkillRootKind::RegistryCache
                            | SkillRootKind::BuiltIn
                            | SkillRootKind::ReviewedPluginSnapshot
                    )
                    && path_is_existing_dir(&r.path)
            })
            .collect()
    }
}

/// Project skill directories that exist but were not loaded because the
/// workspace is not trusted, so discovery can say so instead of dropping them
/// silently.
#[must_use]
pub fn untrusted_project_skill_dirs(
    workspace: &Path,
    home_dir: Option<&Path>,
    configured_skills_dir: Option<&Path>,
    mode: super::SkillDiscoveryMode,
) -> Vec<PathBuf> {
    let catalog = SkillRootCatalog::build(workspace, home_dir, configured_skills_dir)
        .with_flat_workspace_root(mode.flat_workspace_root());
    let present: Vec<PathBuf> = catalog
        .roots
        .iter()
        .filter(|root| {
            let explicit = configured_skills_dir
                .is_some_and(|configured| paths_refer_to_same_dir(configured, &root.path));
            root.scope == SkillScope::Project
                && (root.active_for_runtime || explicit)
                && (mode != super::SkillDiscoveryMode::CodeWhaleOnly
                    || root.is_writable_owned()
                    || explicit)
                && path_is_existing_dir(&root.path)
        })
        .map(|root| root.path.clone())
        .collect();
    if present.is_empty() || codewhale_runtime::config::is_workspace_trusted(workspace) {
        return Vec::new();
    }
    present
}

/// Whether `skills_dir` may load right now. A directory in project scope
/// (repository-supplied, resolving inside the workspace) is held to the same
/// workspace-trust gate as [`SkillRootCatalog::runtime_directories`], so an
/// explicit or resolved skills dir cannot re-admit what the catalog filtered.
/// A session rooted at the home directory is exempt: every path under it is
/// the user's own global content, which the catalog also loads as global.
#[must_use]
pub fn skills_dir_allowed_by_workspace_trust(
    workspace: &Path,
    home_dir: Option<&Path>,
    skills_dir: &Path,
) -> bool {
    classify_configured_skills_dir(workspace, home_dir, skills_dir).2 != SkillScope::Project
        || home_dir.is_some_and(|home| paths_refer_to_same_dir(home, workspace))
        || codewhale_runtime::config::is_workspace_trusted(workspace)
}

/// Resolve candidate skill directories for runtime discovery (existing paths
/// only), preserving historical precedence.
#[must_use]
pub fn skills_directories_with_home_and_mode(
    workspace: &Path,
    home_dir: Option<&Path>,
    mode: super::SkillDiscoveryMode,
) -> Vec<PathBuf> {
    SkillRootCatalog::build(workspace, home_dir, None)
        .with_flat_workspace_root(mode.flat_workspace_root())
        .runtime_directories(workspace, mode)
}

/// CodeWhale project skills dir when it exists and stays inside the workspace.
#[must_use]
pub fn codewhale_workspace_skills_dir(workspace: &Path) -> Option<PathBuf> {
    let skills_dir = workspace.join(".codewhale").join("skills");
    codewhale_project_root_is_inside_workspace(workspace, &skills_dir).then_some(skills_dir)
}

/// Filter candidate paths to existing directories, preserving order and
/// de-duplicating by canonical path.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn existing_skill_dirs(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for path in candidates {
        let Ok(canonical_path) = fs::canonicalize(&path) else {
            continue;
        };
        if canonical_path.is_dir() && seen.insert(canonical_path) {
            out.push(path);
        }
    }
    out
}

/// Classify a configured `skills_dir`: owned only when it is exactly a
/// CodeWhale project/global root; compatible harness paths stay read-only.
#[must_use]
pub fn classify_configured_skills_dir(
    workspace: &Path,
    home_dir: Option<&Path>,
    skills_dir: &Path,
) -> (SkillRootKind, SkillRootAccess, SkillScope) {
    let project_owned = workspace.join(".codewhale").join("skills");
    if paths_refer_to_same_dir(&project_owned, skills_dir) {
        return (
            SkillRootKind::CodeWhaleProject,
            SkillRootAccess::WritableOwned,
            SkillScope::Project,
        );
    }
    if let Some(global_owned) = owned_global_skills_dir(home_dir)
        && paths_refer_to_same_dir(&global_owned, skills_dir)
    {
        return (
            SkillRootKind::CodeWhaleGlobal,
            SkillRootAccess::WritableOwned,
            SkillScope::Global,
        );
    }

    if let Some(harness) = match_compatible_project(workspace, skills_dir) {
        return (
            SkillRootKind::CompatibleProject(harness),
            SkillRootAccess::ReadOnlyExternal,
            SkillScope::Project,
        );
    }
    if let Some(home) = home_dir
        && let Some(harness) = match_compatible_global(home, skills_dir)
    {
        return (
            SkillRootKind::CompatibleGlobal(harness),
            SkillRootAccess::ReadOnlyExternal,
            SkillScope::Global,
        );
    }

    // Unknown configured path: treat as external until an explicit owned-root
    // marker exists (Issue #4651 first cut — do not guess writability).
    let scope = fs::canonicalize(workspace)
        .ok()
        .map_or(SkillScope::Global, |root| {
            fs::canonicalize(skills_dir)
                .ok()
                .filter(|p| p.starts_with(&root))
                .map_or(SkillScope::Global, |_| SkillScope::Project)
        });
    (
        SkillRootKind::Configured,
        SkillRootAccess::ReadOnlyExternal,
        scope,
    )
}

#[must_use]
pub fn safe_display_path(path: &Path, workspace: Option<&Path>, home: Option<&Path>) -> String {
    // Prefer workspace when both apply so project roots stay distinct from
    // `~/...` global paths that happen to live under the same home tree.
    if let Some(workspace) = workspace
        && let Ok(stripped) = path.strip_prefix(workspace)
    {
        return format!("<workspace>/{}", stripped.display()).replace('\\', "/");
    }
    if let Some(home) = home
        && let Ok(stripped) = path.strip_prefix(home)
    {
        return format!("~/{}", stripped.display()).replace('\\', "/");
    }
    // Last resort: basename chain without expanding unrelated absolute parents.
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[must_use]
pub fn paths_refer_to_same_dir(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn codewhale_project_root_is_inside_workspace(workspace: &Path, skills_dir: &Path) -> bool {
    let Ok(canonical_workspace) = fs::canonicalize(workspace) else {
        return false;
    };
    let Ok(canonical_skills) = fs::canonicalize(skills_dir) else {
        return false;
    };
    canonical_skills.is_dir() && canonical_skills.starts_with(canonical_workspace)
}

fn path_is_existing_dir(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).ok().is_some_and(|p| p.is_dir())
        }
        Ok(meta) => meta.is_dir(),
        Err(_) => false,
    }
}

fn match_compatible_project(workspace: &Path, skills_dir: &Path) -> Option<CompatibleHarness> {
    let candidates = [
        (
            CompatibleHarness::Agents,
            workspace.join(".agents").join("skills"),
        ),
        (
            CompatibleHarness::FlatProjectSkills,
            workspace.join("skills"),
        ),
        (
            CompatibleHarness::OpenCode,
            workspace.join(".opencode").join("skills"),
        ),
        (
            CompatibleHarness::Claude,
            workspace.join(".claude").join("skills"),
        ),
        (
            CompatibleHarness::Cursor,
            workspace.join(".cursor").join("skills"),
        ),
        (
            CompatibleHarness::Codex,
            workspace.join(".codex").join("skills"),
        ),
    ];
    for (harness, candidate) in candidates {
        if paths_refer_to_same_dir(&candidate, skills_dir) {
            return Some(harness);
        }
    }
    None
}

fn match_compatible_global(home: &Path, skills_dir: &Path) -> Option<CompatibleHarness> {
    let candidates = [
        (
            CompatibleHarness::Agents,
            home.join(".agents").join("skills"),
        ),
        (
            CompatibleHarness::Claude,
            home.join(".claude").join("skills"),
        ),
        (
            CompatibleHarness::DeepSeekLegacy,
            home.join(".deepseek").join("skills"),
        ),
        (CompatibleHarness::Codex, home.join(".codex").join("skills")),
    ];
    for (harness, candidate) in candidates {
        if paths_refer_to_same_dir(&candidate, skills_dir) {
            return Some(harness);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)] // catalog rows keep ownership flags explicit at call sites
fn push_existing(
    roots: &mut Vec<SkillRootDescriptor>,
    precedence: &mut usize,
    kind: SkillRootKind,
    access: SkillRootAccess,
    scope: SkillScope,
    path: PathBuf,
    active_for_runtime: bool,
    active_for_audit: bool,
    id: &str,
) {
    push_descriptor(
        roots,
        precedence,
        kind,
        access,
        scope,
        path,
        active_for_runtime,
        active_for_audit,
        id,
        false,
    );
}

#[allow(clippy::too_many_arguments)] // shared constructor for the explicit catalog table above
fn push_descriptor(
    roots: &mut Vec<SkillRootDescriptor>,
    precedence: &mut usize,
    kind: SkillRootKind,
    access: SkillRootAccess,
    scope: SkillScope,
    path: PathBuf,
    active_for_runtime: bool,
    active_for_audit: bool,
    id: &str,
    include_missing: bool,
) {
    let exists = path_is_existing_dir(&path);
    if !include_missing && !exists {
        return;
    }
    let canonical_path = fs::canonicalize(&path).ok();
    let slot = *precedence;
    *precedence += 1;
    roots.push(SkillRootDescriptor {
        id: SkillRootId(id.to_string()),
        kind,
        access,
        scope,
        path,
        canonical_path,
        precedence: Some(slot),
        active_for_runtime,
        active_for_audit,
    });
}

fn insert_configured_root(
    roots: &mut Vec<SkillRootDescriptor>,
    workspace: &Path,
    home_dir: Option<&Path>,
    skills_dir: &Path,
    precedence: &mut usize,
) {
    if !path_is_existing_dir(skills_dir) {
        return;
    }
    if roots
        .iter()
        .any(|root| paths_refer_to_same_dir(&root.path, skills_dir))
    {
        return;
    }

    let (kind, access, scope) = classify_configured_skills_dir(workspace, home_dir, skills_dir);
    let workspace_root = fs::canonicalize(workspace).ok();
    let insert_at = workspace_root
        .as_ref()
        .and_then(|root| {
            roots.iter().position(|dir| {
                fs::canonicalize(&dir.path).map_or(true, |dir| !dir.starts_with(root))
            })
        })
        .unwrap_or(roots.len());

    let canonical_path = fs::canonicalize(skills_dir).ok();
    let slot = *precedence;
    *precedence += 1;
    let descriptor = SkillRootDescriptor {
        id: SkillRootId(format!("configured-{slot}")),
        kind,
        access,
        scope,
        path: skills_dir.to_path_buf(),
        canonical_path,
        precedence: Some(slot),
        active_for_runtime: true,
        active_for_audit: true,
    };
    roots.insert(insert_at, descriptor);
    // Re-number precedence after insertion so catalog order stays consistent.
    for (idx, root) in roots.iter_mut().enumerate() {
        root.precedence = Some(idx);
    }
    *precedence = roots.len();
}

fn owned_global_skills_dir(home: Option<&Path>) -> Option<PathBuf> {
    if home == codewhale_paths::user_home().as_deref() {
        codewhale_runtime::config::owned_skills_dir()
    } else {
        home.map(|home| home.join(".codewhale/skills"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkillDiscoveryMode;
    use tempfile::TempDir;

    fn write_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn unavailable_home_has_no_ambient_global_or_cache_root() {
        let tmp = TempDir::new().unwrap();
        let catalog = SkillRootCatalog::build(tmp.path(), None, None);
        assert!(
            catalog
                .roots
                .iter()
                .all(|root| root.scope == SkillScope::Project)
        );
        let owned = catalog.owned_writable_roots();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].kind, SkillRootKind::CodeWhaleProject);
        assert_eq!(owned[0].path, tmp.path().join(".codewhale/skills"));
    }

    #[test]
    fn runtime_compatible_prioritizes_owned_workspace_roots() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        codewhale_runtime::test_support::trust_workspace(&workspace);
        write_dir(&workspace.join(".agents").join("skills"));
        write_dir(&workspace.join("skills"));
        write_dir(&workspace.join(".claude").join("skills"));
        write_dir(&workspace.join(".cursor").join("skills"));
        write_dir(&workspace.join(".codewhale").join("skills"));
        write_dir(&workspace.join(".codex").join("skills"));
        write_dir(&home.join(".codewhale").join("skills"));

        let catalog = SkillRootCatalog::build(&workspace, Some(&home), None);
        let dirs = catalog.runtime_directories(&workspace, SkillDiscoveryMode::Compatible);

        assert_eq!(
            dirs,
            vec![
                workspace.join(".codewhale").join("skills"),
                workspace.join(".agents").join("skills"),
                workspace.join(".claude").join("skills"),
                workspace.join(".cursor").join("skills"),
                home.join(".codewhale").join("skills"),
            ]
        );
        assert!(
            !dirs
                .iter()
                .any(|p| p == &workspace.join(".codex").join("skills")),
            "codex must not activate for runtime"
        );
    }

    #[test]
    fn audit_compatible_includes_codex_without_runtime_activation() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_dir(&workspace.join(".codewhale").join("skills"));
        write_dir(&workspace.join(".codex").join("skills"));
        write_dir(&home.join(".codewhale").join("skills"));
        write_dir(&home.join(".codex").join("skills"));

        let catalog = SkillRootCatalog::build(&workspace, Some(&home), None);
        let audit: Vec<_> = catalog
            .audit_compatible_directories()
            .into_iter()
            .map(|r| r.path.clone())
            .collect();
        assert!(audit.contains(&workspace.join(".codex").join("skills")));
        assert!(audit.contains(&home.join(".codex").join("skills")));

        let runtime = catalog.runtime_directories(&workspace, SkillDiscoveryMode::Compatible);
        assert!(!runtime.contains(&workspace.join(".codex").join("skills")));
        assert!(!runtime.contains(&home.join(".codex").join("skills")));
    }

    #[test]
    fn owned_roots_are_writable_and_codewhale_only() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        codewhale_runtime::test_support::trust_workspace(&workspace);
        write_dir(&workspace.join(".agents").join("skills"));
        write_dir(&workspace.join(".codewhale").join("skills"));
        write_dir(&home.join(".codewhale").join("skills"));
        write_dir(&home.join(".agents").join("skills"));

        let catalog = SkillRootCatalog::build(&workspace, Some(&home), None);
        let owned = catalog.owned_writable_roots();
        assert_eq!(owned.len(), 2);
        assert!(owned.iter().all(|r| r.is_writable_owned()));

        let runtime = catalog.runtime_directories(&workspace, SkillDiscoveryMode::CodeWhaleOnly);
        assert_eq!(
            runtime,
            vec![
                workspace.join(".codewhale").join("skills"),
                home.join(".codewhale").join("skills"),
            ]
        );
    }

    #[test]
    fn configured_compatible_path_stays_read_only() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let agents = workspace.join(".agents").join("skills");
        write_dir(&workspace);
        write_dir(&agents);

        let (kind, access, scope) =
            classify_configured_skills_dir(&workspace, Some(&home), &agents);
        assert_eq!(
            kind,
            SkillRootKind::CompatibleProject(CompatibleHarness::Agents)
        );
        assert_eq!(access, SkillRootAccess::ReadOnlyExternal);
        assert_eq!(scope, SkillScope::Project);
    }

    #[test]
    fn safe_display_path_prefers_home_then_workspace() {
        let home = PathBuf::from("/home/user");
        let workspace = home.join("proj");
        let path = home.join(".codewhale").join("skills");
        assert_eq!(
            safe_display_path(&path, Some(&workspace), Some(&home)),
            "~/.codewhale/skills"
        );
        let project = workspace.join(".codewhale").join("skills");
        assert_eq!(
            safe_display_path(&project, Some(&workspace), Some(&home)),
            "<workspace>/.codewhale/skills"
        );
    }
    #[test]
    fn flat_workspace_root_remains_audit_only_until_opted_in() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        codewhale_runtime::test_support::trust_workspace(&workspace);
        let flat = workspace.join("skills");
        write_dir(&flat);
        let catalog = SkillRootCatalog::build(&workspace, None, None);
        assert!(
            catalog
                .audit_compatible_directories()
                .iter()
                .any(|root| root.path == flat && !root.active_for_runtime)
        );
        assert!(
            !catalog
                .runtime_directories(&workspace, SkillDiscoveryMode::Compatible)
                .contains(&flat)
        );
        let enabled = catalog.with_flat_workspace_root(true);
        assert!(
            enabled
                .runtime_directories(&workspace, SkillDiscoveryMode::CompatibleWithFlatWorkspace)
                .contains(&flat)
        );
        assert!(
            !enabled
                .runtime_directories(&workspace, SkillDiscoveryMode::CodeWhaleOnly)
                .contains(&flat)
        );
    }
}
