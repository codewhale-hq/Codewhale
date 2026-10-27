//! Bounded context-fragment system with hard caps (issue #5264).
//!
//! Every context injection goes through a typed fragment with a
//! `matches_text` recognizer, collected in one `crates/core` module.
//! Hard caps: per-fragment byte cap, 10K-token ceiling, injected-item count.
//! Project-instruction import (#3978, #4079) is a typed fragment.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

// Caps
pub const MAX_FRAGMENT_TOKENS: usize = 10_000;
pub const MAX_FRAGMENT_BYTES: usize = MAX_FRAGMENT_TOKENS * 4; // 40_000
pub const DEFAULT_FRAGMENT_MAX_BYTES: usize = 4 * 1024;
pub const MAX_FRAGMENTS_PER_CONTEXT: usize = 16;
pub const INSTRUCTIONS_FILE_MAX_BYTES: usize = 100 * 1024;
pub const MAX_INSTRUCTION_FILES: usize = 32;

/// Stable fragment identities. Markers are public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FragmentId {
    Workspace,
    Permissions,
    Route,
    AgentTopology,
    SkillsTools,
    TokenBudget,
    ProjectInstructions,
    Constitution,
}

impl FragmentId {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Permissions => "permissions",
            Self::Route => "route",
            Self::AgentTopology => "agent_topology",
            Self::SkillsTools => "skills_tools",
            Self::TokenBudget => "token_budget",
            Self::ProjectInstructions => "project_instructions",
            Self::Constitution => "constitution",
        }
    }
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Workspace => "<!-- cw:ctx:workspace -->",
            Self::Permissions => "<!-- cw:ctx:permissions -->",
            Self::Route => "<!-- cw:ctx:route -->",
            Self::AgentTopology => "<!-- cw:ctx:agent_topology -->",
            Self::SkillsTools => "<!-- cw:ctx:skills_tools -->",
            Self::TokenBudget => "<!-- cw:ctx:token_budget -->",
            Self::ProjectInstructions => "<!-- cw:ctx:project_instructions -->",
            Self::Constitution => "<!-- cw:ctx:constitution -->",
        }
    }
    #[must_use]
    pub fn role(self) -> FragmentRole {
        match self {
            Self::Workspace => FragmentRole::Workspace,
            Self::Permissions => FragmentRole::Permissions,
            Self::Route => FragmentRole::Route,
            Self::AgentTopology => FragmentRole::AgentTopology,
            Self::SkillsTools => FragmentRole::SkillsTools,
            Self::TokenBudget => FragmentRole::TokenBudget,
            Self::ProjectInstructions => FragmentRole::ProjectInstructions,
            Self::Constitution => FragmentRole::Constitution,
        }
    }
    #[must_use]
    pub fn all() -> &'static [FragmentId] {
        &[
            Self::Workspace,
            Self::Permissions,
            Self::Route,
            Self::AgentTopology,
            Self::SkillsTools,
            Self::TokenBudget,
            Self::ProjectInstructions,
            Self::Constitution,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentRole {
    Workspace,
    Permissions,
    Route,
    AgentTopology,
    SkillsTools,
    TokenBudget,
    ProjectInstructions,
    Constitution,
}

impl FragmentRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Permissions => "permissions",
            Self::Route => "route",
            Self::AgentTopology => "agent_topology",
            Self::SkillsTools => "skills_tools",
            Self::TokenBudget => "token_budget",
            Self::ProjectInstructions => "project_instructions",
            Self::Constitution => "constitution",
        }
    }
}

