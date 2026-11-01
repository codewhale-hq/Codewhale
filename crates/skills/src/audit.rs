//! Bounded, read-only skill audit inventory.
//!
//! Separates "what is on disk" from runtime [`super::SkillRegistry`] merging.
//! Never executes skill bodies, never contacts the network, and never writes.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Take};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use super::roots::{
    SkillRootAccess, SkillRootCatalog, SkillRootDescriptor, SkillRootId, SkillRootKind,
    safe_display_path,
};
use super::system::is_exact_bundled_skill;
use super::{SkillRegistry, normalize_skill_name_for_lookup};
use codewhale_skill_install::install::{INSTALLED_FROM_MARKER, TRUSTED_MARKER};
use codewhale_skill_install::package_digest::{self, PackageDigestError};

/// Max bytes of `SKILL.md` the auditor will read into memory.
pub const AUDIT_MAX_SKILL_MD_BYTES: u64 = 512 * 1024;
/// Max directory depth under a skill package (and under a root when locating packages).
pub const AUDIT_MAX_DEPTH: usize = package_digest::PACKAGE_DIGEST_MAX_DEPTH;

/// Which roots the auditor visits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAuditMode {
    /// CodeWhale-owned project/global roots only.
    OwnedOnly,
    /// Owned + compatible roots (including `.codex/skills`). Does not change runtime.
    Compatible,
}

