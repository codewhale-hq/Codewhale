//! Unique skill mutation controller for CodeWhale-owned roots.
//!
//! All install / import / update / remove / trust writes go through this
//! module. Compatible harness roots, built-ins, and plugin snapshots are
//! never mutated.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use codewhale_net::network_policy::NetworkPolicy;

use super::audit::{
    AuditedSkill, AuditedSkillId, DigestState, SkillActionKind, SkillAuditMode, SkillSourceKind,
    scan_with_configured,
};
use super::normalize_skill_name_for_lookup;
use super::roots::{
    SkillRootCatalog, SkillRootDescriptor, SkillRootKind, SkillScope, safe_display_path,
};
use codewhale_skill_install::bounded_tar;
use codewhale_skill_install::frontmatter::{
    SkillValidationMode, parse_frontmatter, validate_skill_frontmatter,
};
use codewhale_skill_install::install::{
    self, InstallOutcome, InstallSource, UpdateResult, write_installed_from_v2, write_trust_v2,
};
use codewhale_skill_install::package_digest;

/// Project vs global CodeWhale-owned install target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTargetScope {
    Project,
    Global,
}

impl SkillTargetScope {
    #[must_use]
    pub fn as_skill_scope(self) -> SkillScope {
        match self {
            Self::Project => SkillScope::Project,
            Self::Global => SkillScope::Global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Reject,
    ReplaceConfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInitiator {
    Person,
    Agent,
    CommandLine,
}

pub type StartedBy = SkillInitiator;

#[derive(Debug, Clone)]
pub enum SkillMutationRequest {
    InstallRemote {
        source: InstallSource,
        target: SkillTargetScope,
    },
    ImportExternal {
        source_id: AuditedSkillId,
        expected_digest: String,
        target: SkillTargetScope,
        conflict_policy: ConflictPolicy,
    },
    Update {
        skill_id: AuditedSkillId,
        expected_digest: Option<String>,
    },
    /// Resolve by name inside owned roots (compatible `/skill update` path).
    UpdateByName {
        name: String,
        scope: Option<SkillTargetScope>,
        expected_digest: Option<String>,
    },
    Remove {
        skill_id: AuditedSkillId,
        expected_digest: Option<String>,
    },
    RemoveByName {
        name: String,
        scope: Option<SkillTargetScope>,
        expected_digest: Option<String>,
    },
    Trust {
        skill_id: AuditedSkillId,
        expected_digest: String,
    },
    TrustByName {
        name: String,
        scope: Option<SkillTargetScope>,
        expected_digest: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillMutationOutcome {
    Installed,
    Updated,
    NoChange,
    Removed,
    Trusted,
    Imported,
    AlreadyPresent,
    NeedsApproval(String),
    NetworkDenied(String),
}

#[derive(Debug, Clone)]
pub enum SkillOneStepRequest {
    Install {
        spec: String,
        target: SkillTargetScope,
    },
    Mutation(SkillMutationRequest),
}

#[derive(Debug, Clone)]
pub struct SkillMutationReceipt {
    #[allow(dead_code)] // surfaced by manager detail / future receipt toast
    pub action: SkillActionKind,
    pub name: String,
    pub scope: SkillScope,
    pub safe_target_path: String,
    #[allow(dead_code)] // reserved for digest-diff UI
    pub before_digest: Option<String>,
    #[allow(dead_code)] // reserved for digest-diff UI
    pub after_digest: Option<String>,
    pub outcome: SkillMutationOutcome,
}

#[derive(Debug, Clone)]
pub struct SkillOneStepReceipt {
    pub receipt: SkillMutationReceipt,
    pub summary: String,
}

impl std::ops::Deref for SkillOneStepReceipt {
    type Target = SkillMutationReceipt;

    fn deref(&self) -> &Self::Target {
        &self.receipt
    }
}

/// Inputs shared by mutation operations.
pub struct MutationContext<'a> {
    pub workspace: &'a Path,
    pub home: Option<&'a Path>,
    pub configured_skills_dir: Option<&'a Path>,
    pub network: &'a NetworkPolicy,
    pub max_size: u64,
    pub registry_url: &'a str,
}

fn owned_anchor<'a>(
    workspace: &'a Path,
    home: Option<&'a Path>,
    target: SkillTargetScope,
) -> Result<&'a Path> {
    match target {
        SkillTargetScope::Project => Ok(workspace),
        SkillTargetScope::Global => home.context("global skill mutations require a home directory"),
    }
}

/// Return whether `path` is an existing real directory, rejecting links and
/// non-directory components. `symlink_metadata` is intentional: following a
/// link before checking it would turn a lexical CodeWhale-owned root into an
/// attacker-selected write target.
fn checked_real_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "refusing to mutate symlinked Codewhale skills path component {}",
                path.display()
            )
        }
        Ok(meta) if !meta.is_dir() => bail!(
            "refusing to mutate through non-directory Codewhale skills path component {}",
            path.display()
        ),
        Ok(_) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

/// Validate the complete owned-root chain without following a symlink in the
/// workspace/home anchor, `.codewhale`, or `skills` component.
fn validate_owned_target_chain(
    anchor: &Path,
    skills_dir: &Path,
    require_existing: bool,
) -> Result<()> {
    let expected = anchor.join(".codewhale").join("skills");
    if skills_dir != expected {
        bail!(
            "refusing to mutate non-canonical Codewhale skills root {}",
            skills_dir.display()
        );
    }

    let anchor_exists = checked_real_directory(anchor)?;
    if !anchor_exists {
        if require_existing {
            bail!("owned skill anchor {} does not exist", anchor.display());
        }
        return Ok(());
    }

    let codewhale_dir = anchor.join(".codewhale");
    let codewhale_exists = checked_real_directory(&codewhale_dir)?;
    if !codewhale_exists {
        if require_existing {
            bail!(
                "owned skill parent {} does not exist",
                codewhale_dir.display()
            );
        }
        return Ok(());
    }

    let skills_exists = checked_real_directory(skills_dir)?;
    if !skills_exists {
        if require_existing {
            bail!("owned skill root {} does not exist", skills_dir.display());
        }
        return Ok(());
    }

    let canonical_anchor = fs::canonicalize(anchor)
        .with_context(|| format!("failed to resolve owned anchor {}", anchor.display()))?;
    let canonical_skills = fs::canonicalize(skills_dir).with_context(|| {
        format!(
            "failed to resolve owned skill root {}",
            skills_dir.display()
        )
    })?;
    if !canonical_skills.starts_with(&canonical_anchor) {
        bail!(
            "owned skill root {} escapes anchor {}",
            skills_dir.display(),
            anchor.display()
        );
    }
    Ok(())
}

fn create_owned_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to create {}", path.display()));
        }
    }
    if !checked_real_directory(path)? {
        bail!("failed to create owned skill directory {}", path.display());
    }
    Ok(())
}

fn prepare_owned_target(
    workspace: &Path,
    home: Option<&Path>,
    target: SkillTargetScope,
) -> Result<PathBuf> {
    let skills_dir = resolve_owned_target(workspace, home, target)?;
    let anchor = owned_anchor(workspace, home, target)?;
    if !checked_real_directory(anchor)? {
        bail!("owned skill anchor {} does not exist", anchor.display());
    }

    let codewhale_dir = anchor.join(".codewhale");
    if !checked_real_directory(&codewhale_dir)? {
        create_owned_directory(&codewhale_dir)?;
    }
    validate_owned_target_chain(anchor, &skills_dir, false)?;
    if !checked_real_directory(&skills_dir)? {
        create_owned_directory(&skills_dir)?;
    }
    validate_owned_target_chain(anchor, &skills_dir, true)?;
    Ok(skills_dir)
}

fn target_scope_for_root(root: &SkillRootDescriptor) -> Result<SkillTargetScope> {
    if !root.is_writable_owned() {
        bail!("refusing to mutate non-owned root {}", root.path.display());
    }
    match root.kind {
        SkillRootKind::CodeWhaleProject => Ok(SkillTargetScope::Project),
        SkillRootKind::CodeWhaleGlobal => Ok(SkillTargetScope::Global),
        _ => bail!("refusing to mutate non-owned root {}", root.path.display()),
    }
}

fn validate_owned_root_descriptor(
    ctx: &MutationContext<'_>,
    root: &SkillRootDescriptor,
) -> Result<PathBuf> {
    let target = target_scope_for_root(root)?;
    let expected = resolve_owned_target(ctx.workspace, ctx.home, target)?;
    if root.path != expected {
        bail!(
            "audited owned root {} does not match mutation target {}",
            root.path.display(),
            expected.display()
        );
    }
    let anchor = owned_anchor(ctx.workspace, ctx.home, target)?;
    validate_owned_target_chain(anchor, &expected, true)?;
    Ok(expected)
}

/// Validate a real direct child of an already validated owned skills root.
/// Returns false only when a missing child is permitted.
fn validate_owned_child(skills_dir: &Path, child: &Path, require_existing: bool) -> Result<bool> {
    if child.parent() != Some(skills_dir) {
        bail!(
            "refusing to mutate path {} outside direct owned root {}",
            child.display(),
            skills_dir.display()
        );
    }
    let exists = checked_real_directory(child)?;
    if !exists {
        if require_existing {
            bail!("owned skill path {} does not exist", child.display());
        }
        return Ok(false);
    }

    let canonical_root = fs::canonicalize(skills_dir).with_context(|| {
        format!(
            "failed to resolve owned skill root {}",
            skills_dir.display()
        )
    })?;
    let canonical_child = fs::canonicalize(child)
        .with_context(|| format!("failed to resolve owned skill path {}", child.display()))?;
    if canonical_child.parent() != Some(canonical_root.as_path()) {
        bail!(
            "owned skill path {} escapes direct root {}",
            child.display(),
            skills_dir.display()
        );
    }
    Ok(true)
}