#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Typed fragment trait with `matches_text` recognizer.
pub trait ContextFragment {
    fn fragment_id(&self) -> FragmentId;
    fn marker(&self) -> &'static str;
    fn content(&self) -> &str;
    fn matches_text(&self, haystack: &str) -> bool {
        haystack.contains(self.marker())
    }
    fn tokens_est(&self) -> usize {
        estimate_tokens(self.content())
    }
    fn max_bytes(&self) -> usize;
    fn is_within_token_ceiling(&self) -> bool {
        self.tokens_est() <= MAX_FRAGMENT_TOKENS
    }
    fn is_within_byte_ceiling(&self) -> bool {
        self.content().len() <= MAX_FRAGMENT_BYTES
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedFragment {
    pub id: FragmentId,
    pub role: FragmentRole,
    pub marker: &'static str,
    pub max_bytes: usize,
    pub content: String,
    pub content_hash: u64,
}

impl BoundedFragment {
    #[must_use]
    pub fn new(id: FragmentId, raw: impl Into<String>) -> Self {
        Self::with_max_bytes(id, raw, DEFAULT_FRAGMENT_MAX_BYTES)
    }
    #[must_use]
    pub fn with_max_bytes(id: FragmentId, raw: impl Into<String>, max_bytes: usize) -> Self {
        let clamped_max = max_bytes.min(MAX_FRAGMENT_BYTES);
        let mut content = enforce_byte_cap(raw.into(), clamped_max);
        if estimate_tokens(&content) > MAX_FRAGMENT_TOKENS {
            content = enforce_byte_cap(content, MAX_FRAGMENT_BYTES);
        }
        let content_hash = hash_content(&content);
        Self {
            id,
            role: id.role(),
            marker: id.marker(),
            max_bytes: clamped_max,
            content,
            content_hash,
        }
    }
    #[must_use]
    pub fn project_instructions(raw: impl Into<String>) -> Self {
        Self::with_max_bytes(FragmentId::ProjectInstructions, raw, MAX_FRAGMENT_BYTES)
    }
    #[must_use]
    pub fn constitution(raw: impl Into<String>) -> Self {
        Self::with_max_bytes(FragmentId::Constitution, raw, MAX_FRAGMENT_BYTES)
    }
    #[must_use]
    pub fn render_marked(&self) -> String {
        format!("{}\n{}", self.marker, self.content.trim_end())
    }
}

impl ContextFragment for BoundedFragment {
    fn fragment_id(&self) -> FragmentId {
        self.id
    }
    fn marker(&self) -> &'static str {
        self.marker
    }
    fn content(&self) -> &str {
        &self.content
    }
    fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FragmentCapError {
    #[error("fragment {id:?} exceeds 10K-token ceiling: {tokens} tokens ({bytes} bytes)")]
    TokenCeiling {
        id: FragmentId,
        tokens: usize,
        bytes: usize,
    },
    #[error("fragment {id:?} exceeds byte ceiling: {bytes} > {max} bytes")]
    ByteCeiling {
        id: FragmentId,
        bytes: usize,
        max: usize,
    },
    #[error("context has too many fragments: {count} > {max}")]
    TooManyFragments { count: usize, max: usize },
}

pub fn validate_fragment(fragment: &BoundedFragment) -> Result<(), FragmentCapError> {
    if fragment.content.len() > MAX_FRAGMENT_BYTES {
        return Err(FragmentCapError::ByteCeiling {
            id: fragment.id,
            bytes: fragment.content.len(),
            max: MAX_FRAGMENT_BYTES,
        });
    }
    let tokens = estimate_tokens(&fragment.content);
    if tokens > MAX_FRAGMENT_TOKENS {
        return Err(FragmentCapError::TokenCeiling {
            id: fragment.id,
            bytes: fragment.content.len(),
            tokens,
        });
    }
    Ok(())
}

pub fn validate_fragment_set(fragments: &[BoundedFragment]) -> Result<(), FragmentCapError> {
    if fragments.len() > MAX_FRAGMENTS_PER_CONTEXT {
        return Err(FragmentCapError::TooManyFragments {
            count: fragments.len(),
            max: MAX_FRAGMENTS_PER_CONTEXT,
        });
    }
    for f in fragments {
        validate_fragment(f)?;
    }
    Ok(())
}

// Project-instruction import (#3978)
pub const PROJECT_INSTRUCTION_CANDIDATES: &[&str] = &[
    "AGENTS.md",
    ".agents/AGENTS.md",
    "CLAUDE.md",
    ".claude/instructions.md",
    ".codewhale/instructions.md",
    ".deepseek/instructions.md",
    ".cursorrules",
    ".cursor/rules",
    ".clinerules",
    ".windsurf/rules",
    ".gemini",
    ".github/copilot-instructions.md",
    ".github/muse-instructions.md",
];

/// Workspace instruction formats not already owned by Codewhale's canonical
/// project-context loader. The TUI uses this subset to avoid injecting
/// `AGENTS.md` / `CLAUDE.md` / `instructions.md` twice while still importing
/// additional agent rule formats through the typed fragment boundary.
pub const ADDITIONAL_PROJECT_INSTRUCTION_CANDIDATES: &[&str] = &[
    ".agents/AGENTS.md",
    ".cursorrules",
    ".cursor/rules",
    ".clinerules",
    ".windsurf/rules",
    ".gemini",
    ".github/copilot-instructions.md",
    ".github/muse-instructions.md",
];

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}
pub fn read_project_instruction_file(file: std::fs::File) -> Option<String> {
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    use std::io::Read as _;
    let mut buf = Vec::new();
    file.take(INSTRUCTIONS_FILE_MAX_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    let observed_len = buf.len() as u64;
    if meta.len().max(observed_len) > INSTRUCTIONS_FILE_MAX_BYTES as u64 {
        buf.truncate(INSTRUCTIONS_FILE_MAX_BYTES);
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        let mut end = INSTRUCTIONS_FILE_MAX_BYTES.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        let omitted = meta
            .len()
            .max(observed_len)
            .saturating_sub(INSTRUCTIONS_FILE_MAX_BYTES as u64);
        text.push_str(&format!("\n[…truncated: {omitted} bytes omitted]"));
        return Some(text);
    }
    let raw = String::from_utf8(buf).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
fn collect_candidate_files(workspace: &Path, candidates: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for candidate in candidates {
        let path = workspace.join(candidate);
        // `is_dir()` follows symlinks, so a symlinked `.cursor/rules` pointing
        // outside the workspace used to be traversed: every file behind it is a
        // real file, so the per-entry symlink checks below all passed and the
        // escape succeeded. `project_context.rs` already refuses symlinked
        // rules directories for exactly this reason; the two loaders now agree.
        if path.is_dir() && !is_symlink(&path) {
            let mut dir_files = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_file() && p.extension().is_some_and(|e| e == "md") && !is_symlink(&p) {
                        dir_files.push(p);
                    }
                }
            }
            if let Ok(entries) = std::fs::read_dir(&path) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir()
                        && !is_symlink(&p)
                        && let Ok(sub) = std::fs::read_dir(&p)
                    {
                        for se in sub.flatten() {
                            let sp = se.path();
                            if sp.is_file()
                                && sp.extension().is_some_and(|e| e == "md")
                                && !is_symlink(&sp)
                            {
                                dir_files.push(sp);
                            }
                        }
                    }
                }
            }
            dir_files.sort();
            let remaining = MAX_INSTRUCTION_FILES.saturating_sub(files.len());
            dir_files.truncate(remaining);
            files.extend(dir_files);
        } else if path.is_file() && !is_symlink(&path) {
            files.push(path);
        }
        if files.len() >= MAX_INSTRUCTION_FILES {
            break;
        }
    }
    files.truncate(MAX_INSTRUCTION_FILES);
    files.sort();
    files.dedup();
    files
}