/// Stable identity for one on-disk skill copy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuditedSkillId {
    pub root_id: SkillRootId,
    pub relative_dir: PathBuf,
    pub canonical_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillSourceKind {
    CodeWhaleManaged,
    CodeWhaleManual,
    CompatibleExternal,
    BuiltIn,
    ReviewedPluginSnapshot,
    RegistryCache,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestUnknownReason {
    Unreadable,
    SymlinkPresent,
    EscapedRoot,
    Cycle,
    Oversized,
    TooManyFiles,
    TooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestState {
    Known(String),
    Unknown(DigestUnknownReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserState {
    Valid,
    Warning(Vec<String>),
    Broken(String),
    Oversized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrecedenceState {
    Active,
    ShadowedBy(AuditedSkillId),
    InactiveSource,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityState {
    Healthy,
    LocalContentDrift,
    BrokenManagedInstall,
    LegacyMetadataUnknown,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    TrustedForDigest(String),
    TrustStale,
    LegacyAdvisory,
    Untrusted,
    // Matched by the skills manager ("n/a") but never constructed: no
    // discovery path yields a non-filesystem row yet.
    #[allow(dead_code)]
    NotApplicable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessState {
    // Constructed only in tests until the #4407 readiness cache is wired.
    #[cfg(any(test, feature = "test-support"))]
    Ready,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceState {
    Managed {
        spec: Option<String>,
        safe_url: Option<String>,
        schema_version: Option<u32>,
    },
    Manual,
    External,
    BuiltIn,
    Plugin,
    Cache,
    // Matched by the skills manager ("unknown") but never constructed: no
    // discovery path yields an unclassified logical source yet.
    #[allow(dead_code)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillActionKind {
    Install,
    Import,
    Update,
    Remove,
    Trust,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillAuditWarning {
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditedSkill {
    pub id: AuditedSkillId,
    pub name: String,
    pub description: Option<String>,
    pub root: SkillRootDescriptor,
    pub safe_display_path: String,
    pub source_kind: SkillSourceKind,
    pub parser: ParserState,
    pub digest: DigestState,
    pub provenance: ProvenanceState,
    pub trust: TrustState,
    pub readiness: ReadinessState,
    pub precedence: PrecedenceState,
    pub integrity: IntegrityState,
    pub available_actions: Vec<SkillActionKind>,
    pub warnings: Vec<SkillAuditWarning>,
    /// Same canonical name + same digest as another copy.
    pub exact_duplicate_of: Option<AuditedSkillId>,
    /// Same canonical name + different digest.
    pub conflicts_with: Vec<AuditedSkillId>,
    /// External copy with no owned same-name skill — import candidate.
    pub import_candidate: bool,
    /// Package path left the declared skill root (symlink escape, etc.).
    pub path_unsafe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillAuditSnapshot {
    pub scan_mode: SkillAuditMode,
    pub roots: Vec<SkillRootDescriptor>,
    pub skills: Vec<AuditedSkill>,
    pub generated_at: SystemTime,
}

/// Optional readiness cache from Issue #4407. Missing → [`ReadinessState::Unknown`].
pub trait SkillReadinessProvider {
    fn readiness_for(&self, skill: &AuditedSkillId) -> Option<ReadinessState>;
}

/// Scan skill roots into a full, unmerged inventory.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn scan(
    workspace: &Path,
    home: Option<&Path>,
    mode: SkillAuditMode,
    readiness: Option<&dyn SkillReadinessProvider>,
) -> SkillAuditSnapshot {
    scan_with_configured(workspace, home, None, mode, readiness)
}

/// Scan skill roots with an optional configured `skills_dir`.
#[must_use]
pub fn scan_with_configured(
    workspace: &Path,
    home: Option<&Path>,
    configured_skills_dir: Option<&Path>,
    mode: SkillAuditMode,
    readiness: Option<&dyn SkillReadinessProvider>,
) -> SkillAuditSnapshot {
    let catalog = SkillRootCatalog::build(workspace, home, configured_skills_dir);
    let root_refs: Vec<SkillRootDescriptor> = match mode {
        SkillAuditMode::OwnedOnly => catalog
            .audit_owned_directories()
            .into_iter()
            .cloned()
            .collect(),
        SkillAuditMode::Compatible => catalog
            .audit_compatible_directories()
            .into_iter()
            .cloned()
            .collect(),
    };

    let mut skills = Vec::new();
    for root in &root_refs {
        skills.extend(scan_root(root, workspace, home));
    }

    classify_cross_root(&mut skills);
    for skill in &mut skills {
        skill.readiness = readiness
            .and_then(|p| p.readiness_for(&skill.id))
            .unwrap_or(ReadinessState::Unknown);
        skill.available_actions = action_policy(skill);
    }

    SkillAuditSnapshot {
        scan_mode: mode,
        roots: root_refs,
        skills,
        generated_at: SystemTime::now(),
    }
}

/// Expand an owned-only inventory to compatible roots without re-reading the
/// unchanged owned packages.
///
/// The manager uses this for its interactive scan-mode toggle. Package audits
/// include bounded content hashing, so re-auditing every bundled owned skill
/// can make a simple keypress appear lost on a cold filesystem. Reusing rows by
/// root keeps the result ordered by catalog precedence while newly eligible
/// external roots are still read from disk.
#[must_use]
pub fn expand_owned_scan_to_compatible(
    workspace: &Path,
    home: Option<&Path>,
    configured_skills_dir: Option<&Path>,
    owned_skills: &[AuditedSkill],
    readiness: Option<&dyn SkillReadinessProvider>,
) -> SkillAuditSnapshot {
    let catalog = SkillRootCatalog::build(workspace, home, configured_skills_dir);
    let root_refs: Vec<SkillRootDescriptor> = catalog
        .audit_compatible_directories()
        .into_iter()
        .cloned()
        .collect();
    let reusable_root_ids: HashSet<SkillRootId> = owned_skills
        .iter()
        .map(|skill| skill.id.root_id.clone())
        .collect();

    let mut skills = Vec::new();
    for root in &root_refs {
        if reusable_root_ids.contains(&root.id) {
            skills.extend(
                owned_skills
                    .iter()
                    .filter(|skill| skill.id.root_id == root.id)
                    .cloned(),
            );
        } else {
            skills.extend(scan_root(root, workspace, home));
        }
    }

    // The owned rows carried their previous cross-root result. Recompute it
    // against the expanded inventory so precedence/conflict/import actions are
    // exactly the same as a fresh compatible scan.
    for skill in &mut skills {
        skill.precedence = if skill.root.active_for_runtime {
            PrecedenceState::Unknown
        } else {
            PrecedenceState::InactiveSource
        };
        skill.exact_duplicate_of = None;
        skill.conflicts_with.clear();
        skill.import_candidate = false;
        skill.available_actions.clear();
    }
    classify_cross_root(&mut skills);
    for skill in &mut skills {
        skill.readiness = readiness
            .and_then(|provider| provider.readiness_for(&skill.id))
            .unwrap_or(ReadinessState::Unknown);
        skill.available_actions = action_policy(skill);
    }

    SkillAuditSnapshot {
        scan_mode: SkillAuditMode::Compatible,
        roots: root_refs,
        skills,
        generated_at: SystemTime::now(),
    }
}

/// Compute available mutations for one audited row (UI and controller share this).
#[must_use]
pub fn action_policy(skill: &AuditedSkill) -> Vec<SkillActionKind> {
    if skill.path_unsafe {
        return Vec::new();
    }

    match skill.source_kind {
        SkillSourceKind::CodeWhaleManaged => {
            let mut actions = Vec::new();
            if matches!(skill.parser, ParserState::Valid | ParserState::Warning(_))
                && !matches!(skill.integrity, IntegrityState::BrokenManagedInstall)
            {
                actions.push(SkillActionKind::Update);
            }
            actions.push(SkillActionKind::Remove);
            if matches!(
                skill.trust,
                TrustState::Untrusted | TrustState::TrustStale | TrustState::LegacyAdvisory
            ) && matches!(skill.digest, DigestState::Known(_))
                && matches!(skill.parser, ParserState::Valid | ParserState::Warning(_))
            {
                actions.push(SkillActionKind::Trust);
            }
            actions
        }
        SkillSourceKind::CodeWhaleManual => Vec::new(),
        SkillSourceKind::CompatibleExternal => {
            // Import is offered for fresh candidates and for same-name owned
            // peers (exact duplicate → AlreadyPresent, conflict → replace confirm).
            // The mutation controller remains the authority on scope/conflict policy.
            let importable = matches!(skill.parser, ParserState::Valid | ParserState::Warning(_))
                && matches!(skill.digest, DigestState::Known(_))
                && (skill.import_candidate
                    || skill.exact_duplicate_of.is_some()
                    || !skill.conflicts_with.is_empty());
            if importable {
                vec![SkillActionKind::Import]
            } else {
                Vec::new()
            }
        }
        SkillSourceKind::BuiltIn
        | SkillSourceKind::ReviewedPluginSnapshot
        | SkillSourceKind::RegistryCache => Vec::new(),
    }
}

// ── per-root scan ────────────────────────────────────────────────────────────

fn scan_root(
    root: &SkillRootDescriptor,
    workspace: &Path,
    home: Option<&Path>,
) -> Vec<AuditedSkill> {
    let mut out = Vec::new();
    let Ok(canonical_root) = fs::canonicalize(&root.path) else {
        return out;
    };
    let mut visited = HashSet::new();
    let mut packages = Vec::new();
    find_skill_packages(&root.path, &canonical_root, 0, &mut visited, &mut packages);

    for package_dir in packages {
        out.push(audit_package(
            root,
            &package_dir,
            &canonical_root,
            workspace,
            home,
        ));
    }
    out
}

fn find_skill_packages(
    dir: &Path,
    canonical_root: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    if depth > AUDIT_MAX_DEPTH {
        return;
    }
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if meta.file_type().is_symlink() {
        let Ok(canonical) = fs::canonicalize(dir) else {
            return;
        };
        if !canonical.starts_with(canonical_root) || !canonical.is_dir() {
            return;
        }
        if !visited.insert(canonical) {
            return;
        }
    } else if meta.is_dir() {
        let Ok(canonical) = fs::canonicalize(dir) else {
            return;
        };
        if !visited.insert(canonical) {
            return;
        }
    } else {
        return;
    }

    let skill_md = dir.join("SKILL.md");
    if skill_md.is_file() || fs::symlink_metadata(&skill_md).is_ok() {
        out.push(dir.to_path_buf());
        return; // do not descend into a skill package
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() || meta.file_type().is_symlink() {
            find_skill_packages(&path, canonical_root, depth + 1, visited, out);
        }
    }
}

fn audit_package(
    root: &SkillRootDescriptor,
    package_dir: &Path,
    canonical_root: &Path,
    workspace: &Path,
    home: Option<&Path>,
) -> AuditedSkill {
    let relative_dir = package_dir
        .strip_prefix(&root.path)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| {
            package_dir
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        });

    let mut warnings = Vec::new();
    let skill_md = package_dir.join("SKILL.md");
    let (parser, name, description, skill_md_content) = parse_skill_md_bounded(&skill_md);

    let package = analyze_package(package_dir, canonical_root);
    let path_unsafe = package.path_unsafe;
    if path_unsafe {
        warnings.push(SkillAuditWarning::Message(
            "package contains a symlink that escapes the skill root or cycles".into(),
        ));
    }
    for w in package.warnings {
        warnings.push(SkillAuditWarning::Message(w));
    }

    let canonical_name = name
        .as_deref()
        .map(normalize_skill_name_for_lookup)
        .unwrap_or_else(|| {
            normalize_skill_name_for_lookup(
                &relative_dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "skill".into()),
            )
        });

    let marker = read_installed_from(package_dir);
    let trust = read_trust_state(package_dir, &package.digest);
    let (source_kind, provenance, integrity) = classify_source(
        root,
        &canonical_name,
        skill_md_content.as_deref(),
        &marker,
        &package.digest,
        path_unsafe,
    );

    let display = format!(
        "{}/{}",
        safe_display_path(&root.path, Some(workspace), home),
        relative_dir.display()
    )
    .replace('\\', "/");

    AuditedSkill {
        id: AuditedSkillId {
            root_id: root.id.clone(),
            relative_dir,
            canonical_name: canonical_name.clone(),
        },
        name: canonical_name,
        description,
        root: root.clone(),
        safe_display_path: display,
        source_kind,
        parser,
        digest: package.digest,
        provenance,
        trust,
        readiness: ReadinessState::Unknown,
        precedence: if root.active_for_runtime {
            PrecedenceState::Unknown // filled in classify_cross_root
        } else {
            PrecedenceState::InactiveSource
        },
        integrity,
        available_actions: Vec::new(),
        warnings,
        exact_duplicate_of: None,
        conflicts_with: Vec::new(),
        import_candidate: false,
        path_unsafe,
    }
}

fn parse_skill_md_bounded(
    path: &Path,
) -> (ParserState, Option<String>, Option<String>, Option<String>) {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) => {
            return (
                ParserState::Broken(format!("cannot stat SKILL.md: {err}")),
                None,
                None,
                None,
            );
        }
    };
    if meta.file_type().is_symlink() {
        return (
            ParserState::Broken("SKILL.md is a symlink".into()),
            None,
            None,
            None,
        );
    }
    if !meta.is_file() {
        return (
            ParserState::Broken("SKILL.md is not a regular file".into()),
            None,
            None,
            None,
        );
    }
    if meta.len() > AUDIT_MAX_SKILL_MD_BYTES {
        return (ParserState::Oversized, None, None, None);
    }

    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) => {
            return (
                ParserState::Broken(format!("cannot open SKILL.md: {err}")),
                None,
                None,
                None,
            );
        }
    };
    let mut limited: Take<File> = file.take(AUDIT_MAX_SKILL_MD_BYTES + 1);
    let mut buf = Vec::new();
    if let Err(err) = limited.read_to_end(&mut buf) {
        return (
            ParserState::Broken(format!("cannot read SKILL.md: {err}")),
            None,
            None,
            None,
        );
    }
    if buf.len() as u64 > AUDIT_MAX_SKILL_MD_BYTES {
        return (ParserState::Oversized, None, None, None);
    }
    let content = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => {
            return (
                ParserState::Broken("SKILL.md is not valid UTF-8".into()),
                None,
                None,
                None,
            );
        }
    };

    match SkillRegistry::parse_verified_content(path, &content) {
        Ok((skill, warnings)) => {
            let desc = if skill.description.is_empty() {
                None
            } else {
                Some(truncate_desc(&skill.description))
            };
            let parser = if warnings.is_empty() {
                ParserState::Valid
            } else {
                ParserState::Warning(warnings)
            };
            (parser, Some(skill.name), desc, Some(content))
        }
        Err(reason) => (ParserState::Broken(reason), None, None, Some(content)),
    }
}

fn truncate_desc(s: &str) -> String {
    const MAX: usize = 280;
    let count = s.chars().count();
    if count <= MAX {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(MAX.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

struct PackageAnalysis {
    digest: DigestState,
    path_unsafe: bool,
    warnings: Vec<String>,
}

/// Test-only digest helper. Prod paths (audit, mutation) call
/// `package_digest::compute_package_digest` directly.
#[cfg(any(test, feature = "test-support"))]
pub fn compute_package_digest(package_dir: &Path) -> Result<String, DigestUnknownReason> {
    package_digest::compute_package_digest(package_dir).map_err(digest_error_to_unknown)
}

fn digest_error_to_unknown(err: PackageDigestError) -> DigestUnknownReason {
    match err {
        PackageDigestError::Unreadable => DigestUnknownReason::Unreadable,
        PackageDigestError::SymlinkPresent => DigestUnknownReason::SymlinkPresent,
        PackageDigestError::EscapedRoot => DigestUnknownReason::EscapedRoot,
        PackageDigestError::Cycle => DigestUnknownReason::Cycle,
        PackageDigestError::Oversized => DigestUnknownReason::Oversized,
        PackageDigestError::TooManyFiles => DigestUnknownReason::TooManyFiles,
        PackageDigestError::TooDeep => DigestUnknownReason::TooDeep,
    }
}

fn analyze_package(package_dir: &Path, _canonical_root: &Path) -> PackageAnalysis {
    match package_digest::compute_package_digest(package_dir) {
        Ok(digest) => PackageAnalysis {
            digest: DigestState::Known(digest),
            path_unsafe: false,
            warnings: Vec::new(),
        },
        Err(err) => {
            let reason = digest_error_to_unknown(err.clone());
            let path_unsafe = matches!(
                err,
                PackageDigestError::SymlinkPresent
                    | PackageDigestError::EscapedRoot
                    | PackageDigestError::Cycle
            );
            PackageAnalysis {
                digest: DigestState::Unknown(reason),
                path_unsafe,
                warnings: vec![err.to_string()],
            }
        }
    }
}

// ── markers ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct InstalledFromFile {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content_digest: Option<String>,
}

#[derive(Debug, Clone)]
enum MarkerParse {
    Absent,
    V1(InstalledFromFile),
    V2(InstalledFromFile),
    // Reason kept for Debug + future warning surfacing; matches bind `_`.
    Broken(#[allow(dead_code)] String),
}

fn read_installed_from(package_dir: &Path) -> MarkerParse {
    let path = package_dir.join(INSTALLED_FROM_MARKER);
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => return MarkerParse::Absent,
    };
    if meta.file_type().is_symlink() {
        return MarkerParse::Broken("symlink .installed-from".into());
    }
    if !meta.is_file() {
        return MarkerParse::Broken(".installed-from is not a regular file".into());
    }
    let Ok(body) = fs::read_to_string(&path) else {
        return MarkerParse::Broken("unreadable .installed-from".into());
    };
    let Ok(parsed) = serde_json::from_str::<InstalledFromFile>(&body) else {
        return MarkerParse::Broken("malformed .installed-from".into());
    };
    match parsed.schema_version {
        Some(v) if v >= 2 => MarkerParse::V2(parsed),
        Some(_) | None => MarkerParse::V1(parsed),
    }
}

#[derive(Debug, Deserialize)]
struct TrustFileV2 {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    content_digest: Option<String>,
}

fn read_trust_state(package_dir: &Path, digest: &DigestState) -> TrustState {
    let path = package_dir.join(TRUSTED_MARKER);
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => return TrustState::Untrusted,
    };
    if meta.file_type().is_symlink() {
        // Do not follow symlink trust markers.
        return TrustState::Unknown;
    }
    if !meta.is_file() {
        return TrustState::Unknown;
    }
    let Ok(body) = fs::read_to_string(&path) else {
        return TrustState::Unknown;
    };
    if let Ok(parsed) = serde_json::from_str::<TrustFileV2>(&body)
        && parsed.schema_version == Some(2)
    {
        let Some(trusted_digest) = parsed.content_digest else {
            return TrustState::Unknown;
        };
        return match digest {
            DigestState::Known(current) if current == &trusted_digest => {
                TrustState::TrustedForDigest(trusted_digest)
            }
            DigestState::Known(_) => TrustState::TrustStale,
            DigestState::Unknown(_) => TrustState::Unknown,
        };
    }
    TrustState::LegacyAdvisory
}

fn sanitize_url_for_display(url: &str) -> String {
    // Strip userinfo, query, and fragment before UI / receipts.
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    if let Some(scheme_end) = without_query.find("://") {
        let scheme = &without_query[..scheme_end];
        let rest = &without_query[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            return format!("{scheme}://{}", &rest[at + 1..]);
        }
    }
    without_query.to_string()
}

fn classify_source(
    root: &SkillRootDescriptor,
    canonical_name: &str,
    skill_md_content: Option<&str>,
    marker: &MarkerParse,
    digest: &DigestState,
    _path_unsafe: bool,
) -> (SkillSourceKind, ProvenanceState, IntegrityState) {
    if matches!(root.kind, SkillRootKind::RegistryCache) {
        return (
            SkillSourceKind::RegistryCache,
            ProvenanceState::Cache,
            IntegrityState::Unknown,
        );
    }
    if matches!(root.kind, SkillRootKind::ReviewedPluginSnapshot) {
        return (
            SkillSourceKind::ReviewedPluginSnapshot,
            ProvenanceState::Plugin,
            IntegrityState::Unknown,
        );
    }

    if root.access != SkillRootAccess::WritableOwned {
        return (
            SkillSourceKind::CompatibleExternal,
            ProvenanceState::External,
            IntegrityState::Unknown,
        );
    }

    // Managed markers win over bundled-name heuristics so a registry install
    // that reuses a bundled command name (e.g. `pdf`) stays Update/Remove/Trust
    // capable. Exact shipped body without a marker is still BuiltIn.
    match marker {
        MarkerParse::V1(_) | MarkerParse::V2(_) | MarkerParse::Broken(_) => {}
        MarkerParse::Absent => {
            if let Some(content) = skill_md_content
                && is_exact_bundled_skill(canonical_name, content)
            {
                return (
                    SkillSourceKind::BuiltIn,
                    ProvenanceState::BuiltIn,
                    IntegrityState::Healthy,
                );
            }
        }
    }

    match marker {
        MarkerParse::Absent => (
            SkillSourceKind::CodeWhaleManual,
            ProvenanceState::Manual,
            IntegrityState::Unknown,
        ),
        MarkerParse::Broken(_) => (
            SkillSourceKind::CodeWhaleManaged,
            ProvenanceState::Managed {
                spec: None,
                safe_url: None,
                schema_version: None,
            },
            IntegrityState::BrokenManagedInstall,
        ),
        MarkerParse::V1(m) => (
            SkillSourceKind::CodeWhaleManaged,
            ProvenanceState::Managed {
                spec: m.spec.clone(),
                safe_url: m.url.as_deref().map(sanitize_url_for_display),
                schema_version: m.schema_version.or(Some(1)),
            },
            IntegrityState::LegacyMetadataUnknown,
        ),
        MarkerParse::V2(m) => {
            let integrity = match (&m.content_digest, digest) {
                (Some(expected), DigestState::Known(actual)) if expected == actual => {
                    IntegrityState::Healthy
                }
                (Some(_), DigestState::Known(_)) => IntegrityState::LocalContentDrift,
                (None, _) => IntegrityState::Unknown,
                (_, DigestState::Unknown(_)) => IntegrityState::Unknown,
            };
            (
                SkillSourceKind::CodeWhaleManaged,
                ProvenanceState::Managed {
                    spec: m.spec.clone(),
                    safe_url: m.url.as_deref().map(sanitize_url_for_display),
                    schema_version: m.schema_version,
                },
                integrity,
            )
        }
    }
}

// ── cross-root classification ────────────────────────────────────────────────

fn classify_cross_root(skills: &mut [AuditedSkill]) {
    // Group by canonical name preserving first-seen order (catalog precedence).
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, skill) in skills.iter().enumerate() {
        by_name
            .entry(skill.id.canonical_name.clone())
            .or_default()
            .push(idx);
    }

    let owned_names: HashSet<String> = skills
        .iter()
        .filter(|s| s.root.is_writable_owned())
        .map(|s| s.id.canonical_name.clone())
        .collect();

    for indices in by_name.values() {
        if indices.is_empty() {
            continue;
        }

        // Runtime-active winners: among copies whose root is active_for_runtime,
        // the earliest in catalog order wins. Audit-only roots stay InactiveSource.
        let runtime_indices: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| skills[i].root.active_for_runtime)
            .collect();
        // Already in scan order which follows catalog precedence.
        if let Some(&winner) = runtime_indices.first() {
            let winner_id = skills[winner].id.clone();
            for &idx in &runtime_indices {
                if idx == winner {
                    if !matches!(skills[idx].precedence, PrecedenceState::InactiveSource) {
                        skills[idx].precedence = PrecedenceState::Active;
                    }
                } else {
                    skills[idx].precedence = PrecedenceState::ShadowedBy(winner_id.clone());
                }
            }
        }

        // Duplicate / conflict among all copies (including inactive).
        let digests: Vec<(usize, Option<String>)> = indices
            .iter()
            .map(|&i| {
                let d = match &skills[i].digest {
                    DigestState::Known(s) => Some(s.clone()),
                    DigestState::Unknown(_) => None,
                };
                (i, d)
            })
            .collect();

        for &(i, ref di) in &digests {
            for &(j, ref dj) in &digests {
                if i >= j {
                    continue;
                }
                match (di, dj) {
                    (Some(a), Some(b)) if a == b => {
                        let other = skills[j].id.clone();
                        if skills[i].exact_duplicate_of.is_none() {
                            skills[i].exact_duplicate_of = Some(other);
                        } else {
                            let other = skills[i].id.clone();
                            if skills[j].exact_duplicate_of.is_none() {
                                skills[j].exact_duplicate_of = Some(other);
                            }
                        }
                    }
                    (Some(_), Some(_)) => {
                        let id_j = skills[j].id.clone();
                        let id_i = skills[i].id.clone();
                        skills[i].conflicts_with.push(id_j);
                        skills[j].conflicts_with.push(id_i);
                    }
                    _ => {}
                }
            }
        }
    }

    for skill in skills.iter_mut() {
        if skill.source_kind == SkillSourceKind::CompatibleExternal
            && !owned_names.contains(&skill.id.canonical_name)
            && matches!(skill.parser, ParserState::Valid | ParserState::Warning(_))
            && matches!(skill.digest, DigestState::Known(_))
            && !skill.path_unsafe
        {
            skill.import_candidate = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn owned_only_skips_compatible_and_codex() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "owned",
            "owned skill",
            "body",
        );
        write_skill(
            &workspace.join(".claude").join("skills"),
            "claude",
            "claude skill",
            "body",
        );
        write_skill(
            &workspace.join(".codex").join("skills"),
            "codex",
            "codex skill",
            "body",
        );

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        let names: Vec<_> = snap.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["owned"]);
        assert!(!snap.roots.iter().any(|r| matches!(
            r.kind,
            SkillRootKind::CompatibleProject(_) | SkillRootKind::CompatibleGlobal(_)
        )));
    }

    #[test]
    fn compatible_includes_codex_without_activating_runtime_precedence() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "shared",
            "owned",
            "owned-body",
        );
        write_skill(
            &workspace.join(".codex").join("skills"),
            "shared",
            "codex",
            "codex-body",
        );

        let snap = scan(&workspace, Some(&home), SkillAuditMode::Compatible, None);
        assert_eq!(snap.skills.len(), 2);
        let codex = snap
            .skills
            .iter()
            .find(|s| {
                matches!(
                    s.root.kind,
                    SkillRootKind::CompatibleProject(super::super::roots::CompatibleHarness::Codex)
                )
            })
            .expect("codex copy");
        assert_eq!(codex.precedence, PrecedenceState::InactiveSource);
        assert!(!codex.root.active_for_runtime);

        let owned = snap
            .skills
            .iter()
            .find(|s| s.root.kind == SkillRootKind::CodeWhaleProject)
            .expect("owned");
        assert_eq!(owned.precedence, PrecedenceState::Active);
    }

    #[test]
    fn expanding_owned_scan_matches_fresh_compatible_scan() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "shared",
            "owned",
            "owned-body",
        );
        write_skill(
            &workspace.join(".agents").join("skills"),
            "shared",
            "external conflict",
            "external-body",
        );
        write_skill(
            &workspace.join(".codex").join("skills"),
            "candidate",
            "import candidate",
            "candidate-body",
        );

        let owned = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        let expanded =
            expand_owned_scan_to_compatible(&workspace, Some(&home), None, &owned.skills, None);
        let fresh = scan(&workspace, Some(&home), SkillAuditMode::Compatible, None);

        assert_eq!(expanded.scan_mode, SkillAuditMode::Compatible);
        assert_eq!(expanded.roots, fresh.roots);
        assert_eq!(expanded.skills, fresh.skills);
    }

    #[test]
    fn detects_shadow_duplicate_and_conflict() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");

        // Identical package content → exact duplicate (after shadowing).
        let identical = "---\nname: shared\ndescription: same\n---\nbody\n";
        fs::create_dir_all(workspace.join(".agents").join("skills").join("shared")).unwrap();
        fs::write(
            workspace
                .join(".agents")
                .join("skills")
                .join("shared")
                .join("SKILL.md"),
            identical,
        )
        .unwrap();
        fs::create_dir_all(workspace.join(".claude").join("skills").join("shared")).unwrap();
        fs::write(
            workspace
                .join(".claude")
                .join("skills")
                .join("shared")
                .join("SKILL.md"),
            identical,
        )
        .unwrap();
        // Different content → conflict with the active copy.
        write_skill(
            &workspace.join(".cursor").join("skills"),
            "shared",
            "cursor conflict",
            "different-body",
        );

        let snap = scan(&workspace, Some(&home), SkillAuditMode::Compatible, None);
        let shared: Vec<_> = snap.skills.iter().filter(|s| s.name == "shared").collect();
        assert_eq!(shared.len(), 3);
        assert!(
            shared
                .iter()
                .any(|s| matches!(s.precedence, PrecedenceState::Active))
        );
        assert!(
            shared
                .iter()
                .any(|s| matches!(s.precedence, PrecedenceState::ShadowedBy(_)))
        );
        assert!(shared.iter().any(|s| s.exact_duplicate_of.is_some()));
        assert!(shared.iter().any(|s| !s.conflicts_with.is_empty()));
    }

    #[test]
    fn external_without_owned_peer_is_import_candidate() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        fs::create_dir_all(workspace.join(".codewhale").join("skills")).unwrap();
        write_skill(
            &workspace.join(".claude").join("skills"),
            "from-claude",
            "desc",
            "body",
        );

        let snap = scan(&workspace, Some(&home), SkillAuditMode::Compatible, None);
        let skill = snap
            .skills
            .iter()
            .find(|s| s.name == "from-claude")
            .expect("skill");
        assert!(skill.import_candidate);
        assert_eq!(skill.available_actions, vec![SkillActionKind::Import]);
        assert_eq!(skill.source_kind, SkillSourceKind::CompatibleExternal);
    }

    #[test]
    fn external_conflicting_with_owned_still_offers_import() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "shared",
            "desc",
            "owned-body",
        );
        write_skill(
            &workspace.join(".claude").join("skills"),
            "shared",
            "desc",
            "external-body",
        );

        let snap = scan(&workspace, Some(&home), SkillAuditMode::Compatible, None);
        let external = snap
            .skills
            .iter()
            .find(|s| s.name == "shared" && s.source_kind == SkillSourceKind::CompatibleExternal)
            .expect("external");
        assert!(!external.import_candidate);
        assert!(!external.conflicts_with.is_empty());
        assert_eq!(external.available_actions, vec![SkillActionKind::Import]);
    }

    #[test]
    fn v1_marker_is_legacy_integrity_and_managed_actions() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "desc", "body");
        fs::write(
            root.join("managed").join(INSTALLED_FROM_MARKER),
            r#"{"spec":"github:o/r","url":"https://user:pass@example.com/x?token=1#frag","checksum":"abc"}"#,
        )
        .unwrap();

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        let skill = &snap.skills[0];
        assert_eq!(skill.source_kind, SkillSourceKind::CodeWhaleManaged);
        assert_eq!(skill.integrity, IntegrityState::LegacyMetadataUnknown);
        assert!(skill.available_actions.contains(&SkillActionKind::Update));
        assert!(skill.available_actions.contains(&SkillActionKind::Remove));
        if let ProvenanceState::Managed { safe_url, .. } = &skill.provenance {
            let url = safe_url.as_deref().unwrap();
            assert!(!url.contains("user:pass"));
            assert!(!url.contains("token"));
            assert!(!url.contains("frag"));
        } else {
            panic!("expected managed provenance");
        }
    }

    #[test]
    fn v2_marker_detects_healthy_and_drift() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "desc", "body");

        // First scan to learn digest, then write matching v2 marker.
        let preliminary = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        let DigestState::Known(digest) = &preliminary.skills[0].digest else {
            panic!("expected known digest");
        };
        fs::write(
            root.join("managed").join(INSTALLED_FROM_MARKER),
            format!(r#"{{"schema_version":2,"spec":"github:o/r","content_digest":"{digest}"}}"#),
        )
        .unwrap();
        let healthy = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(healthy.skills[0].integrity, IntegrityState::Healthy);

        fs::write(
            root.join("managed").join(INSTALLED_FROM_MARKER),
            r#"{"schema_version":2,"spec":"github:o/r","content_digest":"deadbeef"}"#,
        )
        .unwrap();
        let drift = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(drift.skills[0].integrity, IntegrityState::LocalContentDrift);
    }

    #[test]
    fn legacy_trust_and_digest_bound_trust() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "desc", "body");
        fs::write(
            root.join("managed").join(INSTALLED_FROM_MARKER),
            r#"{"spec":"github:o/r","checksum":"x"}"#,
        )
        .unwrap();
        fs::write(root.join("managed").join(TRUSTED_MARKER), "trusted\n").unwrap();

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(snap.skills[0].trust, TrustState::LegacyAdvisory);

        let DigestState::Known(digest) = &snap.skills[0].digest else {
            panic!("digest");
        };
        fs::write(
            root.join("managed").join(TRUSTED_MARKER),
            format!(r#"{{"schema_version":2,"content_digest":"{digest}"}}"#),
        )
        .unwrap();
        let trusted = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert!(matches!(
            trusted.skills[0].trust,
            TrustState::TrustedForDigest(_)
        ));

        fs::write(
            root.join("managed").join(TRUSTED_MARKER),
            r#"{"schema_version":2,"content_digest":"stale"}"#,
        )
        .unwrap();
        let stale = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(stale.skills[0].trust, TrustState::TrustStale);
    }

    #[test]
    fn oversized_skill_md_is_fail_closed() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let skill_dir = workspace.join(".codewhale").join("skills").join("big");
        fs::create_dir_all(&skill_dir).unwrap();
        let huge = format!(
            "---\nname: big\ndescription: x\n---\n{}",
            "x".repeat(AUDIT_MAX_SKILL_MD_BYTES as usize + 64)
        );
        fs::write(skill_dir.join("SKILL.md"), huge).unwrap();

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(snap.skills[0].parser, ParserState::Oversized);
        assert!(snap.skills[0].available_actions.is_empty());
    }

    #[test]
    fn readiness_missing_stays_unknown() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_skill(&workspace.join(".codewhale").join("skills"), "a", "d", "b");
        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(snap.skills[0].readiness, ReadinessState::Unknown);
    }

    #[test]
    fn bundled_name_alone_is_not_built_in() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        // `pdf` is a bundled name, but custom body must not classify as BuiltIn.
        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "pdf",
            "user override",
            "not-the-bundled-body",
        );
        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(snap.skills[0].source_kind, SkillSourceKind::CodeWhaleManual);
        assert!(snap.skills[0].available_actions.is_empty());
    }

    #[test]
    fn managed_marker_wins_over_bundled_name() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let root = workspace.join(".codewhale").join("skills");
        // Bundled command name + different body + install marker → managed.
        write_skill(&root, "pdf", "registry pdf", "community-body");
        fs::write(
            root.join("pdf").join(INSTALLED_FROM_MARKER),
            r#"{"spec":"github:o/pdf-skill","checksum":"abc"}"#,
        )
        .unwrap();

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(
            snap.skills[0].source_kind,
            SkillSourceKind::CodeWhaleManaged
        );
        assert!(
            snap.skills[0]
                .available_actions
                .contains(&SkillActionKind::Update)
        );
        assert!(
            snap.skills[0]
                .available_actions
                .contains(&SkillActionKind::Remove)
        );
        assert!(
            snap.skills[0]
                .available_actions
                .contains(&SkillActionKind::Trust)
        );
    }

    #[test]
    fn exact_bundled_content_is_built_in() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let root = workspace.join(".codewhale").join("skills");
        fs::create_dir_all(root.join("pdf")).unwrap();
        fs::write(
            root.join("pdf").join("SKILL.md"),
            include_str!("../assets/skills/pdf/SKILL.md"),
        )
        .unwrap();

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert_eq!(snap.skills[0].source_kind, SkillSourceKind::BuiltIn);
        assert!(snap.skills[0].available_actions.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_installed_from_marker_is_fail_closed() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "desc", "body");
        let outside = tmp.path().join("outside-marker.json");
        fs::write(&outside, r#"{"spec":"github:o/r","checksum":"x"}"#).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("managed").join(INSTALLED_FROM_MARKER))
            .unwrap();

        let snap = scan(&workspace, Some(&home), SkillAuditMode::OwnedOnly, None);
        assert!(snap.skills[0].path_unsafe);
        assert!(matches!(
            snap.skills[0].digest,
            DigestState::Unknown(DigestUnknownReason::SymlinkPresent)
        ));
        assert_eq!(
            snap.skills[0].integrity,
            IntegrityState::BrokenManagedInstall
        );
        assert!(snap.skills[0].available_actions.is_empty());
    }

    #[test]
    fn sanitize_url_strips_secrets() {
        assert_eq!(
            sanitize_url_for_display("https://user:pw@host/path?token=1#x"),
            "https://host/path"
        );
    }

    struct AlwaysReady;
    impl SkillReadinessProvider for AlwaysReady {
        fn readiness_for(&self, _: &AuditedSkillId) -> Option<ReadinessState> {
            Some(ReadinessState::Ready)
        }
    }

    #[test]
    fn readiness_provider_is_consulted_when_present() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        write_skill(&workspace.join(".codewhale").join("skills"), "a", "d", "b");
        let snap = scan(
            &workspace,
            Some(&home),
            SkillAuditMode::OwnedOnly,
            Some(&AlwaysReady),
        );
        assert_eq!(snap.skills[0].readiness, ReadinessState::Ready);
    }
}