fn validate_owned_skill_path(
    ctx: &MutationContext<'_>,
    skill: &AuditedSkill,
    path: &Path,
) -> Result<PathBuf> {
    if skill.id.root_id != skill.root.id {
        bail!("audited skill root identity changed; refusing mutation");
    }
    let skills_dir = validate_owned_root_descriptor(ctx, &skill.root)?;
    let package_name = on_disk_package_name(&skill.id)?;
    let expected = skills_dir.join(package_name);
    if path != expected {
        bail!(
            "audited skill path {} does not match owned package {}",
            path.display(),
            expected.display()
        );
    }
    validate_owned_child(&skills_dir, path, true)?;
    Ok(skills_dir)
}

/// Resolve the on-disk CodeWhale-owned skills directory for a target scope.
pub fn resolve_owned_target(
    workspace: &Path,
    home: Option<&Path>,
    target: SkillTargetScope,
) -> Result<PathBuf> {
    let catalog = SkillRootCatalog::build(workspace, home, None);
    let kind = match target {
        SkillTargetScope::Project => SkillRootKind::CodeWhaleProject,
        SkillTargetScope::Global => SkillRootKind::CodeWhaleGlobal,
    };
    let root = catalog
        .owned_writable_roots()
        .into_iter()
        .find(|r| r.kind == kind)
        .with_context(|| format!("no owned root for {target:?}"))?;
    if !root.is_writable_owned() {
        bail!("refusing to mutate non-owned root {}", root.path.display());
    }
    let anchor = owned_anchor(workspace, home, target)?;
    validate_owned_target_chain(anchor, &root.path, false)?;
    Ok(root.path.clone())
}

/// Execute a mutation request against CodeWhale-owned roots only.
pub async fn execute(
    request: SkillMutationRequest,
    ctx: &MutationContext<'_>,
) -> Result<SkillMutationReceipt> {
    execute_one_step(SkillOneStepRequest::Mutation(request), None, ctx)
        .await
        .map(|out| out.receipt)
}

pub async fn execute_one_step(
    request: SkillOneStepRequest,
    started_by: Option<SkillInitiator>,
    ctx: &MutationContext<'_>,
) -> Result<SkillOneStepReceipt> {
    match request {
        SkillOneStepRequest::Install { spec, target } => match local_path_spec(&spec) {
            Some(raw) => install_local(raw, target, started_by, ctx),
            None => {
                let source = InstallSource::parse(&spec)?;
                install_remote(source, target, started_by, ctx).await
            }
        },
        SkillOneStepRequest::Mutation(SkillMutationRequest::InstallRemote { source, target }) => {
            install_remote(source, target, started_by, ctx).await
        }
        SkillOneStepRequest::Mutation(SkillMutationRequest::Update {
            skill_id,
            expected_digest,
        }) => update_skill(skill_id, expected_digest, started_by, ctx).await,
        SkillOneStepRequest::Mutation(SkillMutationRequest::UpdateByName {
            name,
            scope,
            expected_digest,
        }) => {
            let resolved = resolve_owned_skill_by_name(ctx, &name, scope)?;
            update_skill(
                resolved.id,
                expected_digest.or(Some(resolved.digest)),
                started_by,
                ctx,
            )
            .await
        }
        SkillOneStepRequest::Mutation(sync) => {
            let receipt = execute_sync(sync, ctx)?;
            Ok(SkillOneStepReceipt {
                receipt,
                summary: String::new(),
            })
        }
    }
}

/// Sync mutations (import / remove / trust) — safe to call without a Tokio runtime.
pub fn execute_sync(
    request: SkillMutationRequest,
    ctx: &MutationContext<'_>,
) -> Result<SkillMutationReceipt> {
    match request {
        SkillMutationRequest::ImportExternal {
            source_id,
            expected_digest,
            target,
            conflict_policy,
        } => import_external(source_id, expected_digest, target, conflict_policy, ctx),
        SkillMutationRequest::Remove {
            skill_id,
            expected_digest,
        } => remove_skill(skill_id, expected_digest, ctx),
        SkillMutationRequest::RemoveByName {
            name,
            scope,
            expected_digest,
        } => {
            let resolved = resolve_owned_skill_by_name(ctx, &name, scope)?;
            remove_skill(resolved.id, expected_digest.or(Some(resolved.digest)), ctx)
        }
        SkillMutationRequest::Trust {
            skill_id,
            expected_digest,
        } => trust_skill(skill_id, expected_digest, ctx),
        SkillMutationRequest::TrustByName {
            name,
            scope,
            expected_digest,
        } => {
            let resolved = resolve_owned_skill_by_name(ctx, &name, scope)?;
            let digest = expected_digest.unwrap_or(resolved.digest);
            trust_skill(resolved.id, digest, ctx)
        }
        SkillMutationRequest::InstallRemote { .. }
        | SkillMutationRequest::Update { .. }
        | SkillMutationRequest::UpdateByName { .. } => {
            bail!("this mutation requires async execute (network I/O)")
        }
    }
}

#[derive(Debug)]
struct ResolvedOwnedSkill {
    id: AuditedSkillId,
    digest: String,
}

fn resolve_owned_skill_by_name(
    ctx: &MutationContext<'_>,
    name: &str,
    scope: Option<SkillTargetScope>,
) -> Result<ResolvedOwnedSkill> {
    // Match SkillRegistry::get — users may pass frontmatter/dir forms
    // (Hello_World) while audit stores the folded canonical key (hello-world).
    let canonical = normalize_skill_name_for_lookup(name);
    let snap = scan_with_configured(
        ctx.workspace,
        ctx.home,
        ctx.configured_skills_dir,
        SkillAuditMode::OwnedOnly,
        None,
    );
    let mut matches: Vec<&AuditedSkill> = snap
        .skills
        .iter()
        .filter(|s| s.id.canonical_name == canonical)
        .filter(|s| s.root.is_writable_owned())
        .collect();

    if let Some(scope) = scope {
        let want = match scope {
            SkillTargetScope::Project => SkillRootKind::CodeWhaleProject,
            SkillTargetScope::Global => SkillRootKind::CodeWhaleGlobal,
        };
        matches.retain(|s| s.root.kind == want);
    }

    match matches.as_slice() {
        [] => {
            // If the name only exists externally, tell the user to import.
            let compatible = scan_with_configured(
                ctx.workspace,
                ctx.home,
                ctx.configured_skills_dir,
                SkillAuditMode::Compatible,
                None,
            );
            if compatible.skills.iter().any(|s| {
                s.id.canonical_name == canonical
                    && s.source_kind == SkillSourceKind::CompatibleExternal
            }) {
                bail!(
                    "skill '{name}' exists only in a compatible external root; \
                     import it with /skills (refusing to write external harness directories)"
                );
            }
            bail!("skill '{name}' not found in Codewhale-owned project/global roots");
        }
        [only] => {
            let DigestState::Known(digest) = &only.digest else {
                bail!("skill '{name}' has unknown package digest; refusing mutation");
            };
            Ok(ResolvedOwnedSkill {
                id: only.id.clone(),
                digest: digest.clone(),
            })
        }
        _ => bail!(
            "skill '{name}' exists in both project and global Codewhale roots; \
             specify --project or --global"
        ),
    }
}

fn find_audited_skill(
    ctx: &MutationContext<'_>,
    skill_id: &AuditedSkillId,
) -> Result<(AuditedSkill, PathBuf)> {
    let snap = scan_with_configured(
        ctx.workspace,
        ctx.home,
        ctx.configured_skills_dir,
        SkillAuditMode::Compatible,
        None,
    );
    let skill = snap
        .skills
        .into_iter()
        .find(|s| &s.id == skill_id)
        .with_context(|| format!("audited skill {} not found", skill_id.canonical_name))?;
    let path = skill.root.path.join(&skill.id.relative_dir);
    Ok((skill, path))
}

/// Directory segment under the skills root. Prefer this over `canonical_name`
/// when calling install helpers that join `skills_dir / name` — installs use
/// raw frontmatter names, while audit stores a normalized lookup key.
fn on_disk_package_name(skill_id: &AuditedSkillId) -> Result<&str> {
    let mut components = skill_id.relative_dir.components();
    let name = match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => name.to_str(),
        _ => None,
    }
    .filter(|name| !name.is_empty())
    .ok_or_else(|| {
        anyhow::anyhow!(
            "invalid on-disk package directory for skill '{}'",
            skill_id.canonical_name
        )
    })?;
    Ok(name)
}

fn verify_expected_digest(path: &Path, expected: Option<&str>) -> Result<Option<String>> {
    let current = package_digest::compute_package_digest(path)
        .with_context(|| format!("cannot digest {}", path.display()))?;
    if let Some(expected) = expected
        && expected != current
    {
        bail!(
            "skill content changed since audit (expected {expected}, found {current}); \
             re-review before mutating"
        );
    }
    Ok(Some(current))
}

fn ensure_remote_updatable(skill_dir: &Path) -> Result<()> {
    let marker_path = skill_dir.join(install::INSTALLED_FROM_MARKER);
    let body = fs::read_to_string(&marker_path)
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("malformed {}", install::INSTALLED_FROM_MARKER))?;
    let spec = value
        .get("spec")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !install::is_registry_updatable_spec(spec) {
        bail!(
            "skill was imported locally (spec '{spec}') and cannot be updated from a registry; \
             re-import or remove it first"
        );
    }
    Ok(())
}