/// Enumerate the safe, bounded file paths selected for a caller-provided set
/// of project-instruction candidates.
///
/// This is the same traversal used by
/// [`load_selected_project_instruction_fragment`]. Callers that cache the
/// loader's result can fingerprint these paths without maintaining a second,
/// subtly different directory walk. Content validation still happens in the
/// loader: an empty, unreadable, or non-UTF-8 file may be selected here and
/// then contribute no fragment.
#[must_use]
pub fn selected_project_instruction_candidate_files(
    workspace: &Path,
    candidates: &[&str],
) -> Vec<PathBuf> {
    collect_candidate_files(workspace, candidates)
}

fn load_project_instruction_fragment_from_candidates(
    workspace: &Path,
    candidates: &[&str],
    mut opener: impl FnMut(&Path) -> std::io::Result<std::fs::File>,
) -> Option<BoundedFragment> {
    let files = collect_candidate_files(workspace, candidates);
    if files.is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    for path in files {
        if let Some(content) = opener(&path).ok().and_then(read_project_instruction_file) {
            let rel = path
                .strip_prefix(workspace)
                .unwrap_or(&path)
                .display()
                .to_string();
            sections.push(format!(
                "<project_instructions source=\"{rel}\">\n{content}\n</project_instructions>"
            ));
        }
    }
    if sections.is_empty() {
        return None;
    }
    let merged = sections.join("\n\n");
    let fragment = BoundedFragment::project_instructions(merged);
    debug_assert!(validate_fragment(&fragment).is_ok());
    Some(fragment)
}