const LOCAL_SPEC_PREFIX: &str = "path:";
const MAX_LOCAL_ARCHIVE_ENTRIES: usize = 512;

#[must_use]
pub fn local_path_spec(spec: &str) -> Option<&str> {
    let spec = spec.trim();
    if let Some(rest) = spec.strip_prefix(LOCAL_SPEC_PREFIX) {
        return Some(rest.trim());
    }
    let bytes = spec.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let local = matches!(spec, "." | "..")
        || spec.starts_with(['/', '~', '\\'])
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.contains('\\')
        || drive;
    local.then_some(spec)
}

fn scope_label(target: SkillTargetScope) -> &'static str {
    match target {
        SkillTargetScope::Project => "project",
        SkillTargetScope::Global => "global",
    }
}

fn script_count(skill_dir: &Path) -> usize {
    fn count(dir: &Path, depth: usize) -> usize {
        if depth > 16 {
            return 0;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|entry| match entry.file_type() {
                Ok(kind) if kind.is_dir() => count(&entry.path(), depth + 1),
                Ok(kind) if kind.is_file() => 1,
                _ => 0,
            })
            .sum()
    }
    count(&skill_dir.join("scripts"), 0)
}

fn skill_is_on(name: &str, home: Option<&Path>) -> bool {
    let store = match (codewhale_config::codewhale_home_is_explicit(), home) {
        (false, Some(home)) => codewhale_runtime::skill_state::SkillStateStore::load_from(
            home.join(".codewhale").join("skills_state.toml"),
        ),
        _ => codewhale_runtime::skill_state::SkillStateStore::load_default(),
    };
    let canonical = normalize_skill_name_for_lookup(name);
    let legacy = (canonical != name).then_some(name);
    store
        .map(|state| state.is_enabled_with_legacy(&canonical, legacy))
        .unwrap_or(true)
}

fn receipt_summary(
    verb: &str,
    name: &str,
    target: SkillTargetScope,
    spec: &str,
    path: &Path,
    replaced: Option<&str>,
    home: Option<&Path>,
) -> String {
    let mut line = format!(
        "{verb} skill {name} ({}) from {spec}, {}",
        scope_label(target),
        if skill_is_on(name, home) { "on" } else { "off" }
    );
    if let Some(old) = replaced {
        line.push_str(&format!("; replaces the copy from {old}"));
    }
    match script_count(path) {
        0 => {}
        1 => line.push_str("; includes 1 script"),
        n => line.push_str(&format!("; includes {n} scripts")),
    }
    line
}

struct InstalledMarker {
    spec: String,
    content_digest: Option<String>,
}

fn read_installed_marker(skill_dir: &Path) -> Result<Option<InstalledMarker>> {
    let marker_path = skill_dir.join(install::INSTALLED_FROM_MARKER);
    match fs::symlink_metadata(&marker_path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to inspect {}", marker_path.display()));
        }
    }
    let body = fs::read_to_string(&marker_path)
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("malformed {}", install::INSTALLED_FROM_MARKER))?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Ok(Some(InstalledMarker {
        spec: text("spec").unwrap_or_default(),
        content_digest: text("content_digest"),
    }))
}

fn installed_specs(skills_dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(skills_dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let meta = fs::symlink_metadata(entry.path()).ok()?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return None;
            }
            let marker = read_installed_marker(&entry.path()).ok()??;
            Some((name, marker.spec))
        })
        .collect();
    found.sort();
    found
}

fn record_person_approval(
    initiator: Option<SkillInitiator>,
    target: SkillTargetScope,
    skills_dir: &Path,
    path: &Path,
    digest: Option<&str>,
) -> Result<()> {
    if initiator != Some(SkillInitiator::Person) || target != SkillTargetScope::Global {
        return Ok(());
    }
    let Some(digest) = digest else {
        return Ok(());
    };
    validate_owned_child(skills_dir, path, true)?;
    write_trust_v2(path, digest)
}

async fn install_remote(
    source: InstallSource,
    target: SkillTargetScope,
    initiator: Option<SkillInitiator>,
    ctx: &MutationContext<'_>,
) -> Result<SkillOneStepReceipt> {
    let skills_dir = prepare_owned_target(ctx.workspace, ctx.home, target)?;
    let anchor = owned_anchor(ctx.workspace, ctx.home, target)?;

    let spec = install::source_spec_string(&source);
    let before_specs = installed_specs(&skills_dir);
    let same_source = before_specs
        .iter()
        .find(|(_, installed)| *installed == spec)
        .map(|(name, _)| name.clone());

    enum Placed {
        Installed(install::InstalledSkill),
        NoChange(String),
        Approval(String),
        Denied(String),
    }
    let placed = match &same_source {
        Some(name) => match install::update_with_registry(
            name,
            &skills_dir,
            ctx.max_size,
            ctx.network,
            ctx.registry_url,
        )
        .await?
        {
            UpdateResult::Updated(installed) => Placed::Installed(installed),
            UpdateResult::NoChange => Placed::NoChange(name.clone()),
            UpdateResult::NeedsApproval(host) => Placed::Approval(host),
            UpdateResult::NetworkDenied(host) => Placed::Denied(host),
        },
        None => match install::install_with_registry(
            source,
            &skills_dir,
            ctx.max_size,
            ctx.network,
            true,
            ctx.registry_url,
        )
        .await?
        {
            InstallOutcome::Installed(installed) => Placed::Installed(installed),
            InstallOutcome::NeedsApproval(host) => Placed::Approval(host),
            InstallOutcome::NetworkDenied(host) => Placed::Denied(host),
        },
    };
    validate_owned_target_chain(anchor, &skills_dir, true)?;

    let blocked = |outcome: SkillMutationOutcome| SkillOneStepReceipt {
        receipt: SkillMutationReceipt {
            action: SkillActionKind::Install,
            name: String::new(),
            scope: target.as_skill_scope(),
            safe_target_path: safe_display_path(&skills_dir, Some(ctx.workspace), ctx.home),
            before_digest: None,
            after_digest: None,
            outcome,
        },
        summary: String::new(),
    };

    match placed {
        Placed::Installed(installed) => {
            validate_owned_child(&skills_dir, &installed.path, true)?;
            let after = package_digest::compute_package_digest(&installed.path).ok();
            let previous = before_specs
                .iter()
                .find(|(name, _)| *name == installed.name)
                .map(|(_, old)| old.as_str());
            let replaced = previous.filter(|old| **old != *spec);
            record_person_approval(
                initiator,
                target,
                &skills_dir,
                &installed.path,
                after.as_deref(),
            )?;
            let (action, outcome, verb) = if previous.is_some() {
                (
                    SkillActionKind::Update,
                    SkillMutationOutcome::Updated,
                    "Updated",
                )
            } else {
                (
                    SkillActionKind::Install,
                    SkillMutationOutcome::Installed,
                    "Installed",
                )
            };
            let summary = receipt_summary(
                verb,
                &installed.name,
                target,
                &spec,
                &installed.path,
                replaced,
                ctx.home,
            );
            Ok(SkillOneStepReceipt {
                receipt: SkillMutationReceipt {
                    action,
                    name: installed.name,
                    scope: target.as_skill_scope(),
                    safe_target_path: safe_display_path(
                        &installed.path,
                        Some(ctx.workspace),
                        ctx.home,
                    ),
                    before_digest: None,
                    after_digest: after,
                    outcome,
                },
                summary,
            })
        }
        Placed::NoChange(name) => {
            let path = skills_dir.join(&name);
            validate_owned_child(&skills_dir, &path, true)?;
            let current = package_digest::compute_package_digest(&path).ok();
            let recorded = read_installed_marker(&path)?.and_then(|m| m.content_digest);
            if current.is_some() && current == recorded {
                record_person_approval(initiator, target, &skills_dir, &path, current.as_deref())?;
            }
            let summary = format!(
                "Skill {name} ({}) is already up to date with {spec}",
                scope_label(target)
            );
            Ok(SkillOneStepReceipt {
                receipt: SkillMutationReceipt {
                    action: SkillActionKind::Update,
                    name,
                    scope: target.as_skill_scope(),
                    safe_target_path: safe_display_path(&path, Some(ctx.workspace), ctx.home),
                    before_digest: current.clone(),
                    after_digest: current,
                    outcome: SkillMutationOutcome::NoChange,
                },
                summary,
            })
        }
        Placed::Approval(host) => Ok(blocked(SkillMutationOutcome::NeedsApproval(host))),
        Placed::Denied(host) => Ok(blocked(SkillMutationOutcome::NetworkDenied(host))),
    }
}

fn expand_local_source(raw: &str, ctx: &MutationContext<'_>) -> Result<PathBuf> {
    if raw.is_empty() {
        bail!("local skill path must not be empty");
    }
    if let Some(rest) = raw.strip_prefix('~') {
        let rest = match rest {
            "" => "",
            _ if rest.starts_with(['/', '\\']) => rest.trim_start_matches(['/', '\\']),
            _ => bail!("only '~' for your own home directory is supported (got {raw})"),
        };
        let home = ctx.home.context("'~' needs a home directory")?;
        return Ok(home.join(rest));
    }
    let path = Path::new(raw);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.workspace.join(path)
    })
}

fn is_reserved_root_marker(name: &str) -> bool {
    name.eq_ignore_ascii_case(install::INSTALLED_FROM_MARKER)
        || name.eq_ignore_ascii_case(install::TRUSTED_MARKER)
        || name.eq_ignore_ascii_case(".system-installed-version")
}

fn is_tar_gz_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
        })
}

fn map_archive_limit(err: anyhow::Error, max_size: u64) -> anyhow::Error {
    bounded_tar::map_stream_limit(err, || {
        anyhow::anyhow!("skill archive exceeds maximum unpacked size of {max_size} bytes")
    })
}

fn has_regular_skill_md(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join("SKILL.md")).is_ok_and(|meta| meta.is_file())
}

fn strip_reserved_markers_at_root(skill_root: &Path) -> Result<()> {
    for entry in fs::read_dir(skill_root)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_reserved_root_marker(&name.to_string_lossy()) {
            let entry_path = entry.path();
            let meta = fs::symlink_metadata(&entry_path)?;
            if meta.is_dir() {
                fs::remove_dir_all(&entry_path)?;
            } else {
                fs::remove_file(&entry_path)?;
            }
        }
    }
    Ok(())
}

fn normalize_archive_entry_path(raw_bytes: &[u8], raw_path: &Path) -> Result<Option<PathBuf>> {
    if raw_bytes.iter().any(|&b| b == 0 || b == b'\\') || !install::is_safe_path(raw_path) {
        bail!("unsafe archive entry path {}", raw_path.display());
    }
    let mut normalized = PathBuf::new();
    for component in raw_path.components() {
        match component {
            Component::Normal(seg) => normalized.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe archive entry path {}", raw_path.display());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(normalized))
    }
}

fn resolve_unpacked_skill_root(unpack_root: &Path) -> Result<PathBuf> {
    if has_regular_skill_md(unpack_root) {
        return Ok(unpack_root.to_path_buf());
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(unpack_root)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            bail!("refusing symlinked entry in unpacked skill archive");
        }
        if meta.is_dir() {
            dirs.push(entry.path());
        } else {
            files.push(entry.path());
        }
    }
    if dirs.len() == 1 && files.is_empty() && has_regular_skill_md(&dirs[0]) {
        return Ok(dirs.swap_remove(0));
    }
    bail!("skill archive does not contain SKILL.md at root or inside a single top-level directory");
}

fn unpack_tar_gz(bytes: &[u8], max_size: u64) -> Result<(tempfile::TempDir, PathBuf)> {
    let unpack_dir = tempfile::TempDir::new()?;
    let mut archive = bounded_tar::open_bounded_tarball(bytes, max_size);
    let entries = archive
        .entries()
        .map_err(|err| map_archive_limit(err.into(), max_size))?;
    let mut entry_count: usize = 0;
    let mut total_unpacked: u64 = 0;

    for next in entries {
        let mut entry = next.map_err(|err| {
            map_archive_limit(
                anyhow::Error::from(err).context("failed to read skill archive entry"),
                max_size,
            )
        })?;
        entry_count += 1;
        if entry_count > MAX_LOCAL_ARCHIVE_ENTRIES {
            bail!("skill archive exceeds maximum entry count ({MAX_LOCAL_ARCHIVE_ENTRIES})");
        }
        if !bounded_tar::entry_names_within_bound(&entry) {
            bail!(
                "archive entry path exceeds maximum length ({} bytes)",
                bounded_tar::MAX_ENTRY_NAME_BYTES
            );
        }
        let entry_type = entry.header().entry_type();
        if entry_type == tar::EntryType::XGlobalHeader {
            continue;
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            bail!("refusing symlink or hardlink entry in skill archive");
        }
        if !entry_type.is_dir() && !entry_type.is_file() {
            bail!("refusing unsupported entry type ({entry_type:?}) in skill archive");
        }

        let path_bytes = entry.path_bytes().into_owned();
        let raw_path = entry
            .path()
            .context("skill archive entry has invalid path")?
            .into_owned();
        let Some(rel) = normalize_archive_entry_path(&path_bytes, &raw_path)? else {
            if entry_type.is_dir() {
                continue;
            }
            bail!("unsafe empty file path in skill archive");
        };

        let dest = unpack_dir.path().join(&rel);
        if entry_type.is_dir() {
            if let Ok(meta) = fs::symlink_metadata(&dest)
                && !meta.is_dir()
            {
                bail!(
                    "archive directory conflicts with existing file at {}",
                    rel.display()
                );
            }
            fs::create_dir_all(&dest)?;
            continue;
        }

        let remaining = max_size.saturating_sub(total_unpacked);
        let declared_size = entry.header().size().unwrap_or(0);
        if declared_size > remaining {
            bail!("skill archive exceeds maximum unpacked size of {max_size} bytes");
        }
        let body = bounded_tar::read_body_within(&mut entry, remaining)
            .map_err(|err| map_archive_limit(err.into(), max_size))?
            .ok_or_else(|| {
                anyhow::anyhow!("skill archive exceeds maximum unpacked size of {max_size} bytes")
            })?;
        total_unpacked = total_unpacked.saturating_add(body.len() as u64);

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .with_context(|| format!("failed to unpack archive entry {}", rel.display()))?;
        out.write_all(&body)?;
        #[cfg(unix)]
        {
            let mode = entry.header().mode().unwrap_or(0o644);
            let perms = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
            fs::set_permissions(&dest, fs::Permissions::from_mode(perms))?;
        }
    }

    let skill_root = resolve_unpacked_skill_root(unpack_dir.path())?;
    strip_reserved_markers_at_root(&skill_root)?;
    Ok((unpack_dir, skill_root))
}

struct PreparedLocalSource {
    _unpacked: Option<tempfile::TempDir>,
    package_dir: PathBuf,
    provenance_spec: String,
}

fn prepare_local_source(given: &Path, ctx: &MutationContext<'_>) -> Result<PreparedLocalSource> {
    match fs::symlink_metadata(given) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing symlinked skill source {}", given.display())
        }
        Ok(meta) if meta.is_dir() => {
            let source = fs::canonicalize(given)
                .with_context(|| format!("cannot resolve skill source {}", given.display()))?;
            let provenance_spec = format!("{LOCAL_SPEC_PREFIX}{}", source.display());
            Ok(PreparedLocalSource {
                _unpacked: None,
                package_dir: source,
                provenance_spec,
            })
        }
        Ok(meta) if meta.is_file() => {
            if !is_tar_gz_path(given) {
                bail!(
                    "{} is a file; local installs support directories or .tar.gz / .tgz archives",
                    given.display()
                );
            }
            let compressed_cap = bounded_tar::stream_cap(ctx.max_size).max(1024);
            if meta.len() > compressed_cap {
                bail!(
                    "skill archive {} exceeds maximum compressed size of {} bytes",
                    given.display(),
                    compressed_cap
                );
            }
            let file = fs::File::open(given)
                .with_context(|| format!("cannot read skill archive {}", given.display()))?;
            let mut bytes = Vec::new();
            Read::take(file, compressed_cap.saturating_add(1))
                .read_to_end(&mut bytes)
                .with_context(|| format!("cannot read skill archive {}", given.display()))?;
            if bytes.len() as u64 > compressed_cap {
                bail!(
                    "skill archive {} exceeds maximum compressed size of {} bytes",
                    given.display(),
                    compressed_cap
                );
            }
            let canonical_archive = fs::canonicalize(given)
                .with_context(|| format!("cannot resolve skill archive {}", given.display()))?;
            let (tempdir, skill_root) = unpack_tar_gz(&bytes, ctx.max_size)?;
            let provenance_spec = format!("{LOCAL_SPEC_PREFIX}{}", canonical_archive.display());
            Ok(PreparedLocalSource {
                _unpacked: Some(tempdir),
                package_dir: skill_root,
                provenance_spec,
            })
        }
        Ok(_) => bail!("refusing non-regular skill source {}", given.display()),
        Err(err) => {
            Err(err).with_context(|| format!("cannot read skill source {}", given.display()))
        }
    }
}

fn skill_name_from_markdown(skill_dir: &Path) -> Result<String> {
    let skill_md = skill_dir.join("SKILL.md");
    match fs::symlink_metadata(&skill_md) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing symlinked {}", skill_md.display())
        }
        Ok(meta) if meta.is_file() => {}
        Ok(_) => bail!("{} is not a regular file", skill_md.display()),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!(
                "{} has no SKILL.md; nothing to install",
                skill_dir.display()
            )
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", skill_md.display()));
        }
    }
    let content = fs::read_to_string(&skill_md).context("SKILL.md is not valid UTF-8 text")?;
    let parsed = parse_frontmatter(&content).map_err(anyhow::Error::msg)?;
    validate_skill_frontmatter(
        parsed.as_ref().map(|(metadata, _)| metadata),
        None,
        SkillValidationMode::Strict,
    )
    .map_err(anyhow::Error::msg)?;
    let (metadata, _) = parsed.context("SKILL.md frontmatter is missing")?;
    let name = metadata.get("name").context("SKILL.md has no name")?;
    Ok(install::validate_skill_name_segment(name)?.to_string())
}

fn install_local(
    raw: &str,
    target: SkillTargetScope,
    initiator: Option<SkillInitiator>,
    ctx: &MutationContext<'_>,
) -> Result<SkillOneStepReceipt> {
    let given = expand_local_source(raw, ctx)?;
    let prepared = prepare_local_source(&given, ctx)?;
    let name = skill_name_from_markdown(&prepared.package_dir)?;
    place_local(
        &prepared,
        &name,
        target,
        initiator,
        ctx,
        SkillActionKind::Install,
    )
}