pub fn load_project_instruction_fragment(workspace: &Path) -> Option<BoundedFragment> {
    load_project_instruction_fragment_from_candidates(
        workspace,
        PROJECT_INSTRUCTION_CANDIDATES,
        |path| std::fs::File::open(path),
    )
}

/// Load only instruction formats that the canonical TUI project-context path
/// does not already render. This prevents duplicate authority while retaining
/// the broader compatibility import added by the bounded fragment system.
///
/// Prefer [`load_selected_project_instruction_fragment`]: importing every
/// foreign format unconditionally makes another tool's instruction file
/// standing authority here without anyone asking for it.
pub fn load_additional_project_instruction_fragment(workspace: &Path) -> Option<BoundedFragment> {
    load_project_instruction_fragment_from_candidates(
        workspace,
        ADDITIONAL_PROJECT_INSTRUCTION_CANDIDATES,
        |path| std::fs::File::open(path),
    )
}

/// Load exactly the instruction candidates the caller names.
///
/// The caller owns the opt-in decision; an empty list imports nothing and
/// yields `None` rather than silently falling back to the full candidate set.
pub fn load_selected_project_instruction_fragment(
    workspace: &Path,
    candidates: &[&str],
) -> Option<BoundedFragment> {
    load_selected_project_instruction_fragment_with_reader(workspace, candidates, |path| {
        std::fs::File::open(path)
    })
}

pub fn load_selected_project_instruction_fragment_with_reader(
    workspace: &Path,
    candidates: &[&str],
    opener: impl FnMut(&Path) -> std::io::Result<std::fs::File>,
) -> Option<BoundedFragment> {
    if candidates.is_empty() {
        return None;
    }
    load_project_instruction_fragment_from_candidates(workspace, candidates, opener)
}

pub fn project_instructions_from_sources(
    sources: impl IntoIterator<Item = (String, String)>,
) -> Option<BoundedFragment> {
    let mut sections = Vec::new();
    for (name, content) in sources {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let body = if trimmed.len() > INSTRUCTIONS_FILE_MAX_BYTES {
            let mut end = INSTRUCTIONS_FILE_MAX_BYTES;
            while end > 0 && !trimmed.is_char_boundary(end) {
                end -= 1;
            }
            let omitted = trimmed.len() - end;
            format!("{}\n[…truncated: {omitted} bytes omitted]", &trimmed[..end])
        } else {
            trimmed.to_string()
        };
        sections.push(format!(
            "<project_instructions source=\"{name}\">\n{body}\n</project_instructions>"
        ));
        if sections.len() >= MAX_INSTRUCTION_FILES {
            break;
        }
    }
    if sections.is_empty() {
        return None;
    }
    Some(BoundedFragment::project_instructions(sections.join("\n\n")))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FragmentBudgetSnapshot {
    pub fragment_ids: Vec<String>,
    pub fragment_markers: Vec<String>,
    pub max_fragment_bytes: usize,
    pub max_fragment_tokens: usize,
    pub default_fragment_max_bytes: usize,
    pub max_fragments_per_context: usize,
    pub instructions_file_max_bytes: usize,
    pub max_instruction_files: usize,
    pub project_instruction_candidates: Vec<String>,
}

#[must_use]
pub fn fragment_budget_snapshot() -> FragmentBudgetSnapshot {
    FragmentBudgetSnapshot {
        fragment_ids: FragmentId::all()
            .iter()
            .map(|id| id.as_str().to_string())
            .collect(),
        fragment_markers: FragmentId::all()
            .iter()
            .map(|id| id.marker().to_string())
            .collect(),
        max_fragment_bytes: MAX_FRAGMENT_BYTES,
        max_fragment_tokens: MAX_FRAGMENT_TOKENS,
        default_fragment_max_bytes: DEFAULT_FRAGMENT_MAX_BYTES,
        max_fragments_per_context: MAX_FRAGMENTS_PER_CONTEXT,
        instructions_file_max_bytes: INSTRUCTIONS_FILE_MAX_BYTES,
        max_instruction_files: MAX_INSTRUCTION_FILES,
        project_instruction_candidates: PROJECT_INSTRUCTION_CANDIDATES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

fn hash_content(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}
fn enforce_byte_cap(raw: String, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    if raw.len() <= max_bytes {
        return raw;
    }
    let omitted = raw.len().saturating_sub(max_bytes);
    let marker = format!("\n[…truncated: {omitted} bytes omitted]");
    if marker.len() >= max_bytes {
        return marker.chars().take(max_bytes).collect();
    }
    let keep = max_bytes.saturating_sub(marker.len());
    let mut end = keep;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = raw[..end].to_string();
    out.push_str(&marker);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn fragment_has_matches_text_recognizer() {
        let fragment = BoundedFragment::new(FragmentId::Workspace, "repo: /tmp/demo");
        let rendered = fragment.render_marked();
        assert!(fragment.matches_text(&rendered));
        assert!(!fragment.matches_text("no marker here"));
        assert_eq!(FragmentId::Workspace.marker(), "<!-- cw:ctx:workspace -->");
        assert_eq!(
            FragmentId::ProjectInstructions.marker(),
            "<!-- cw:ctx:project_instructions -->"
        );
        assert_eq!(
            FragmentId::Constitution.marker(),
            "<!-- cw:ctx:constitution -->"
        );
    }
    #[test]
    fn all_fragment_types_go_through_bounded_module() {
        for id in FragmentId::all() {
            let fragment = BoundedFragment::new(*id, "hello");
            assert_eq!(fragment.marker, id.marker());
            assert_eq!(fragment.id, *id);
            validate_fragment(&fragment).expect("small fragment must pass caps");
            assert!(fragment.is_within_token_ceiling());
            assert!(fragment.is_within_byte_ceiling());
        }
    }
    #[test]
    fn per_fragment_byte_cap_truncates_with_marker() {
        let oversized = "x".repeat(DEFAULT_FRAGMENT_MAX_BYTES + 64);
        let fragment = BoundedFragment::new(FragmentId::AgentTopology, oversized);
        assert!(fragment.content.len() <= DEFAULT_FRAGMENT_MAX_BYTES);
        assert!(fragment.content.contains("[…truncated:"));
        validate_fragment(&fragment).expect("truncated fragment must pass caps");
    }
    #[test]
    fn ten_k_token_ceiling_is_enforced() {
        let huge = "a".repeat(MAX_FRAGMENT_BYTES + 1_000);
        let fragment = BoundedFragment::project_instructions(huge);
        assert!(fragment.content.len() <= MAX_FRAGMENT_BYTES);
        assert!(estimate_tokens(&fragment.content) <= MAX_FRAGMENT_TOKENS);
        validate_fragment(&fragment).expect("capped fragment must satisfy token ceiling");
        let also_huge = "b".repeat(MAX_FRAGMENT_BYTES + 5000);
        let fragment = BoundedFragment::with_max_bytes(FragmentId::Workspace, also_huge, 100_000);
        assert!(fragment.max_bytes <= MAX_FRAGMENT_BYTES);
        assert!(fragment.content.len() <= MAX_FRAGMENT_BYTES);
        assert!(fragment.is_within_token_ceiling());
    }
    #[test]
    fn injected_item_count_cap_is_enforced() {
        let fragments: Vec<BoundedFragment> = (0..MAX_FRAGMENTS_PER_CONTEXT)
            .map(|i| BoundedFragment::new(FragmentId::Workspace, format!("item {i}")))
            .collect();
        validate_fragment_set(&fragments).expect("exactly MAX_FRAGMENTS must pass");
        let mut too_many = fragments.clone();
        too_many.push(BoundedFragment::new(FragmentId::Route, "one too many"));
        let err = validate_fragment_set(&too_many).expect_err("one over cap must fail");
        assert!(matches!(err, FragmentCapError::TooManyFragments { .. }));
    }
    #[test]
    #[cfg(unix)]
    fn symlinked_candidate_directory_cannot_escape_the_workspace() {
        // `Path::is_dir()` follows symlinks, so a symlinked candidate dir used
        // to be traversed: every file behind it is a real file, so the
        // per-entry symlink checks passed and content outside the workspace was
        // imported as project instruction authority.
        let outside = tempdir().expect("outside");
        fs::write(outside.path().join("leaked.md"), "SECRET-OUTSIDE-WORKSPACE")
            .expect("write outside");

        let dir = tempdir().expect("workspace");
        let ws = dir.path();
        fs::create_dir_all(ws.join(".cursor")).expect("mkdir .cursor");
        std::os::unix::fs::symlink(outside.path(), ws.join(".cursor").join("rules"))
            .expect("symlink rules dir");

        let fragment = load_selected_project_instruction_fragment(ws, &[".cursor/rules"]);
        assert!(
            fragment.is_none(),
            "a symlinked candidate directory must not be traversed: {fragment:?}"
        );
    }

    #[test]
    fn selected_candidates_import_nothing_when_the_list_is_empty() {
        let dir = tempdir().expect("tempdir");
        let ws = dir.path();
        fs::write(ws.join(".cursorrules"), "cursor: always use tabs").expect("write cursor");
        assert!(
            load_selected_project_instruction_fragment(ws, &[]).is_none(),
            "an empty opt-in list must import nothing, not fall back to every format"
        );
        assert!(
            load_selected_project_instruction_fragment(ws, &[".cursorrules"]).is_some(),
            "an explicitly named candidate is still imported"
        );
    }

    #[test]
    fn project_instruction_import_is_a_typed_fragment() {
        let dir = tempdir().expect("tempdir");
        let ws = dir.path();
        fs::write(ws.join(".cursorrules"), "cursor: always use tabs").expect("write cursor");
        fs::write(ws.join(".clinerules"), "cline: prefer functional style").expect("write cline");
        fs::create_dir_all(ws.join(".windsurf").join("rules")).expect("mkdir windsurf");
        fs::write(
            ws.join(".windsurf").join("rules").join("extra.md"),
            "# windsurf extra",
        )
        .expect("write windsurf");
        fs::create_dir_all(ws.join(".github")).expect("mkdir github");
        fs::write(
            ws.join(".github").join("copilot-instructions.md"),
            "# copilot says hello",
        )
        .expect("write copilot");
        let fragment =
            load_project_instruction_fragment(ws).expect("must find imported instructions");
        assert_eq!(fragment.id, FragmentId::ProjectInstructions);
        assert!(fragment.matches_text(&fragment.render_marked()));
        assert!(
            fragment.content.contains(".cursorrules") || fragment.content.contains(".clinerules")
        );
        validate_fragment(&fragment).expect("project-instructions fragment must satisfy caps");
        let from_sources = project_instructions_from_sources(vec![
            ("AGENTS.md".to_string(), "# AGENTS\nbe helpful".to_string()),
            (
                ".cursorrules".to_string(),
                "cursor: do the thing".to_string(),
            ),
        ])
        .expect("sources");
        assert_eq!(from_sources.id, FragmentId::ProjectInstructions);
        assert!(from_sources.content.contains("AGENTS.md"));
        assert!(from_sources.content.contains(".cursorrules"));
        validate_fragment(&from_sources).expect("explicit sources must also satisfy caps");
    }
    #[test]
    fn additional_project_instruction_import_does_not_duplicate_canonical_authority() {
        let dir = tempdir().expect("tempdir");
        let ws = dir.path();
        fs::write(ws.join("AGENTS.md"), "canonical authority marker").expect("write agents");

        assert!(
            load_additional_project_instruction_fragment(ws).is_none(),
            "AGENTS.md is already owned by the canonical project-context loader"
        );

        fs::write(ws.join(".cursorrules"), "additional cursor marker").expect("write cursor rules");
        let additional = load_additional_project_instruction_fragment(ws)
            .expect("additional rules must produce a typed fragment");
        assert!(additional.content.contains("additional cursor marker"));
        assert!(!additional.content.contains("canonical authority marker"));

        let complete = load_project_instruction_fragment(ws)
            .expect("complete importer must retain every supported source");
        assert!(complete.content.contains("canonical authority marker"));
        assert!(complete.content.contains("additional cursor marker"));
    }
    #[test]
    fn fragment_budget_snapshot_is_stable() {
        let snap = fragment_budget_snapshot();
        assert_eq!(snap.max_fragment_tokens, 10_000);
        assert_eq!(snap.max_fragment_bytes, 40_000);
        assert_eq!(snap.max_fragments_per_context, 16);
        assert_eq!(snap.default_fragment_max_bytes, 4 * 1024);
        assert!(
            snap.fragment_ids
                .contains(&"project_instructions".to_string())
        );
        assert!(snap.fragment_ids.contains(&"constitution".to_string()));
        assert!(
            snap.project_instruction_candidates
                .contains(&".cursorrules".to_string())
        );
        assert!(
            snap.project_instruction_candidates
                .contains(&".github/copilot-instructions.md".to_string())
        );
        assert!(
            snap.fragment_markers
                .contains(&"<!-- cw:ctx:project_instructions -->".to_string())
        );
    }
}