fn place_local(
    prepared: &PreparedLocalSource,
    name: &str,
    target: SkillTargetScope,
    initiator: Option<SkillInitiator>,
    ctx: &MutationContext<'_>,
    action: SkillActionKind,
) -> Result<SkillOneStepReceipt> {
    let source = &prepared.package_dir;
    let spec = prepared.provenance_spec.as_str();
    let source_digest = package_digest::compute_package_digest(source)
        .context("source package is not safe to copy")?;
    let skills_dir = prepare_owned_target(ctx.workspace, ctx.home, target)?;
    let anchor = owned_anchor(ctx.workspace, ctx.home, target)?;
    let dest = skills_dir.join(name);

    let dest_exists = validate_owned_child(&skills_dir, &dest, false)?;
    let mut previous: Option<InstalledMarker> = None;
    let mut before = None;
    if dest_exists {
        previous = read_installed_marker(&dest)?;
        if previous.is_none() {
            bail!(
                "skill '{name}' already exists and was not installed by Codewhale; \
                 refusing to replace it"
            );
        }
        before = package_digest::compute_package_digest(&dest).ok();
        let same_source = previous.as_ref().is_some_and(|m| m.spec == spec);
        let recorded = previous.as_ref().and_then(|m| m.content_digest.clone());
        if same_source && before.as_deref() == Some(source_digest.as_str()) && before == recorded {
            record_person_approval(initiator, target, &skills_dir, &dest, before.as_deref())?;
            let summary = format!(
                "Skill {name} ({}) is already up to date with {spec}",
                scope_label(target)
            );
            return Ok(SkillOneStepReceipt {
                receipt: SkillMutationReceipt {
                    action,
                    name: name.to_string(),
                    scope: target.as_skill_scope(),
                    safe_target_path: safe_display_path(&dest, Some(ctx.workspace), ctx.home),
                    before_digest: before.clone(),
                    after_digest: before,
                    outcome: SkillMutationOutcome::NoChange,
                },
                summary,
            });
        }
    }

    let after = place_package(&PlaceRequest {
        skills_dir: &skills_dir,
        anchor,
        source,
        expected_digest: &source_digest,
        dest: &dest,
        segment: name,
        dest_exists,
        spec,
        installed_name: name,
    })?;
    record_person_approval(initiator, target, &skills_dir, &dest, Some(&after))?;

    let replaced = previous
        .as_ref()
        .map(|m| m.spec.as_str())
        .filter(|old| *old != spec);
    let (outcome, verb) = if previous.is_some() {
        (SkillMutationOutcome::Updated, "Updated")
    } else {
        (SkillMutationOutcome::Installed, "Installed")
    };
    let summary = receipt_summary(verb, name, target, spec, &dest, replaced, ctx.home);
    Ok(SkillOneStepReceipt {
        receipt: SkillMutationReceipt {
            action,
            name: name.to_string(),
            scope: target.as_skill_scope(),
            safe_target_path: safe_display_path(&dest, Some(ctx.workspace), ctx.home),
            before_digest: before,
            after_digest: Some(after),
            outcome,
        },
        summary,
    })
}

fn import_external(
    source_id: AuditedSkillId,
    expected_digest: String,
    target: SkillTargetScope,
    conflict_policy: ConflictPolicy,
    ctx: &MutationContext<'_>,
) -> Result<SkillMutationReceipt> {
    let (source_skill, source_path) = find_audited_skill(ctx, &source_id)?;
    if source_skill.source_kind != SkillSourceKind::CompatibleExternal {
        bail!("import source must be a compatible external skill");
    }
    if source_skill.path_unsafe {
        bail!("refusing to import unsafe external skill package");
    }

    let owned_snap = scan_with_configured(
        ctx.workspace,
        ctx.home,
        ctx.configured_skills_dir,
        SkillAuditMode::OwnedOnly,
        None,
    );
    let owned_has_name = owned_snap
        .skills
        .iter()
        .any(|s| s.id.canonical_name == source_id.canonical_name);

    if !owned_has_name && !source_skill.import_candidate {
        bail!("skill is not an import candidate");
    }

    let before = verify_expected_digest(&source_path, Some(&expected_digest))?;

    let skills_dir = prepare_owned_target(ctx.workspace, ctx.home, target)?;
    let anchor = owned_anchor(ctx.workspace, ctx.home, target)?;

    let want_kind = match target {
        SkillTargetScope::Project => SkillRootKind::CodeWhaleProject,
        SkillTargetScope::Global => SkillRootKind::CodeWhaleGlobal,
    };
    let existing_in_target: Vec<&AuditedSkill> = owned_snap
        .skills
        .iter()
        .filter(|s| s.id.canonical_name == source_id.canonical_name)
        .filter(|s| s.root.kind == want_kind)
        .collect();

    // Prefer the on-disk directory of an existing same-scope package so we
    // replace/reject against Hello_World rather than creating a parallel
    // hello-world next to it.
    let (dest, dest_segment) = match existing_in_target.as_slice() {
        [] => {
            let segment = source_id.canonical_name.clone();
            (skills_dir.join(&segment), segment)
        }
        [only] => {
            let segment = on_disk_package_name(&only.id)?.to_string();
            (only.root.path.join(&only.id.relative_dir), segment)
        }
        _ => bail!(
            "skill '{}' has multiple owned packages in the target scope; \
             remove or consolidate them before importing",
            source_id.canonical_name
        ),
    };

    let dest_exists = validate_owned_child(&skills_dir, &dest, false)?;
    if dest_exists {
        let existing = package_digest::compute_package_digest(&dest).ok();
        if existing.as_deref() == Some(expected_digest.as_str()) {
            return Ok(SkillMutationReceipt {
                action: SkillActionKind::Import,
                name: source_id.canonical_name.clone(),
                scope: target.as_skill_scope(),
                safe_target_path: safe_display_path(&dest, Some(ctx.workspace), ctx.home),
                before_digest: before,
                after_digest: existing,
                outcome: SkillMutationOutcome::AlreadyPresent,
            });
        }
        if conflict_policy == ConflictPolicy::Reject {
            bail!(
                "destination {} already exists with different content",
                dest.display()
            );
        }
    }

    let after = place_package(&PlaceRequest {
        skills_dir: &skills_dir,
        anchor,
        source: &source_path,
        expected_digest: &expected_digest,
        dest: &dest,
        segment: &dest_segment,
        dest_exists,
        spec: &format!("import:{}", source_skill.safe_display_path),
        installed_name: &source_id.canonical_name,
    })?;

    Ok(SkillMutationReceipt {
        action: SkillActionKind::Import,
        name: source_id.canonical_name,
        scope: target.as_skill_scope(),
        safe_target_path: safe_display_path(&dest, Some(ctx.workspace), ctx.home),
        before_digest: before,
        after_digest: Some(after),
        outcome: SkillMutationOutcome::Imported,
    })
}

struct PlaceRequest<'a> {
    skills_dir: &'a Path,
    anchor: &'a Path,
    source: &'a Path,
    expected_digest: &'a str,
    dest: &'a Path,
    segment: &'a str,
    dest_exists: bool,
    spec: &'a str,
    installed_name: &'a str,
}

fn place_package(req: &PlaceRequest<'_>) -> Result<String> {
    let PlaceRequest {
        skills_dir,
        anchor,
        source,
        expected_digest,
        dest,
        segment,
        dest_exists,
        spec,
        installed_name,
    } = *req;

    // Re-verify source digest immediately before copy (TOCTOU).
    let _ = verify_expected_digest(source, Some(expected_digest))?;
    validate_owned_target_chain(anchor, skills_dir, true)?;

    let staging = skills_dir.join(format!("{segment}.tmp"));
    if validate_owned_child(skills_dir, &staging, false)? {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to clean stale staging dir {}", staging.display()))?;
    }
    copy_skill_package(source, &staging)?;
    validate_owned_target_chain(anchor, skills_dir, true)?;
    validate_owned_child(skills_dir, &staging, true)?;
    package_digest::compute_package_digest(&staging)
        .context("staged package failed digest validation")?;

    let mut backup_path: Option<PathBuf> = None;
    if dest_exists {
        validate_owned_child(skills_dir, dest, true)?;
        let backup = skills_dir.join(format!("{segment}.bak"));
        if validate_owned_child(skills_dir, &backup, false)? {
            fs::remove_dir_all(&backup).with_context(|| {
                format!("failed to clean stale backup dir {}", backup.display())
            })?;
        }
        fs::rename(dest, &backup)
            .with_context(|| format!("failed to backup {}", dest.display()))?;
        if let Err(err) = fs::rename(&staging, dest) {
            let _ = fs::rename(&backup, dest);
            let _ = fs::remove_dir_all(&staging);
            return Err(err).context("failed to replace destination with new package");
        }
        backup_path = Some(backup);
    } else if let Err(err) = fs::rename(&staging, dest) {
        let _ = fs::remove_dir_all(&staging);
        return Err(err).context("failed to install package");
    }
    validate_owned_target_chain(anchor, skills_dir, true)?;
    validate_owned_child(skills_dir, dest, true)?;

    let finalize = (|| -> Result<String> {
        let _ = verify_expected_digest(source, Some(expected_digest))?;
        validate_owned_target_chain(anchor, skills_dir, true)?;
        validate_owned_child(skills_dir, dest, true)?;
        let after = package_digest::compute_package_digest(dest)?;
        if after != expected_digest {
            bail!("package content digest mismatch after copy; aborted");
        }
        write_installed_from_v2(dest, spec, None, expected_digest, &after, installed_name)?;
        Ok(after)
    })();

    match finalize {
        Ok(after) => {
            if let Some(backup) = backup_path.take() {
                fs::remove_dir_all(&backup).ok();
            }
            Ok(after)
        }
        Err(err) => {
            let _ = fs::remove_dir_all(dest);
            if let Some(backup) = backup_path.take() {
                let _ = fs::rename(&backup, dest);
            }
            Err(err)
        }
    }
}

fn copy_skill_package(src: &Path, dest: &Path) -> Result<()> {
    // Fail closed: source must already pass package digest (no symlinks).
    package_digest::compute_package_digest(src).context("source package is not safe to copy")?;
    copy_dir_regular_files(src, dest, true)?;
    Ok(())
}

fn copy_dir_regular_files(src: &Path, dest: &Path, package_root: bool) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            bail!("refusing to copy symlink {}", path.display());
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if package_root && is_reserved_root_marker(&name_str) {
            continue;
        }
        let target = dest.join(&name);
        if meta.is_dir() {
            copy_dir_regular_files(&path, &target, false)?;
        } else if meta.is_file() {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

async fn update_skill(
    skill_id: AuditedSkillId,
    expected_digest: Option<String>,
    initiator: Option<SkillInitiator>,
    ctx: &MutationContext<'_>,
) -> Result<SkillOneStepReceipt> {
    let (skill, path) = find_audited_skill(ctx, &skill_id)?;
    if !skill.root.is_writable_owned() {
        bail!("refusing to update skill outside Codewhale-owned roots");
    }
    if skill.source_kind != SkillSourceKind::CodeWhaleManaged {
        bail!("only Codewhale managed skills can be updated");
    }
    let skills_dir = validate_owned_skill_path(ctx, &skill, &path)?;
    // Imported skills carry `import:…` provenance and must not hit the registry.
    ensure_remote_updatable(&path)?;
    let before = verify_expected_digest(&path, expected_digest.as_deref())?;
    let scope = match skill.root.kind {
        SkillRootKind::CodeWhaleProject => SkillScope::Project,
        SkillRootKind::CodeWhaleGlobal => SkillScope::Global,
        _ => SkillScope::Logical,
    };

    let package_name = on_disk_package_name(&skill_id)?;
    validate_owned_skill_path(ctx, &skill, &path)?;
    let installed_spec = read_installed_marker(&path)?
        .map(|m| m.spec)
        .unwrap_or_default();
    if let Some(raw) = installed_spec.strip_prefix(LOCAL_SPEC_PREFIX) {
        let source_path = expand_local_source(raw, ctx)?;
        let prepared = prepare_local_source(&source_path, ctx)?;
        let name = skill_name_from_markdown(&prepared.package_dir)?;
        if name != package_name {
            bail!(
                "the source {} now names its skill '{name}', not '{package_name}'; \
                 install it again to add it as a new skill",
                source_path.display()
            );
        }
        let target = target_scope_for_root(&skill.root)?;
        return place_local(
            &prepared,
            &name,
            target,
            initiator,
            ctx,
            SkillActionKind::Update,
        );
    }
    let outcome = install::update_with_registry(
        package_name,
        &skills_dir,
        ctx.max_size,
        ctx.network,
        ctx.registry_url,
    )
    .await?;
    validate_owned_root_descriptor(ctx, &skill.root)?;
    validate_owned_child(&skills_dir, &path, true)?;

    match outcome {
        UpdateResult::NoChange => {
            let target = target_scope_for_root(&skill.root)?;
            let recorded = read_installed_marker(&path)?.and_then(|m| m.content_digest);
            if before.is_some() && before == recorded {
                record_person_approval(initiator, target, &skills_dir, &path, before.as_deref())?;
            }
            let summary = format!(
                "Skill {} ({}) is already up to date with {installed_spec}",
                skill_id.canonical_name,
                scope_label(target)
            );
            Ok(SkillOneStepReceipt {
                receipt: SkillMutationReceipt {
                    action: SkillActionKind::Update,
                    name: skill_id.canonical_name,
                    scope,
                    safe_target_path: safe_display_path(&path, Some(ctx.workspace), ctx.home),
                    before_digest: before.clone(),
                    after_digest: before,
                    outcome: SkillMutationOutcome::NoChange,
                },
                summary,
            })
        }
        UpdateResult::Updated(installed) => {
            let target = target_scope_for_root(&skill.root)?;
            let after = package_digest::compute_package_digest(&installed.path).ok();
            record_person_approval(
                initiator,
                target,
                &skills_dir,
                &installed.path,
                after.as_deref(),
            )?;
            let summary = receipt_summary(
                "Updated",
                &installed.name,
                target,
                &installed_spec,
                &installed.path,
                None,
                ctx.home,
            );
            Ok(SkillOneStepReceipt {
                receipt: SkillMutationReceipt {
                    action: SkillActionKind::Update,
                    name: installed.name,
                    scope,
                    safe_target_path: safe_display_path(
                        &installed.path,
                        Some(ctx.workspace),
                        ctx.home,
                    ),
                    before_digest: before,
                    after_digest: after,
                    outcome: SkillMutationOutcome::Updated,
                },
                summary,
            })
        }
        UpdateResult::NeedsApproval(host) => Ok(SkillOneStepReceipt {
            receipt: SkillMutationReceipt {
                action: SkillActionKind::Update,
                name: skill_id.canonical_name,
                scope,
                safe_target_path: safe_display_path(&path, Some(ctx.workspace), ctx.home),
                before_digest: before,
                after_digest: None,
                outcome: SkillMutationOutcome::NeedsApproval(host),
            },
            summary: String::new(),
        }),
        UpdateResult::NetworkDenied(host) => Ok(SkillOneStepReceipt {
            receipt: SkillMutationReceipt {
                action: SkillActionKind::Update,
                name: skill_id.canonical_name,
                scope,
                safe_target_path: safe_display_path(&path, Some(ctx.workspace), ctx.home),
                before_digest: before,
                after_digest: None,
                outcome: SkillMutationOutcome::NetworkDenied(host),
            },
            summary: String::new(),
        }),
    }
}

fn remove_skill(
    skill_id: AuditedSkillId,
    expected_digest: Option<String>,
    ctx: &MutationContext<'_>,
) -> Result<SkillMutationReceipt> {
    let (skill, path) = find_audited_skill(ctx, &skill_id)?;
    if !skill.root.is_writable_owned() {
        bail!("refusing to remove skill outside Codewhale-owned roots");
    }
    if skill.source_kind != SkillSourceKind::CodeWhaleManaged {
        bail!("only Codewhale managed skills can be removed");
    }
    let skills_dir = validate_owned_skill_path(ctx, &skill, &path)?;
    let before = verify_expected_digest(&path, expected_digest.as_deref())?;
    let scope = match skill.root.kind {
        SkillRootKind::CodeWhaleProject => SkillScope::Project,
        SkillRootKind::CodeWhaleGlobal => SkillScope::Global,
        _ => SkillScope::Logical,
    };
    let package_name = on_disk_package_name(&skill_id)?;
    validate_owned_skill_path(ctx, &skill, &path)?;
    install::uninstall(package_name, &skills_dir)?;
    Ok(SkillMutationReceipt {
        action: SkillActionKind::Remove,
        name: skill_id.canonical_name,
        scope,
        safe_target_path: safe_display_path(&path, Some(ctx.workspace), ctx.home),
        before_digest: before,
        after_digest: None,
        outcome: SkillMutationOutcome::Removed,
    })
}

fn trust_skill(
    skill_id: AuditedSkillId,
    expected_digest: String,
    ctx: &MutationContext<'_>,
) -> Result<SkillMutationReceipt> {
    let (skill, path) = find_audited_skill(ctx, &skill_id)?;
    if !skill.root.is_writable_owned() {
        bail!("refusing to trust skill outside Codewhale-owned roots");
    }
    if skill.source_kind != SkillSourceKind::CodeWhaleManaged {
        bail!("only Codewhale managed skills can be trusted");
    }
    validate_owned_skill_path(ctx, &skill, &path)?;
    let before = verify_expected_digest(&path, Some(&expected_digest))?;
    validate_owned_skill_path(ctx, &skill, &path)?;
    write_trust_v2(&path, &expected_digest)?;
    let scope = match skill.root.kind {
        SkillRootKind::CodeWhaleProject => SkillScope::Project,
        SkillRootKind::CodeWhaleGlobal => SkillScope::Global,
        _ => SkillScope::Logical,
    };
    Ok(SkillMutationReceipt {
        action: SkillActionKind::Trust,
        name: skill_id.canonical_name,
        scope,
        safe_target_path: safe_display_path(&path, Some(ctx.workspace), ctx.home),
        before_digest: before.clone(),
        after_digest: before,
        outcome: SkillMutationOutcome::Trusted,
    })
}

#[cfg(test)]
#[path = "mutation_onestep_tests.rs"]
mod onestep_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_net::network_policy::NetworkPolicy;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str, body: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n{body}\n"),
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn write_managed_skill(root: &Path, name: &str) -> String {
        write_skill(root, name, "managed body");
        let skill_dir = root.join(name);
        let digest = package_digest::compute_package_digest(&skill_dir).unwrap();
        write_installed_from_v2(
            &skill_dir,
            "github:owner/repo",
            None,
            "source-digest",
            &digest,
            name,
        )
        .unwrap();
        digest
    }

    fn ctx<'a>(
        workspace: &'a Path,
        home: &'a Path,
        network: &'a NetworkPolicy,
    ) -> MutationContext<'a> {
        MutationContext {
            workspace,
            home: Some(home),
            configured_skills_dir: None,
            network,
            max_size: install::DEFAULT_MAX_SIZE_BYTES,
            registry_url: install::DEFAULT_REGISTRY_URL,
        }
    }

    #[test]
    fn resolve_owned_target_project_and_global() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();

        let project =
            resolve_owned_target(&workspace, Some(&home), SkillTargetScope::Project).unwrap();
        let global =
            resolve_owned_target(&workspace, Some(&home), SkillTargetScope::Global).unwrap();
        assert_eq!(project, workspace.join(".codewhale").join("skills"));
        assert_eq!(global, home.join(".codewhale").join("skills"));
    }

    #[cfg(unix)]
    #[test]
    fn owned_target_resolution_rejects_symlinked_parent_and_root_in_both_scopes() {
        for target in [SkillTargetScope::Project, SkillTargetScope::Global] {
            let tmp = TempDir::new().unwrap();
            let workspace = tmp.path().join("ws");
            let home = tmp.path().join("home");
            fs::create_dir_all(&workspace).unwrap();
            fs::create_dir_all(&home).unwrap();
            let anchor = match target {
                SkillTargetScope::Project => &workspace,
                SkillTargetScope::Global => &home,
            };

            let outside_parent = tmp.path().join("outside-parent");
            fs::create_dir_all(&outside_parent).unwrap();
            std::os::unix::fs::symlink(&outside_parent, anchor.join(".codewhale")).unwrap();
            let err = resolve_owned_target(&workspace, Some(&home), target).unwrap_err();
            assert!(err.to_string().contains("symlinked"), "got: {err}");

            fs::remove_file(anchor.join(".codewhale")).unwrap();
            fs::create_dir(anchor.join(".codewhale")).unwrap();
            let outside_root = tmp.path().join("outside-root");
            fs::create_dir_all(&outside_root).unwrap();
            std::os::unix::fs::symlink(&outside_root, anchor.join(".codewhale").join("skills"))
                .unwrap();
            let err = resolve_owned_target(&workspace, Some(&home), target).unwrap_err();
            assert!(err.to_string().contains("symlinked"), "got: {err}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_rejects_symlinked_owned_root_without_external_writes() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(workspace.join(".codewhale")).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("SENTINEL"), "untouched").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join(".codewhale").join("skills")).unwrap();

        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let err = execute(
            SkillMutationRequest::InstallRemote {
                source: InstallSource::GitHubRepo("owner/repo".into()),
                target: SkillTargetScope::Project,
            },
            &c,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("symlinked"), "got: {err}");
        assert_eq!(
            fs::read_to_string(outside.join("SENTINEL")).unwrap(),
            "untouched"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlinked_owned_parent_without_external_writes() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let outside_parent = tmp.path().join("outside-parent");
        let outside_root = outside_parent.join("skills");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside_root).unwrap();
        fs::write(outside_root.join("SENTINEL"), "untouched").unwrap();
        write_skill(
            &workspace.join(".claude").join("skills"),
            "from-claude",
            "import me",
        );
        std::os::unix::fs::symlink(&outside_parent, workspace.join(".codewhale")).unwrap();

        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap
            .skills
            .iter()
            .find(|skill| skill.source_kind == SkillSourceKind::CompatibleExternal)
            .unwrap();
        let DigestState::Known(digest) = &external.digest else {
            panic!("digest");
        };
        let err = execute_sync(
            SkillMutationRequest::ImportExternal {
                source_id: external.id.clone(),
                expected_digest: digest.clone(),
                target: SkillTargetScope::Project,
                conflict_policy: ConflictPolicy::Reject,
            },
            &c,
        )
        .unwrap_err();
        assert!(err.to_string().contains("symlinked"), "got: {err}");
        assert_eq!(
            fs::read_to_string(outside_root.join("SENTINEL")).unwrap(),
            "untouched"
        );
        assert!(!outside_root.join("from-claude").exists());
    }

    #[cfg(unix)]
    #[test]
    fn remove_rejects_symlinked_owned_root_without_external_deletion() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let outside_root = tmp.path().join("outside-root");
        fs::create_dir_all(workspace.join(".codewhale")).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside_root).unwrap();
        let digest = write_managed_skill(&outside_root, "managed");
        std::os::unix::fs::symlink(&outside_root, workspace.join(".codewhale").join("skills"))
            .unwrap();

        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::OwnedOnly,
            None,
        );
        let skill = snap.skills.first().expect("managed skill through symlink");
        let err = execute_sync(
            SkillMutationRequest::Remove {
                skill_id: skill.id.clone(),
                expected_digest: Some(digest),
            },
            &c,
        )
        .unwrap_err();
        assert!(err.to_string().contains("symlinked"), "got: {err}");
        assert!(outside_root.join("managed").join("SKILL.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn trust_rejects_symlinked_owned_parent_without_external_marker_write() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let outside_parent = tmp.path().join("outside-parent");
        let outside_root = outside_parent.join("skills");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside_root).unwrap();
        let digest = write_managed_skill(&outside_root, "managed");
        std::os::unix::fs::symlink(&outside_parent, workspace.join(".codewhale")).unwrap();

        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::OwnedOnly,
            None,
        );
        let skill = snap.skills.first().expect("managed skill through symlink");
        let err = execute_sync(
            SkillMutationRequest::Trust {
                skill_id: skill.id.clone(),
                expected_digest: digest,
            },
            &c,
        )
        .unwrap_err();
        assert!(err.to_string().contains("symlinked"), "got: {err}");
        assert!(
            !outside_root
                .join("managed")
                .join(install::TRUSTED_MARKER)
                .exists()
        );
    }

    #[test]
    fn remove_and_trust_require_managed_owned() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "manual",
            "body",
        );
        let err = resolve_owned_skill_by_name(&c, "manual", Some(SkillTargetScope::Project));
        // Manual skill resolves, but remove/trust should fail source_kind check.
        let resolved = err.unwrap();
        let remove = remove_skill(resolved.id.clone(), Some(resolved.digest.clone()), &c);
        assert!(remove.is_err());

        write_skill(&workspace.join(".claude").join("skills"), "ext", "body");
        // Place sentinel in external root.
        let sentinel = workspace.join(".claude").join("skills").join("SENTINEL");
        fs::write(&sentinel, "do-not-touch").unwrap();

        let err = resolve_owned_skill_by_name(&c, "ext", None);
        assert!(err.unwrap_err().to_string().contains("compatible external"));
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "do-not-touch");
    }

    #[test]
    fn import_external_copies_into_project_owned() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        fs::create_dir_all(workspace.join(".codewhale").join("skills")).unwrap();
        write_skill(
            &workspace.join(".claude").join("skills"),
            "from-claude",
            "import-me",
        );
        let sentinel = workspace.join(".claude").join("skills").join("SENTINEL");
        fs::write(&sentinel, "keep").unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap
            .skills
            .iter()
            .find(|s| s.name == "from-claude")
            .unwrap();
        let DigestState::Known(digest) = &external.digest else {
            panic!("digest");
        };

        let receipt = import_external(
            external.id.clone(),
            digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::Reject,
            &c,
        )
        .unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Imported);
        assert!(
            workspace
                .join(".codewhale")
                .join("skills")
                .join("from-claude")
                .join("SKILL.md")
                .is_file()
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
        // Source untouched.
        assert!(
            workspace
                .join(".claude")
                .join("skills")
                .join("from-claude")
                .join("SKILL.md")
                .is_file()
        );
    }

    #[test]
    fn import_external_keeps_hidden_files_the_digest_covers() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        fs::create_dir_all(workspace.join(".codewhale").join("skills")).unwrap();
        let source_root = workspace.join(".claude").join("skills");
        write_skill(&source_root, "dotted", "import-me");
        fs::write(source_root.join("dotted").join(".DS_Store"), "finder").unwrap();
        fs::create_dir_all(source_root.join("dotted").join(".config")).unwrap();
        fs::write(source_root.join("dotted").join(".config").join("a"), "x").unwrap();
        fs::write(
            source_root.join("dotted").join(".config").join(".trusted"),
            "y",
        )
        .unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap.skills.iter().find(|s| s.name == "dotted").unwrap();
        let DigestState::Known(digest) = &external.digest else {
            panic!("digest");
        };

        let receipt = import_external(
            external.id.clone(),
            digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::Reject,
            &c,
        )
        .unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Imported);
        let dest = workspace.join(".codewhale").join("skills").join("dotted");
        assert_eq!(
            fs::read_to_string(dest.join(".DS_Store")).unwrap(),
            "finder"
        );
        assert_eq!(
            fs::read_to_string(dest.join(".config").join("a")).unwrap(),
            "x"
        );
        assert_eq!(
            fs::read_to_string(dest.join(".config").join(".trusted")).unwrap(),
            "y"
        );
    }

    #[test]
    fn import_exact_duplicate_is_already_present() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        let content = "---\nname: shared\ndescription: d\n---\nbody\n";
        let owned = workspace.join(".codewhale").join("skills").join("shared");
        let external = workspace.join(".claude").join("skills").join("shared");
        fs::create_dir_all(&owned).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(owned.join("SKILL.md"), content).unwrap();
        fs::write(external.join("SKILL.md"), content).unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap
            .skills
            .iter()
            .find(|s| s.name == "shared" && s.source_kind == SkillSourceKind::CompatibleExternal)
            .unwrap();
        let DigestState::Known(digest) = &external.digest else {
            panic!("digest");
        };

        let receipt = import_external(
            external.id.clone(),
            digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::Reject,
            &c,
        )
        .unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::AlreadyPresent);
    }

    #[test]
    fn trust_writes_v2_digest_binding() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "body");
        let digest = package_digest::compute_package_digest(&root.join("managed")).unwrap();
        write_installed_from_v2(
            &root.join("managed"),
            "github:o/r",
            None,
            "src",
            &digest,
            "managed",
        )
        .unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::OwnedOnly,
            None,
        );
        let skill = &snap.skills[0];
        let receipt = trust_skill(skill.id.clone(), digest.clone(), &c).unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Trusted);
        let trust_body =
            fs::read_to_string(root.join("managed").join(install::TRUSTED_MARKER)).unwrap();
        assert!(trust_body.contains("schema_version"));
        assert!(trust_body.contains(&digest));
    }

    #[test]
    fn remove_managed_skill() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "body");
        let digest = package_digest::compute_package_digest(&root.join("managed")).unwrap();
        write_installed_from_v2(
            &root.join("managed"),
            "github:o/r",
            None,
            "src",
            &digest,
            "managed",
        )
        .unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::OwnedOnly,
            None,
        );
        let skill = &snap.skills[0];
        let receipt = remove_skill(skill.id.clone(), Some(digest), &c).unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Removed);
        assert!(!root.join("managed").exists());
    }

    #[test]
    fn resolve_by_name_normalizes_like_registry() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "Hello_World", "body");
        let digest = package_digest::compute_package_digest(&root.join("Hello_World")).unwrap();
        write_installed_from_v2(
            &root.join("Hello_World"),
            "github:o/r",
            None,
            "src",
            &digest,
            "Hello_World",
        )
        .unwrap();

        let resolved =
            resolve_owned_skill_by_name(&c, "Hello_World", Some(SkillTargetScope::Project))
                .unwrap();
        assert_eq!(resolved.id.canonical_name, "hello-world");
        assert_eq!(
            resolve_owned_skill_by_name(&c, "hello-world", Some(SkillTargetScope::Project))
                .unwrap()
                .id,
            resolved.id
        );
    }

    #[test]
    fn remove_uses_on_disk_dir_not_canonical_name() {
        // Frontmatter/dir keep underscores+case; audit canonicalizes to dashes+lower.
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let root = workspace.join(".codewhale").join("skills");
        let dir_name = "Hello_World";
        write_skill(&root, dir_name, "body");
        let digest = package_digest::compute_package_digest(&root.join(dir_name)).unwrap();
        write_installed_from_v2(
            &root.join(dir_name),
            "github:o/r",
            None,
            "src",
            &digest,
            dir_name,
        )
        .unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::OwnedOnly,
            None,
        );
        let skill = snap
            .skills
            .iter()
            .find(|s| s.id.canonical_name == "hello-world")
            .expect("canonical name should fold Hello_World → hello-world");
        assert_ne!(skill.id.canonical_name, dir_name);
        assert_eq!(
            skill.id.relative_dir.file_name().unwrap().to_str().unwrap(),
            dir_name
        );

        let receipt = remove_skill(skill.id.clone(), Some(digest), &c).unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Removed);
        assert!(!root.join(dir_name).exists());
    }

    #[test]
    fn imported_skill_rejects_registry_update() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        fs::create_dir_all(workspace.join(".codewhale").join("skills")).unwrap();
        write_skill(
            &workspace.join(".claude").join("skills"),
            "from-claude",
            "import-me",
        );
        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap
            .skills
            .iter()
            .find(|s| s.name == "from-claude")
            .unwrap();
        let DigestState::Known(digest) = &external.digest else {
            panic!("digest");
        };
        import_external(
            external.id.clone(),
            digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::Reject,
            &c,
        )
        .unwrap();

        let owned = workspace
            .join(".codewhale")
            .join("skills")
            .join("from-claude");
        assert!(ensure_remote_updatable(&owned).is_err());
        assert!(!install::is_registry_updatable_spec(
            "import:<workspace>/.claude/skills/from-claude"
        ));
        assert!(install::is_registry_updatable_spec("github:owner/repo"));
    }

    #[test]
    fn import_rejects_when_owned_dir_differs_from_canonical() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        let owned_root = workspace.join(".codewhale").join("skills");
        write_skill(&owned_root, "Hello_World", "original-owned");
        let original_digest =
            package_digest::compute_package_digest(&owned_root.join("Hello_World")).unwrap();
        write_installed_from_v2(
            &owned_root.join("Hello_World"),
            "github:o/r",
            None,
            "src",
            &original_digest,
            "Hello_World",
        )
        .unwrap();

        write_skill(
            &workspace.join(".claude").join("skills"),
            "Hello_World",
            "external-different",
        );
        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap
            .skills
            .iter()
            .find(|s| {
                s.id.canonical_name == "hello-world"
                    && s.source_kind == SkillSourceKind::CompatibleExternal
            })
            .unwrap();
        let DigestState::Known(ext_digest) = &external.digest else {
            panic!("digest");
        };

        let err = import_external(
            external.id.clone(),
            ext_digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::Reject,
            &c,
        );
        assert!(err.unwrap_err().to_string().contains("already exists"));
        assert!(owned_root.join("Hello_World").exists());
        assert!(!owned_root.join("hello-world").exists());
        let still =
            package_digest::compute_package_digest(&owned_root.join("Hello_World")).unwrap();
        assert_eq!(still, original_digest);

        let receipt = import_external(
            external.id.clone(),
            ext_digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::ReplaceConfirmed,
            &c,
        )
        .unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Imported);
        assert!(owned_root.join("Hello_World").exists());
        assert!(!owned_root.join("hello-world").exists());
        assert!(!owned_root.join("Hello_World.bak").exists());
        let after =
            package_digest::compute_package_digest(&owned_root.join("Hello_World")).unwrap();
        assert_eq!(after, *ext_digest);
    }

    #[test]
    fn import_conflict_reject_keeps_owned_and_replace_cleans_backup() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        let owned_root = workspace.join(".codewhale").join("skills");
        write_skill(&owned_root, "shared", "original-owned");
        let original_digest =
            package_digest::compute_package_digest(&owned_root.join("shared")).unwrap();
        write_installed_from_v2(
            &owned_root.join("shared"),
            "github:o/r",
            None,
            "src",
            &original_digest,
            "shared",
        )
        .unwrap();

        write_skill(
            &workspace.join(".claude").join("skills"),
            "shared",
            "external-different",
        );
        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::Compatible,
            None,
        );
        let external = snap
            .skills
            .iter()
            .find(|s| s.name == "shared" && s.source_kind == SkillSourceKind::CompatibleExternal)
            .unwrap();
        let DigestState::Known(ext_digest) = &external.digest else {
            panic!("digest");
        };

        let err = import_external(
            external.id.clone(),
            ext_digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::Reject,
            &c,
        );
        assert!(err.is_err());
        let still = package_digest::compute_package_digest(&owned_root.join("shared")).unwrap();
        assert_eq!(still, original_digest);

        let receipt = import_external(
            external.id.clone(),
            ext_digest.clone(),
            SkillTargetScope::Project,
            ConflictPolicy::ReplaceConfirmed,
            &c,
        )
        .unwrap();
        assert_eq!(receipt.outcome, SkillMutationOutcome::Imported);
        let after = package_digest::compute_package_digest(&owned_root.join("shared")).unwrap();
        assert_eq!(after, *ext_digest);
        assert!(!owned_root.join("shared.bak").exists());
    }

    #[test]
    fn digest_mismatch_rejects_remove() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);
        let root = workspace.join(".codewhale").join("skills");
        write_skill(&root, "managed", "body");
        let digest = package_digest::compute_package_digest(&root.join("managed")).unwrap();
        write_installed_from_v2(
            &root.join("managed"),
            "github:o/r",
            None,
            "src",
            &digest,
            "managed",
        )
        .unwrap();

        let snap = scan_with_configured(
            &workspace,
            Some(&home),
            None,
            SkillAuditMode::OwnedOnly,
            None,
        );
        let err = remove_skill(snap.skills[0].id.clone(), Some("deadbeef".into()), &c);
        assert!(err.unwrap_err().to_string().contains("changed since audit"));
        assert!(root.join("managed").exists());
    }

    #[test]
    fn dual_scope_same_name_requires_explicit_scope() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        write_skill(
            &workspace.join(".codewhale").join("skills"),
            "dup",
            "project",
        );
        write_skill(&home.join(".codewhale").join("skills"), "dup", "global");

        let err = resolve_owned_skill_by_name(&c, "dup", None).unwrap_err();
        assert!(
            err.to_string().contains("--project") && err.to_string().contains("--global"),
            "got: {err}"
        );

        let project =
            resolve_owned_skill_by_name(&c, "dup", Some(SkillTargetScope::Project)).unwrap();
        let global =
            resolve_owned_skill_by_name(&c, "dup", Some(SkillTargetScope::Global)).unwrap();
        assert_ne!(project.id.root_id, global.id.root_id);
        assert_eq!(project.id.canonical_name, "dup");
        assert_eq!(global.id.canonical_name, "dup");
    }

    #[test]
    fn uninstall_external_only_refuses_and_keeps_sentinel() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let home = tmp.path().join("home");
        let network = NetworkPolicy::default();
        let c = ctx(&workspace, &home, &network);

        fs::create_dir_all(workspace.join(".codewhale").join("skills")).unwrap();
        write_skill(
            &workspace.join(".claude").join("skills"),
            "only-ext",
            "body",
        );
        let sentinel = workspace.join(".claude").join("skills").join("SENTINEL");
        fs::write(&sentinel, "untouched").unwrap();

        let err = resolve_owned_skill_by_name(&c, "only-ext", None).unwrap_err();
        assert!(
            err.to_string().contains("compatible external"),
            "got: {err}"
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched");
        assert!(
            workspace
                .join(".claude")
                .join("skills")
                .join("only-ext")
                .join("SKILL.md")
                .is_file()
        );
    }
}
