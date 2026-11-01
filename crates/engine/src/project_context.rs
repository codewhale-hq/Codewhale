//! Project context loading for Codewhale.
//!
//! This module handles loading project-specific context files that provide
//! instructions and context to the AI agent. These include:
//!
//! - `AGENTS.md` - Cross-agent project instructions (canonical, highest priority)
//! - `.claude/instructions.md` - Claude-style hidden instructions (compat)
//! - `CLAUDE.md` - Claude-style instructions (compat)
//! - `.codewhale/instructions.md` - Hidden instructions file (compat)
//! - `.deepseek/instructions.md` - Hidden instructions file (legacy)
//!
//! Codewhale-specific repo authority/prioritization policy lives separately in
//! `.codewhale/constitution.json` and is rendered as its own higher-authority
//! block. The loaded content is injected into the system prompt to give the
//! agent context about the project's conventions, structure, and requirements.

mod budget;
mod constitution;
#[cfg(test)]
mod instruction_budget_tests;
mod pack;
mod types;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub use self::budget::InstructionAssembly;
pub use self::budget::{
    InstructionAction, InstructionBudgetConfig, InstructionBudgetPolicy, InstructionBudgets,
    InstructionDetail, InstructionLimit, InstructionNotice, InstructionOrigin, InstructionRefusal,
    InstructionScope, InstructionSegment, apply_instruction_budgets, estimate_instruction_tokens,
    render_notice_body,
};
pub use self::constitution::load_repo_constitution_block;
use self::constitution::repo_constitution_candidate_paths;
pub use self::constitution::{RepoLawAction, RepoLawRule, load_repo_law_rules};
use self::pack::generate_bounded_project_overview;
pub use self::pack::generate_project_context_pack;
pub use self::types::ProjectContext;
use self::types::ProjectContextError;
pub use self::types::project_instructions_source_label;
use self::types::repo_relative_source_label;
use codewhale_config::instruction_budget::DEFAULT_FILE_MAX_BYTES;

/// Names of project context files to look for, in priority order.
///
/// `AGENTS.md` is the canonical cross-agent project-instructions file.
/// `WHALE.md` is no longer an active context surface; when present, Codewhale
/// reports a migration warning but ignores it. Codewhale-specific repo
/// authority now lives in `.codewhale/constitution.json`, not a bespoke
/// markdown file. `CLAUDE.md` and the `*/instructions.md` variants are
/// read-only compatibility fallbacks; Codewhale never creates or recommends
/// them.
const PROJECT_CONTEXT_FILES: &[&str] = &[
    "AGENTS.md",
    ".claude/instructions.md",
    "CLAUDE.md",
    ".codewhale/instructions.md",
    ".deepseek/instructions.md",
];

/// A foreign agent's instruction format. These are read only when the user
/// opts in by name, because a file written as law for a different tool is not
/// automatically law for this one — and because silently treating a file the
/// user never pointed at us as standing authority is an injection surface,
/// not a convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ForeignInstructionFormat {
    Claude,
    Cursor,
    Cline,
    Windsurf,
    Gemini,
    Copilot,
    Muse,
}

impl ForeignInstructionFormat {
    pub const ALL: &'static [Self] = &[
        Self::Claude,
        Self::Cursor,
        Self::Cline,
        Self::Windsurf,
        Self::Gemini,
        Self::Copilot,
        Self::Muse,
    ];

    /// Configuration spelling, as written in `project_instruction_imports`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Cline => "cline",
            Self::Windsurf => "windsurf",
            Self::Gemini => "gemini",
            Self::Copilot => "copilot",
            Self::Muse => "muse",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        Self::ALL.iter().copied().find(|f| f.key() == value)
    }

    /// Workspace-relative instruction files this format contributes.
    fn context_files(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &[".claude/instructions.md", "CLAUDE.md"],
            // The remaining formats are imported through the bounded-fragment
            // loader in `codewhale_core::fragments`, not through this chain.
            _ => &[],
        }
    }

    /// Workspace-relative rules directories this format contributes.
    fn rules_dirs(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &[".claude/rules"],
            _ => &[],
        }
    }

    /// Candidates handled by the bounded-fragment loader in
    /// `codewhale_core::fragments` rather than by the instruction chain.
    fn fragment_candidates(self) -> &'static [&'static str] {
        match self {
            Self::Cursor => &[".cursorrules", ".cursor/rules"],
            Self::Cline => &[".clinerules"],
            Self::Windsurf => &[".windsurf/rules"],
            Self::Gemini => &[".gemini"],
            Self::Copilot => &[".github/copilot-instructions.md"],
            Self::Muse => &[".github/muse-instructions.md"],
            // Claude's files are read by the instruction chain above, so
            // importing them here too would inject the same bytes twice.
            Self::Claude => &[],
        }
    }
}

/// The set of foreign formats the user has explicitly opted into.
///
/// Resolved from config once and read by the loader. Empty by default: a fresh
/// checkout containing a `CLAUDE.md` written for another tool contributes
/// nothing to Codewhale's standing instructions until someone says so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignInstructionImports {
    enabled: std::collections::BTreeSet<&'static str>,
}

impl ForeignInstructionImports {
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Parse configured names, returning the set plus any unrecognized names
    /// so the caller can warn instead of silently ignoring a typo.
    #[must_use]
    pub fn from_config(values: &[String]) -> (Self, Vec<String>) {
        let mut enabled = std::collections::BTreeSet::new();
        let mut unknown = Vec::new();
        for value in values {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.eq_ignore_ascii_case("all") {
                enabled.extend(ForeignInstructionFormat::ALL.iter().map(|f| f.key()));
                continue;
            }
            match ForeignInstructionFormat::parse(trimmed) {
                Some(format) => {
                    enabled.insert(format.key());
                }
                None => unknown.push(trimmed.to_string()),
            }
        }
        (Self { enabled }, unknown)
    }

    #[must_use]
    pub fn is_enabled(&self, format: ForeignInstructionFormat) -> bool {
        self.enabled.contains(format.key())
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }

    /// Enabled format keys, for provenance and diagnostics.
    #[must_use]
    pub fn keys(&self) -> Vec<&'static str> {
        self.enabled.iter().copied().collect()
    }
}

#[must_use]
pub fn fragment_candidates_for(imports: &ForeignInstructionImports) -> Vec<&'static str> {
    let mut candidates: Vec<&'static str> = vec![".agents/AGENTS.md"];
    for format in ForeignInstructionFormat::ALL {
        if imports.is_enabled(*format) {
            candidates.extend(format.fragment_candidates());
        }
    }
    candidates
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSettings {
    pub budget: InstructionBudgetConfig,
    pub imports: ForeignInstructionImports,
    pub home: Option<PathBuf>,
}

impl Default for InstructionSettings {
    fn default() -> Self {
        Self {
            budget: InstructionBudgetConfig::default(),
            imports: ForeignInstructionImports::default(),
            home: codewhale_paths::user_home(),
        }
    }
}

impl InstructionSettings {
    pub fn from_config(config: &codewhale_runtime::config::Config) -> Self {
        Self {
            budget: config.instruction_budget.clone().unwrap_or_default(),
            imports: ForeignInstructionImports::from_config(&config.project_instruction_imports).0,
            home: codewhale_paths::user_home(),
        }
    }

    fn file_max_bytes(&self) -> u64 {
        self.budget
            .file_max_bytes
            .unwrap_or(DEFAULT_FILE_MAX_BYTES)
            .min(DEFAULT_FILE_MAX_BYTES)
    }
    fn claude_fallback(&self) -> bool {
        self.budget.claude_fallback.unwrap_or(false)
    }
}

/// Instruction file candidates in priority order for the active opt-in set.
fn context_files_for(imports: &ForeignInstructionImports) -> Vec<&'static str> {
    // Order is priority: `load_dir_instructions` takes the first usable match
    // in a directory. AGENTS.md leads, then Codewhale's own files, and only
    // then anything imported from another tool — an imported CLAUDE.md must
    // never outrank .codewhale/instructions.md the way the old flat list let
    // it. Only an enabled import adds a foreign candidate here; the source
    // listing adds the opt-in `claude_fallback` candidates itself.
    let mut files: Vec<&'static str> = vec![
        "AGENTS.md",
        ".codewhale/instructions.md",
        ".deepseek/instructions.md",
    ];
    for format in ForeignInstructionFormat::ALL {
        if imports.is_enabled(*format) {
            files.extend(format.context_files());
        }
    }
    files
}

/// Rules directories for the active opt-in set.
fn rules_dirs_for(imports: &ForeignInstructionImports) -> Vec<&'static str> {
    let mut dirs: Vec<&'static str> = vec![".codewhale/rules"];
    for format in ForeignInstructionFormat::ALL {
        if imports.is_enabled(*format) {
            dirs.extend(format.rules_dirs());
        }
    }
    dirs
}

fn rules_dir_has_loadable_content(workspace: &Path, rules_dir_name: &str) -> bool {
    let rules_dir = workspace.join(rules_dir_name);
    if fs::symlink_metadata(&rules_dir).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return false;
    }
    let Ok(entries) = fs::read_dir(rules_dir) else {
        return false;
    };
    let mut candidates = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "md")
                && context_candidate_exists(path)
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .into_iter()
        .take(MAX_RULES_FILES)
        .any(|path| load_context_file(workspace, &path).is_ok())
}

/// Foreign instruction files that exist in the workspace but were not
/// imported, so the user can discover the opt-in instead of wondering why
/// their `CLAUDE.md` is being ignored.
fn unimported_foreign_warnings(
    workspace: &Path,
    imports: &ForeignInstructionImports,
) -> Vec<String> {
    let mut seen: Vec<&'static str> = Vec::new();
    for format in ForeignInstructionFormat::ALL {
        if imports.is_enabled(*format) {
            continue;
        }
        let direct_context_present = format
            .context_files()
            .iter()
            .any(|relative| load_context_file(workspace, &workspace.join(relative)).is_ok());
        let rules_present = format
            .rules_dirs()
            .iter()
            .any(|relative| rules_dir_has_loadable_content(workspace, relative));
        let fragment_present =
            codewhale_core::fragments::load_selected_project_instruction_fragment_with_reader(
                workspace,
                format.fragment_candidates(),
                |path| codewhale_util::fs_confined::open_read(workspace, path),
            )
            .is_some();
        // Keep discovery aligned with the loaders. A path can exist without
        // being importable (for example an empty or unreadable file, a rules
        // directory containing no Markdown, an oversized file, or a symlink
        // that the loaders deliberately refuse to follow). Warning for those
        // paths would claim there is content available to import when there is
        // not.
        let present = direct_context_present || rules_present || fragment_present;
        if present {
            seen.push(format.key());
        }
    }
    if seen.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "Found instruction files for {} in this workspace; they are not loaded.          Codewhale reads AGENTS.md and its own instruction files by default.          To import them, set project_instruction_imports = [{}] in config.",
        seen.join(", "),
        seen.iter()
            .map(|key| format!("\"{key}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )]
}

/// Rules directories auto-discovered at workspace level, in priority order.
/// `.codewhale/rules/` is Codewhale-native; `.claude/rules/` is Claude compatibility.
/// All `.md` files in these directories are loaded as project rules in filename order.
/// Security model: same trust class as AGENTS.md — workspace-contained content only,
/// no absolute-path escape. Does not require #417 project-config relaxation.
const RULES_DIRS: &[&str] = &[".codewhale/rules", ".claude/rules"];

/// File name of the deprecated Codewhale-native instructions file.
const DEPRECATED_WHALE_FILENAME: &str = "WHALE.md";

/// Warning surfaced when an ignored `WHALE.md` is present.
const WHALE_IGNORED_WARNING: &str = "WHALE.md is ignored; move project instructions to AGENTS.md, or Codewhale-specific authority policy to .codewhale/constitution.json.";

/// User-level project instructions loaded as a fallback when the workspace and
/// its parents do not define project context. Any global AGENTS.md takes
/// priority over a global instructions.md (#3012). Within each file name,
/// `.codewhale/` takes priority over vendor-neutral `.agents/`, which takes
/// priority over legacy `.deepseek/`. Global `WHALE.md` files are ignored and
/// reported as migration-only diagnostics.
const GLOBAL_AGENTS_RELATIVE_PATH: &[&str] = &[".codewhale", "AGENTS.md"];
const GLOBAL_AGENTS_VENDOR_NEUTRAL_PATH: &[&str] = &[".agents", "AGENTS.md"];
const GLOBAL_AGENTS_LEGACY_PATH: &[&str] = &[".deepseek", "AGENTS.md"];
const GLOBAL_WHALE_RELATIVE_PATH: &[&str] = &[".codewhale", "WHALE.md"];
const GLOBAL_WHALE_VENDOR_NEUTRAL_PATH: &[&str] = &[".agents", "WHALE.md"];
const GLOBAL_WHALE_LEGACY_PATH: &[&str] = &[".deepseek", "WHALE.md"];
/// Global `instructions.md` (#3012): auto-loaded as a fallback context layer,
/// ranked below AGENTS.md, mirroring the project-level precedence.
const GLOBAL_INSTRUCTIONS_RELATIVE_PATH: &[&str] = &[".codewhale", "instructions.md"];
const GLOBAL_INSTRUCTIONS_VENDOR_NEUTRAL_PATH: &[&str] = &[".agents", "instructions.md"];
const GLOBAL_INSTRUCTIONS_LEGACY_PATH: &[&str] = &[".deepseek", "instructions.md"];

/// Maximum number of rule files loaded per rules directory.
///
/// A safety cap, not a budget: the files it drops are reported as
/// `skipped (rules_cap)` instead of disappearing into a log line.
const MAX_RULES_FILES: usize = 50;

/// Claude-format instruction files, most authoritative first. `CLAUDE.md` is
/// read as a same-directory alternative to `.claude/CLAUDE.md`; `CLAUDE.local.md`
/// is Claude-specific and is deliberately not imported.
const CLAUDE_CONTEXT_FILES: &[&str] = &["CLAUDE.md", ".claude/CLAUDE.md"];

/// Render the raw segments into the single instruction string the prompt
/// carries: global scope first (so the workspace keeps the last word), then
/// the repository-root → workspace chain.
///
/// Labels use repository-relative project paths, home-relative global paths,
/// and configured file names so host roots stay outside the pinned prefix.
#[must_use]
pub fn render_joined_instructions(segments: &[InstructionSegment]) -> Option<String> {
    let global = render_instruction_scope(segments, InstructionScope::Global);
    let project = render_instruction_scope(segments, InstructionScope::Project);
    match (global, project) {
        (Some(global_text), Some(project_text)) => {
            let global_label = segments
                .iter()
                .find(|segment| segment.scope == InstructionScope::Global)
                .and_then(|segment| segment.label.as_deref())
                .map(|label| format!("<!-- global: {label} -->"))
                .unwrap_or_else(|| "<!-- global -->".to_string());
            Some(format!(
                "{global_label}\n{}\n\n<!-- project (overrides global where they conflict) -->\n{}",
                global_text.trim_end(),
                project_text.trim_start(),
            ))
        }
        (Some(text), None) | (None, Some(text)) => Some(text),
        (None, None) => None,
    }
}

fn render_instruction_scope(
    segments: &[InstructionSegment],
    scope: InstructionScope,
) -> Option<String> {
    let mut out = String::new();
    for segment in segments.iter().filter(|segment| segment.scope == scope) {
        let label = segment
            .label
            .clone()
            .or_else(|| segment.path.as_ref().map(|path| path.display().to_string()))
            .unwrap_or_else(|| scope.as_str().to_string());
        match segment.origin {
            InstructionOrigin::Configured => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&format!(
                    "<instructions source=\"{label}\">\n{}\n</instructions>",
                    segment.text.trim()
                ));
            }
            _ if scope == InstructionScope::Global => {
                if !out.is_empty() {
                    out.push_str(&format!("\n\n<!-- global instructions: {label} -->\n"));
                }
                out.push_str(&segment.text);
            }
            _ => {
                if !out.is_empty() {
                    out.push_str(&format!(
                        "\n\n<!-- scoped instructions: {label} (overrides wider scopes where they conflict) -->\n"
                    ));
                }
                out.push_str(&segment.text);
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Assemble both scopes for one request: read the configured `instructions`
/// entries fresh, apply per-scope budgets, and re-render the instruction
/// string.
///
/// This is the only place configured entries are read and charged. They are
/// user-level authority, so they belong to the global scope, and reading them
/// after the cache lookup means editing one takes effect without invalidating
/// the file-keyed context cache.
pub fn assemble_instruction_block(
    ctx: &mut ProjectContext,
    configured: &[crate::prompts::InstructionSource],
    budgets: &InstructionBudgets,
) -> InstructionAssembly {
    append_configured_instructions(ctx, configured, budgets.file_max_bytes);
    let mut budget_notices =
        apply_instruction_budgets(&mut ctx.instruction_segments, &mut ctx.rules_block, budgets);
    budget_notices.retain(|notice| {
        notice.is_loss()
            || !ctx
                .notices
                .iter()
                .any(|loss| loss.is_loss() && loss.path.is_some() && loss.path == notice.path)
    });
    ctx.notices.append(&mut budget_notices);
    ctx.rebuild_joined_instructions();

    let global_tokens =
        render_instruction_scope(&ctx.instruction_segments, InstructionScope::Global)
            .as_deref()
            .map_or(0, estimate_instruction_tokens);
    let mut project_tokens =
        render_instruction_scope(&ctx.instruction_segments, InstructionScope::Project)
            .as_deref()
            .map_or(0, estimate_instruction_tokens);
    if let Some(rules) = ctx.rules_block.as_deref() {
        project_tokens = project_tokens.saturating_add(estimate_instruction_tokens(rules));
    }

    InstructionAssembly {
        block: ctx.as_system_block(),
        source_path: ctx.source_path.clone(),
        constitution_source_path: ctx.constitution_source_path.clone(),
        constitution_tokens: ctx
            .constitution_block
            .as_deref()
            .map_or(0, estimate_instruction_tokens),
        warnings: ctx.warnings.clone(),
        prompt_range: None,
        notices: ctx.notices.clone(),
        global_tokens,
        project_tokens,
        budgets: *budgets,
    }
}

pub fn append_configured_instructions(
    ctx: &mut ProjectContext,
    configured: &[crate::prompts::InstructionSource],
    file_max_bytes: u64,
) {
    let (configured_segments, mut fresh_notices) =
        configured_instruction_segments(configured, file_max_bytes);
    let mut loaded: Vec<PathBuf> = ctx
        .instruction_segments
        .iter()
        .filter_map(|segment| segment.canonical_path.clone())
        .collect();
    for segment in configured_segments {
        if let Some(canonical) = segment.canonical_path.clone() {
            if loaded.contains(&canonical) {
                fresh_notices.push(InstructionNotice {
                    path: segment.path.clone(),
                    scope: segment.scope,
                    origin: segment.origin,
                    action: InstructionAction::Skipped,
                    bytes_kept: 0,
                    bytes_dropped: segment.text.len() as u64,
                    tokens_kept: 0,
                    tokens_dropped: estimate_instruction_tokens(&segment.text),
                    limit: InstructionLimit::DuplicatePath,
                    fallback_selected: None,
                    detail: Some(InstructionDetail::AlreadyLoaded),
                });
                continue;
            }
            loaded.push(canonical);
        }
        ctx.instruction_segments.push(segment);
    }

    ctx.notices.append(&mut fresh_notices);
    ctx.rebuild_joined_instructions();
}

pub fn capture_instruction_context(
    workspace: &Path,
    configured: &[crate::prompts::InstructionSource],
    settings: &InstructionSettings,
) -> ProjectContext {
    let mut ctx = load_project_context_with_settings(workspace, settings);
    append_configured_instructions(&mut ctx, configured, settings.file_max_bytes());
    ctx
}

/// The instruction notice a session must show before its first turn, or an
/// error when `policy = "refuse"` and a budget loss would have occurred.
///
/// Deterministic from the current files and config, so the interactive start,
/// the headless stderr line, and a resumed session all state the same losses.
#[cfg(any(test, feature = "test-support"))]
pub fn startup_instruction_notice(
    workspace: &Path,
    configured: &[crate::prompts::InstructionSource],
    window_tokens: Option<usize>,
    output_tokens: Option<usize>,
    settings: &InstructionSettings,
) -> Result<Option<String>, String> {
    let budgets =
        InstructionBudgets::resolve(Some(&settings.budget), window_tokens, output_tokens)?;
    let mut ctx = load_project_context_with_settings(workspace, settings);
    let assembly = assemble_instruction_block(&mut ctx, configured, &budgets);
    if budgets.policy == InstructionBudgetPolicy::Refuse
        && let Some(loss) = assembly.notices.iter().find(|notice| {
            notice.is_loss()
                && matches!(
                    notice.limit,
                    InstructionLimit::ScopeBudget
                        | InstructionLimit::StaticGuard
                        | InstructionLimit::RulesBudget
                )
        })
    {
        let mut refused = loss.clone();
        refused.action = InstructionAction::Refused;
        return Err(format!(
            "instruction budget refused this session (policy = \"refuse\"): {refused}"
        ));
    }
    Ok(render_notice_body(&assembly.notices, "en"))
}

/// Read a configured instruction file without trusting its metadata length.
///
/// Returns the text and whether the file exceeded `cap`. An over-cap file
/// returns no text at all: the caller reports a skip, and nothing past the
/// ceiling is read into memory.
fn read_instruction_file_bounded(path: &Path, cap: u64) -> std::io::Result<(String, bool)> {
    use std::io::Read as _;

    let file = codewhale_util::fs_confined::open_user_read(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Ok((String::new(), true));
    }
    String::from_utf8(bytes)
        .map(|text| (text, false))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Read the configured `instructions = [...]` entries as global-scope
/// segments. Missing, empty, and over-ceiling files are reported and skipped;
/// they never fail the launch, and `policy = refuse` does not turn a skip into
/// a refusal — one bad include must not make the project unopenable.
fn configured_instruction_segments(
    configured: &[crate::prompts::InstructionSource],
    file_max_bytes: u64,
) -> (Vec<InstructionSegment>, Vec<InstructionNotice>) {
    use crate::prompts::InstructionSource;

    let mut segments = Vec::new();
    let mut notices = Vec::new();
    for source in configured {
        if matches!(source, InstructionSource::Inline { name, .. } if name.starts_with("runtime:"))
        {
            continue;
        }
        let (path, name, content, read_error) = match source {
            InstructionSource::File(path) => {
                // Bound the read on the open handle instead of trusting the
                // metadata length: a zero-length device such as `/dev/zero`, or
                // a file that grows after the check, must not become an
                // unbounded read.
                match read_instruction_file_bounded(path, file_max_bytes) {
                    Ok((_, true)) => {
                        notices.push(InstructionNotice {
                            path: Some(path.clone()),
                            scope: InstructionScope::Global,
                            origin: InstructionOrigin::Configured,
                            action: InstructionAction::Skipped,
                            bytes_kept: 0,
                            bytes_dropped: 0,
                            tokens_kept: 0,
                            tokens_dropped: 0,
                            limit: InstructionLimit::FileCeiling,
                            fallback_selected: None,
                            detail: Some(InstructionDetail::OverFileCeiling {
                                max_bytes: file_max_bytes,
                            }),
                        });
                        continue;
                    }
                    Ok((text, false)) => (Some(path.clone()), None, text, None),
                    Err(error) => (Some(path.clone()), None, String::new(), Some(error)),
                }
            }
            InstructionSource::Inline { name, content } => {
                (None, Some(name.clone()), content.clone(), None)
            }
        };
        if let Some(error) = read_error {
            notices.push(InstructionNotice {
                path,
                scope: InstructionScope::Global,
                origin: InstructionOrigin::Configured,
                action: InstructionAction::Skipped,
                bytes_kept: 0,
                bytes_dropped: 0,
                tokens_kept: 0,
                tokens_dropped: 0,
                limit: InstructionLimit::None,
                fallback_selected: None,
                detail: Some(InstructionDetail::unreadable(&error)),
            });
            continue;
        }
        if content.trim().is_empty() {
            notices.push(InstructionNotice::loss(
                InstructionScope::Global,
                InstructionOrigin::Configured,
                path,
                InstructionLimit::EmptyFile,
                InstructionAction::Skipped,
            ));
            continue;
        }
        let content = if path.is_some() {
            retain_file_content(
                content,
                InstructionScope::Global,
                InstructionOrigin::Configured,
                path.as_deref(),
                &mut notices,
            )
        } else {
            content
        };
        let label = name.or_else(|| {
            path.as_ref().map(|path| {
                if path.is_absolute() {
                    file_label(path)
                } else {
                    path.to_string_lossy().replace('\\', "/")
                }
            })
        });
        let mut segment = InstructionSegment::new(
            InstructionScope::Global,
            InstructionOrigin::Configured,
            path,
            content.trim().to_string(),
        );
        segment.canonical_path = segment
            .path
            .as_deref()
            .and_then(|path| fs::canonicalize(path).ok());
        segment.label = label;
        segments.push(segment);
    }
    (segments, notices)
}

fn retain_file_content(
    content: String,
    scope: InstructionScope,
    origin: InstructionOrigin,
    path: Option<&Path>,
    notices: &mut Vec<InstructionNotice>,
) -> String {
    const CONTENT_BYTES: usize = 100 * 1024;
    if content.len() <= CONTENT_BYTES {
        return content;
    }
    let mut end = CONTENT_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let head = &content[..end];
    let dropped = content.len() - end;
    notices.push(InstructionNotice {
        path: path.map(Path::to_path_buf),
        scope,
        origin,
        action: InstructionAction::Trimmed,
        bytes_kept: end as u64,
        bytes_dropped: dropped as u64,
        tokens_kept: estimate_instruction_tokens(head),
        tokens_dropped: estimate_instruction_tokens(&content[end..]),
        limit: InstructionLimit::FileContent,
        fallback_selected: None,
        detail: None,
    });
    format!("{head}\n\n[…instruction file truncated: {end} bytes kept, {dropped} bytes dropped…]")
}

/// One directory's resolved instruction segments plus the skips that produced
/// them.
#[derive(Debug, Default)]
struct DirInstructions {
    segments: Vec<InstructionSegment>,
    notices: Vec<InstructionNotice>,
    /// Human-readable reason a candidate was refused. The notice carries the
    /// structured form; this is the text `/context report` and the Runtime API
    /// listing have always shown.
    warnings: Vec<String>,
}
/// Load project context from the workspace directory.
///
/// This searches for known project context files and loads the first one found.
/// Convenience wrapper that imports no foreign format.
///
/// Production goes through [`load_project_context_with_imports`] so the import
/// set is explicit at the call site; this exists for tests that only care about
/// default behaviour.
#[cfg(any(test, feature = "test-support"))]
pub fn load_project_context(workspace: &Path) -> ProjectContext {
    load_project_context_with_imports(workspace, &ForeignInstructionImports::default())
}

/// Load workspace project context under an explicit foreign-import set.
///
/// The set is a parameter rather than a global read so a caller — and every
/// test — states which foreign formats are in play instead of depending on
/// process-wide state that parallel tests would race on. Production loads the
/// repository chain through `load_project_context_with_settings`, which shares
/// every helper here.
#[cfg(any(test, feature = "test-support"))]
pub fn load_project_context_with_imports(
    workspace: &Path,
    imports: &ForeignInstructionImports,
) -> ProjectContext {
    let mut ctx = ProjectContext::empty(workspace.to_path_buf());
    let label_root = find_git_root(workspace);
    let mut loaded: Vec<PathBuf> = Vec::new();
    let dir = {
        let mut scope = DirLoadContext {
            imports,
            file_max_bytes: DEFAULT_FILE_MAX_BYTES,
            claude_fallback: false,
            label_root: label_root.as_deref(),
            loaded: &mut loaded,
        };
        load_dir_instructions(workspace, &mut scope)
    };
    if let Some(last) = dir.segments.last() {
        ctx.source_path = last.path.clone();
    }
    ctx.notices.extend(dir.notices);
    ctx.warnings.extend(dir.warnings);
    ctx.instruction_segments.extend(dir.segments);

    ctx.warnings
        .extend(ignored_project_whale_warnings(workspace));
    ctx.warnings
        .extend(unimported_foreign_warnings(workspace, imports));

    // Rules are workspace-scope authority. Labels are repo-relative because
    // the block sits inside the pinned system prompt; the project scope
    // budget charges it after the instruction chain.
    let rules = load_rules_block(
        workspace,
        imports,
        label_root.as_deref().unwrap_or(workspace),
        DEFAULT_FILE_MAX_BYTES,
    );
    ctx.notices.extend(rules.notices);
    ctx.rules_block = rules.block;

    // Check for trust file
    ctx.is_trusted = check_trust_status(workspace);

    ctx.rebuild_joined_instructions();
    ctx
}

/// Everything one scope directory's loader needs, so the selection rules are
/// stated once and shared by the workspace loader and the ancestor chain.
struct DirLoadContext<'a> {
    imports: &'a ForeignInstructionImports,
    file_max_bytes: u64,
    /// Read `CLAUDE.md` when no `AGENTS.md`/codewhale file is usable.
    claude_fallback: bool,
    /// Repository root for repo-relative segment labels.
    label_root: Option<&'a Path>,
    /// Canonical paths already loaded by wider scopes; one physical file loads
    /// once no matter how many scopes link to it.
    loaded: &'a mut Vec<PathBuf>,
}

/// Resolve one directory's instruction files.
///
/// Selection follows OpenCode's model with codewhale's own sources preserved:
/// `AGENTS.md` wins; codewhale's `.codewhale/instructions.md` (and the legacy
/// `.deepseek/` spelling) sit next; `CLAUDE.md` is co-read when the `claude`
/// import is enabled and is read as a fallback only when `claude_fallback` is
/// opted in. A candidate that
/// exists but cannot be used counts as **absent**, so the next candidate is
/// tried — including `CLAUDE.md`. A duplicate of an already-loaded file is
/// *not* absent: the content is already in the prompt from a wider scope, so
/// no fallback runs and the notice says `duplicate`.
fn load_dir_instructions(dir: &Path, scope: &mut DirLoadContext<'_>) -> DirInstructions {
    let mut out = DirInstructions::default();
    let mut chain: Vec<(&'static str, InstructionOrigin)> = vec![
        ("AGENTS.md", InstructionOrigin::Agents),
        (
            ".codewhale/instructions.md",
            InstructionOrigin::Instructions,
        ),
        (".deepseek/instructions.md", InstructionOrigin::Instructions),
    ];
    if scope.imports.is_enabled(ForeignInstructionFormat::Claude) {
        chain.push((".claude/instructions.md", InstructionOrigin::Claude));
    }

    let mut primary_resolved = false;
    let mut preferred_skipped: Option<PathBuf> = None;
    for (filename, origin) in chain {
        let path = dir.join(filename);
        if !context_candidate_exists(&path) {
            continue;
        }
        if is_duplicate(&path, scope.loaded) {
            out.notices.push(duplicate_notice(origin, &path));
            // A duplicate is not "absent": the content is already in the
            // prompt from a wider scope, so no fallback runs for this
            // directory.
            primary_resolved = true;
            break;
        }
        match load_context_file_limited(dir, &path, scope.file_max_bytes) {
            Ok(content) => {
                let pushed = push_loaded_segment(&mut out, scope, origin, &path, content);
                if pushed
                    && let Some(preferred) = preferred_skipped.as_ref()
                    && let Some(skipped) = out
                        .notices
                        .iter_mut()
                        .find(|notice| notice.path.as_ref() == Some(preferred))
                {
                    skipped.fallback_selected = Some(relative_label(dir, &path));
                }
                primary_resolved = true;
                break;
            }
            Err(error) => {
                if preferred_skipped.is_none() {
                    preferred_skipped = Some(path.clone());
                }
                out.warnings.push(error.to_string());
                out.notices.push(skip_notice(origin, &path, &error));
            }
        }
    }

    let claude_imported = scope.imports.is_enabled(ForeignInstructionFormat::Claude);
    let claude_paths: Vec<PathBuf> = CLAUDE_CONTEXT_FILES
        .iter()
        .map(|filename| dir.join(filename))
        .filter(|path| context_candidate_exists(path))
        .collect();
    // An explicit import co-reads `CLAUDE.md` next to `AGENTS.md`; the default
    // fallback runs only when nothing else in this directory loaded, and
    // `claude_fallback = false` is an explicit opt-out.
    let read_claude = claude_imported || (scope.claude_fallback && !primary_resolved);
    if read_claude {
        // Candidates are tried in order and a skipped one falls through to the
        // next, exactly like the `AGENTS.md` chain.
        for path in claude_paths {
            if is_duplicate(&path, scope.loaded) {
                out.notices
                    .push(duplicate_notice(InstructionOrigin::Claude, &path));
                return out;
            }
            match load_context_file_limited(dir, &path, scope.file_max_bytes) {
                Ok(content) => {
                    let pushed = push_loaded_segment(
                        &mut out,
                        scope,
                        InstructionOrigin::Claude,
                        &path,
                        content,
                    );
                    if pushed
                        && !primary_resolved
                        && let Some(preferred) = preferred_skipped.as_ref()
                        && let Some(skipped) = out
                            .notices
                            .iter_mut()
                            .find(|notice| notice.path.as_ref() == Some(preferred))
                    {
                        skipped.fallback_selected = Some(relative_label(dir, &path));
                    }
                    break;
                }
                Err(error) => {
                    out.warnings.push(error.to_string());
                    out.notices
                        .push(skip_notice(InstructionOrigin::Claude, &path, &error))
                }
            }
        }
    }

    out
}

/// Push one loaded file as a project-scope segment, reporting a duplicate
/// instead of loading the same physical file twice.
fn push_loaded_segment(
    out: &mut DirInstructions,
    scope: &mut DirLoadContext<'_>,
    origin: InstructionOrigin,
    path: &Path,
    content: String,
) -> bool {
    let canonical = fs::canonicalize(path).ok();
    if let Some(canonical) = canonical.as_ref() {
        if scope.loaded.contains(canonical) {
            out.notices.push(InstructionNotice {
                path: Some(path.to_path_buf()),
                scope: InstructionScope::Project,
                origin,
                action: InstructionAction::Skipped,
                bytes_kept: 0,
                bytes_dropped: content.len() as u64,
                tokens_kept: 0,
                tokens_dropped: estimate_instruction_tokens(&content),
                limit: InstructionLimit::DuplicatePath,
                fallback_selected: None,
                detail: Some(InstructionDetail::AlreadyLoaded),
            });
            return false;
        }
        scope.loaded.push(canonical.clone());
    }
    tracing::info!(
        "Loaded project context from {} ({} bytes)",
        path.display(),
        content.len()
    );
    let content = retain_file_content(
        content,
        InstructionScope::Project,
        origin,
        Some(path),
        &mut out.notices,
    );
    let mut segment = InstructionSegment::new(
        InstructionScope::Project,
        origin,
        Some(path.to_path_buf()),
        content,
    );
    segment.canonical_path = canonical;
    segment.label = Some(repo_relative_source_label(path, scope.label_root));
    out.segments.push(segment);
    true
}

/// Whether `path` names a physical file already loaded by a wider scope.
fn is_duplicate(path: &Path, loaded: &[PathBuf]) -> bool {
    fs::canonicalize(path).is_ok_and(|canonical| loaded.contains(&canonical))
}

fn relative_label(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn duplicate_notice(origin: InstructionOrigin, path: &Path) -> InstructionNotice {
    InstructionNotice {
        path: Some(path.to_path_buf()),
        scope: InstructionScope::Project,
        origin,
        action: InstructionAction::Skipped,
        bytes_kept: 0,
        bytes_dropped: 0,
        tokens_kept: 0,
        tokens_dropped: 0,
        limit: InstructionLimit::DuplicatePath,
        fallback_selected: None,
        detail: Some(InstructionDetail::AlreadyLoaded),
    }
}

fn skip_notice(
    origin: InstructionOrigin,
    path: &Path,
    error: &ProjectContextError,
) -> InstructionNotice {
    InstructionNotice {
        path: Some(path.to_path_buf()),
        scope: InstructionScope::Project,
        origin,
        action: InstructionAction::Skipped,
        bytes_kept: 0,
        bytes_dropped: 0,
        tokens_kept: 0,
        tokens_dropped: 0,
        limit: error.kind(),
        fallback_selected: None,
        detail: error.detail(),
    }
}

/// Load project context from the containing repository as well.
///
/// Applicable instruction files resolve from the repository root down to the
/// workspace (inclusive) and are assembled in that order under one aggregate
/// byte budget, so wider scopes read first and the workspace keeps the last
/// word. Repository identity comes from the containing checkout itself
/// (Git dir/worktree traversal, [`find_git_root`]) — never from branch names
/// or paths mentioned in conversation — and the chain never crosses the
/// repository boundary. Outside any repository only the workspace itself is
/// searched.
pub fn load_project_context_with_parents(workspace: &Path) -> ProjectContext {
    load_project_context_with_settings(workspace, &InstructionSettings::default())
}

pub fn load_project_context_with_settings(
    workspace: &Path,
    settings: &InstructionSettings,
) -> ProjectContext {
    let workspace = canonicalize_workspace_or_keep(workspace);
    let pre = crate::project_context_cache::compute_cache_key_with_settings(&workspace, settings);
    if let Some(ctx) = crate::project_context_cache::lookup(&pre) {
        return ctx;
    }
    let ctx = load_project_context_with_settings_uncached(&workspace, settings);
    let post = crate::project_context_cache::compute_cache_key_with_settings(&workspace, settings);
    if pre == post {
        crate::project_context_cache::store(pre, ctx.clone());
    }
    ctx
}

#[cfg(test)]
fn load_project_context_with_parents_cached_and_home(
    workspace: &Path,
    home_dir: Option<&Path>,
) -> ProjectContext {
    load_project_context_with_settings(
        workspace,
        &InstructionSettings {
            home: home_dir.map(Path::to_path_buf),
            ..Default::default()
        },
    )
}

#[cfg(test)]
fn load_project_context_with_parents_and_home(
    workspace: &Path,
    home_dir: Option<&Path>,
) -> ProjectContext {
    load_project_context_with_parents_cached_and_home(workspace, home_dir)
}

fn load_project_context_with_settings_uncached(
    workspace: &Path,
    settings: &InstructionSettings,
) -> ProjectContext {
    let home_dir = settings.home.as_deref();
    let imports = &settings.imports;
    let workspace_canonical = canonicalize_workspace_or_keep(workspace);
    // Chain-segment labels sit inside the pinned system prompt, so they are
    // rendered repo-relative: a checkout move or recase leaves the labels —
    // and therefore the whole prompt prefix — byte-identical. The chain
    // never leaves the checkout (`context_chain_dirs`), so every labeled
    // path strips cleanly.
    let git_root = find_git_root(&workspace_canonical);
    let claude_fallback = settings.claude_fallback();
    let mut ctx = ProjectContext::empty(workspace_canonical.clone());

    // The global layer loads first: it is the widest scope, so a project file
    // symlinked to it is recognised as a duplicate instead of being charged
    // twice.
    let mut loaded: Vec<PathBuf> = Vec::new();
    let global = load_global_agents_context(
        &workspace_canonical,
        home_dir,
        claude_fallback || imports.is_enabled(ForeignInstructionFormat::Claude),
        settings.file_max_bytes(),
        &mut loaded,
    );
    if let Some(global) = &global {
        ctx.warnings.extend(global.warnings.iter().cloned());
        ctx.notices.extend(global.notices.iter().cloned());
    }

    // Assemble the repository-root → workspace instruction chain. The chain
    // directories come from Git traversal of the containing checkout, so a
    // linked worktree contributes its own root and files above the root —
    // other checkouts, unrelated parents — stay out of scope. Segments are
    // appended root first, so the nearest scope has the last word and is the
    // last thing a budget trim drops.
    let chain_dirs = context_chain_dirs(&workspace_canonical, git_root.as_deref(), home_dir);
    let mut project_segments: Vec<InstructionSegment> = Vec::new();
    {
        let mut scope = DirLoadContext {
            imports,
            file_max_bytes: settings.file_max_bytes(),
            claude_fallback,
            label_root: git_root.as_deref(),
            loaded: &mut loaded,
        };
        for dir in &chain_dirs {
            ctx.warnings.extend(ignored_project_whale_warnings(dir));
            let dir_instructions = load_dir_instructions(dir, &mut scope);
            ctx.notices.extend(dir_instructions.notices);
            ctx.warnings.extend(dir_instructions.warnings);
            project_segments.extend(dir_instructions.segments);
        }
    }

    // Unimported foreign formats stay discoverable rather than silently
    // ignored: the warning names the opt-in key that would read them.
    ctx.warnings
        .extend(unimported_foreign_warnings(&workspace_canonical, imports));

    // Rules are workspace-scope authority: the chain loader never reads
    // ancestor rules.
    let rules = load_rules_block(
        &workspace_canonical,
        imports,
        git_root.as_deref().unwrap_or(workspace_canonical.as_path()),
        settings.file_max_bytes(),
    );
    ctx.notices.extend(rules.notices);
    ctx.rules_block = rules.block;

    // `source_path` names the most specific source — the workspace file when
    // there is one, otherwise the nearest ancestor that contributed.
    if let Some(last) = project_segments.last() {
        ctx.source_path = last.path.clone();
    }
    ctx.instruction_segments = project_segments;

    if let Some(global) = global {
        let mut segments = global.instruction_segments;
        segments.append(&mut ctx.instruction_segments);
        ctx.instruction_segments = segments;
        if ctx.source_path.is_none() {
            ctx.source_path = global.source_path;
        }
    }
    ctx.is_trusted = check_trust_status(&workspace_canonical);
    ctx.rebuild_joined_instructions();

    // Generate a bounded in-memory fallback when no context file exists
    // anywhere. This keeps prompt shape stable without creating project-local
    // `.codewhale/` files merely because Codewhale was opened in a directory.
    if !ctx.has_instructions()
        && let Some(generated) = generate_ephemeral_context(workspace)
    {
        ctx.instruction_segments.push(InstructionSegment::new(
            InstructionScope::Project,
            InstructionOrigin::Instructions,
            None,
            generated,
        ));
        ctx.source_path = None;
        ctx.rebuild_joined_instructions();
    }

    // Load the Codewhale-specific repo authority policy
    // (.codewhale/constitution.json) independently of the prose instructions —
    // it is a distinct, higher-authority artifact and may exist with or without
    // an AGENTS.md. Legacy WHALE.md files are ignored and reported as
    // migration-only diagnostics.
    let (constitution_block, constitution_source_path, constitution_warnings) =
        load_repo_constitution_block(workspace);
    ctx.warnings.extend(constitution_warnings);
    ctx.constitution_block = constitution_block;
    ctx.constitution_source_path = constitution_source_path;

    ctx
}

/// Assemble the workspace rules block. Labels are repo-relative because the
/// block sits inside the pinned system prompt.
fn load_rules_block(
    workspace: &Path,
    imports: &ForeignInstructionImports,
    label_root: &Path,
    file_max_bytes: u64,
) -> RulesLoad {
    let mut block = String::new();
    let mut notices = Vec::new();
    for rules_dir in rules_dirs_for(imports) {
        let rules = load_rules_from_dir(workspace, rules_dir, file_max_bytes);
        notices.extend(rules.notices);
        for (path, content) in rules.entries {
            if !block.is_empty() {
                block.push('\n');
            }
            block.push_str(&format!(
                "<project_rule source=\"{}\">\n{}\n</project_rule>",
                repo_relative_source_label(&path, Some(label_root)),
                content.trim()
            ));
        }
    }
    RulesLoad {
        block: if block.is_empty() { None } else { Some(block) },
        notices,
    }
}

/// One rules directory pass: the loaded files and the skips that produced
/// them. A rules overflow or an unreadable file is a reported skip, never a
/// silent drop.
struct RulesDirLoad {
    entries: Vec<(PathBuf, String)>,
    notices: Vec<InstructionNotice>,
}

/// The assembled rules block plus the skips from every rules directory.
struct RulesLoad {
    block: Option<String>,
    notices: Vec<InstructionNotice>,
}

pub fn project_context_cache_candidate_paths(
    workspace: &Path,
    home_dir: Option<&Path>,
) -> Vec<(PathBuf, Option<PathBuf>)> {
    let workspace = canonicalize_workspace_or_keep(workspace);
    let mut paths = Vec::new();

    // Enumerate the superset of instruction candidates, not just the ones the
    // active opt-in set loads: a `CLAUDE.md` that is *not* imported still
    // decides whether the "not loaded" warning fires, so its content has to
    // invalidate the cache too. The cache key carries the loading settings, so
    // a changed opt-in set never reuses an entry and over-enumerating here
    // only ever costs an extra reload.
    let repo_root = find_git_root(&workspace);
    for dir in context_chain_dirs(&workspace, repo_root.as_deref(), home_dir) {
        for filename in PROJECT_CONTEXT_FILES {
            paths.push((dir.join(filename), Some(dir.clone())));
        }
        // `CLAUDE.md` / `.claude/CLAUDE.md` decide the selection even when the
        // `claude` import is off (`claude_fallback`), so their content must
        // invalidate the cache too. `PROJECT_CONTEXT_FILES` already carries
        // `CLAUDE.md`; the hidden spelling is added here.
        paths.push((dir.join(".claude/CLAUDE.md"), Some(dir.clone())));
        paths.push((dir.join(DEPRECATED_WHALE_FILENAME), Some(dir.clone())));
    }

    if let Some(home) = home_dir {
        for candidate in global_context_relative_paths() {
            paths.push((join_relative_components(home, candidate), None));
        }
        // Last-resort global `~/.claude/CLAUDE.md` (#6526).
        paths.push((
            join_relative_components(home, &[".claude", "CLAUDE.md"]),
            None,
        ));
        for candidate in legacy_global_whale_relative_paths() {
            paths.push((join_relative_components(home, candidate), None));
        }
    }

    paths.extend(
        repo_constitution_candidate_paths(&workspace)
            .into_iter()
            .map(|path| {
                let root = path.parent().and_then(Path::parent).map(Path::to_path_buf);
                (path, root)
            }),
    );
    paths.push((
        workspace.join(".deepseek").join("trusted"),
        Some(workspace.clone()),
    ));
    paths.push((
        workspace.join(".deepseek").join("trust.json"),
        Some(workspace.clone()),
    ));
    paths.extend(
        codewhale_runtime::config::workspace_trust_config_candidate_paths()
            .into_iter()
            .map(|path| (path, None)),
    );

    // Include auto-discovered rules directory files so cache invalidates
    // when rules change (not just when AGENTS.md changes).
    for rules_dir in RULES_DIRS {
        let dir_path = workspace.join(rules_dir);
        // Skip symlinked rules directories (same guard as load_rules_from_dir)
        if fs::symlink_metadata(&dir_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "md") {
                    paths.push((path, Some(workspace.clone())));
                }
            }
        }
    }

    // The warning for unimported foreign formats is part of the cached
    // ProjectContext. Fingerprint exactly the safe, capped fragment files the
    // bounded loader can select so creating, changing, removing, or making one
    // unusable invalidates a previously cached warning decision. Enumerating
    // every format is intentional: changing the opt-in set clears the cache,
    // and the superset keeps disabled-format discovery correct.
    for format in ForeignInstructionFormat::ALL {
        paths.extend(
            codewhale_core::fragments::selected_project_instruction_candidate_files(
                &workspace,
                format.fragment_candidates(),
            )
            .into_iter()
            .map(|path| (path, Some(workspace.clone()))),
        );
    }

    paths
}

fn global_context_relative_paths() -> [&'static [&'static str]; 6] {
    [
        GLOBAL_AGENTS_RELATIVE_PATH,
        GLOBAL_AGENTS_VENDOR_NEUTRAL_PATH,
        GLOBAL_AGENTS_LEGACY_PATH,
        GLOBAL_INSTRUCTIONS_RELATIVE_PATH,
        GLOBAL_INSTRUCTIONS_VENDOR_NEUTRAL_PATH,
        GLOBAL_INSTRUCTIONS_LEGACY_PATH,
    ]
}

fn legacy_global_whale_relative_paths() -> [&'static [&'static str]; 3] {
    [
        GLOBAL_WHALE_RELATIVE_PATH,
        GLOBAL_WHALE_VENDOR_NEUTRAL_PATH,
        GLOBAL_WHALE_LEGACY_PATH,
    ]
}

fn join_relative_components(base: &Path, relative: &[&str]) -> PathBuf {
    let mut path = base.to_path_buf();
    for component in relative {
        path.push(component);
    }
    path
}

fn ignored_project_whale_warnings(dir: &Path) -> Vec<String> {
    let path = dir.join(DEPRECATED_WHALE_FILENAME);
    ignored_whale_warning_for_path(&path).into_iter().collect()
}

fn ignored_global_whale_warnings(home: &Path) -> Vec<String> {
    legacy_global_whale_relative_paths()
        .iter()
        .filter_map(|candidate| {
            let path = join_relative_components(home, candidate);
            ignored_whale_warning_for_path(&path)
        })
        .collect()
}

fn ignored_whale_warning_for_path(path: &Path) -> Option<String> {
    context_candidate_exists(path)
        .then(|| format!("{WHALE_IGNORED_WARNING} Ignored file: {}", path.display()))
}

fn canonicalize_workspace_or_keep(workspace: &Path) -> PathBuf {
    fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
}

/// Find the root of the checkout that contains `dir`.
///
/// Walks upward looking for a `.git` entry, following Git's own discovery
/// semantics: a `.git` directory must hold `HEAD`, and a `.git` file must be
/// a `gitdir:` pointer (a linked worktree). A linked worktree is therefore
/// its own root — the main checkout is reachable only through that pointer,
/// never through directory heuristics, branch names, or paths mentioned in
/// conversation. This is the single source of truth for repository identity
/// in project-context scope resolution.
pub fn find_git_root(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        let git_entry = current.join(".git");
        if is_git_metadata_entry(&git_entry) {
            return Some(current);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

fn is_git_metadata_entry(path: &Path) -> bool {
    if path.is_dir() {
        return path.join("HEAD").is_file();
    }

    fs::read_to_string(path)
        .map(|content| content.trim_start().starts_with("gitdir:"))
        .unwrap_or(false)
}

/// Directories whose instruction files apply to `workspace`, ordered from the
/// repository root down to the workspace (inclusive).
///
/// Repository identity comes from the containing checkout itself
/// ([`find_git_root`]), passed in as `repo_root` so the chain bounds and the
/// chain-segment labels are derived from one and the same walk; the chain
/// never crosses the repository boundary, so sibling checkouts and unrelated
/// parents stay out of scope. Outside any repository (`repo_root` is `None`)
/// only the workspace itself is searched. When `home_dir` is an ancestor it
/// remains an outer boundary the walk never leaves.
fn context_chain_dirs(
    workspace: &Path,
    repo_root: Option<&Path>,
    home_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut stop = repo_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.to_path_buf());

    if let Some(home) = home_dir {
        let home = canonicalize_workspace_or_keep(home);
        // Clamp only when the walk would otherwise leave the user's home
        // (home sits between the workspace and the repository root).
        if workspace.starts_with(&home) && home.starts_with(&stop) {
            stop = home;
        }
    }

    let mut dirs = Vec::new();
    let mut cursor = workspace.to_path_buf();
    loop {
        dirs.push(cursor.clone());
        if cursor == stop {
            break;
        }
        match cursor.parent() {
            Some(parent) if parent != cursor => cursor = parent.to_path_buf(),
            _ => break,
        }
    }
    dirs.reverse();
    dirs
}

fn load_global_agents_context(
    workspace: &Path,
    home_dir: Option<&Path>,
    claude_fallback: bool,
    file_max_bytes: u64,
    loaded: &mut Vec<PathBuf>,
) -> Option<ProjectContext> {
    let home = home_dir?;

    // Priority order (AGENTS.md preferred; instructions.md next, #3012):
    // 1. ~/.codewhale/AGENTS.md       (canonical)
    // 2. ~/.agents/AGENTS.md          (vendor-neutral fallback)
    // 3. ~/.deepseek/AGENTS.md        (legacy fallback)
    // 4. ~/.codewhale/instructions.md (canonical)
    // 5. ~/.agents/instructions.md    (vendor-neutral fallback)
    // 6. ~/.deepseek/instructions.md  (legacy fallback)
    // 7. ~/.claude/CLAUDE.md          (last resort, `claude_fallback` only)
    //
    // A candidate that exists but cannot be used counts as absent and the
    // next one is tried. Global WHALE.md files are ignored and reported as
    // migration-only diagnostics, never loaded as fallback law.
    let mut warnings = ignored_global_whale_warnings(home);
    let mut notices: Vec<InstructionNotice> = Vec::new();
    let mut global_preferred_skipped: Option<PathBuf> = None;
    let mut chosen: Option<(PathBuf, String, InstructionOrigin)> = None;

    for (index, candidate) in global_context_relative_paths().iter().enumerate() {
        let origin = if index < 3 {
            InstructionOrigin::Agents
        } else {
            InstructionOrigin::Instructions
        };
        let path = join_relative_components(home, candidate);
        if !context_candidate_exists(&path) {
            continue;
        }
        match load_global_context_file_limited(&path, file_max_bytes) {
            Ok(content) => {
                chosen = Some((path, content, origin));
                break;
            }
            Err(error) => {
                warnings.push(error.to_string());
                if global_preferred_skipped.is_none() {
                    global_preferred_skipped = Some(path.clone());
                }
                notices.push(InstructionNotice {
                    path: Some(path.clone()),
                    scope: InstructionScope::Global,
                    origin,
                    action: InstructionAction::Skipped,
                    bytes_kept: 0,
                    bytes_dropped: 0,
                    tokens_kept: 0,
                    tokens_dropped: 0,
                    limit: error.kind(),
                    fallback_selected: None,
                    detail: error.detail(),
                });
            }
        }
    }

    // `~/.claude/CLAUDE.md` is the last resort: foreign files never outrank
    // codewhale's own, so it is read only when none of the six candidates
    // loaded.
    if chosen.is_none() && claude_fallback {
        let path = join_relative_components(home, &[".claude", "CLAUDE.md"]);
        if context_candidate_exists(&path) {
            match load_global_context_file_limited(&path, file_max_bytes) {
                Ok(content) => {
                    chosen = Some((path, content, InstructionOrigin::Claude));
                }
                Err(error) => {
                    warnings.push(error.to_string());
                    notices.push(InstructionNotice {
                        path: Some(path.clone()),
                        scope: InstructionScope::Global,
                        origin: InstructionOrigin::Claude,
                        action: InstructionAction::Skipped,
                        bytes_kept: 0,
                        bytes_dropped: 0,
                        tokens_kept: 0,
                        tokens_dropped: 0,
                        limit: error.kind(),
                        fallback_selected: None,
                        detail: error.detail(),
                    });
                }
            }
        }
    }

    let mut ctx = ProjectContext::empty(workspace.to_path_buf());
    ctx.warnings = warnings;
    ctx.notices = notices;
    if let Some((path, content, origin)) = chosen {
        if let Some(preferred) = global_preferred_skipped.as_ref()
            && let Some(skipped) = ctx
                .notices
                .iter_mut()
                .find(|notice| notice.path.as_ref() == Some(preferred))
        {
            skipped.fallback_selected = Some(relative_label(home, &path));
        }
        let content = retain_file_content(
            content,
            InstructionScope::Global,
            origin,
            Some(&path),
            &mut ctx.notices,
        );
        let canonical = fs::canonicalize(&path).ok();
        if let Some(canonical) = canonical.as_ref() {
            loaded.push(canonical.clone());
        }
        let mut segment = InstructionSegment::new(
            InstructionScope::Global,
            origin,
            Some(path.clone()),
            content,
        );
        segment.canonical_path = canonical;
        segment.label = Some(relative_label(home, &path));
        ctx.instruction_segments = vec![segment];
        ctx.source_path = Some(path);
        ctx.rebuild_joined_instructions();
    }

    if ctx.instruction_segments.is_empty() && ctx.warnings.is_empty() && ctx.notices.is_empty() {
        return None;
    }
    Some(ctx)
}

/// Generate ephemeral context from the project tree. Returns the generated
/// content on success without writing workspace files.
fn generate_ephemeral_context(workspace: &Path) -> Option<String> {
    let overview = generate_bounded_project_overview(workspace)?;

    Some(format!(
        "# Project Context (Auto-generated, ephemeral)\n\n\
         > This context was generated in memory by Codewhale.\n\
         > No .codewhale/instructions.md file was written.\n\n\
         {overview}"
    ))
}

/// Load a context file with size checking and no links below its scope root.
fn load_context_file(root: &Path, path: &Path) -> Result<String, ProjectContextError> {
    load_context_file_limited(root, path, DEFAULT_FILE_MAX_BYTES)
}

fn load_context_file_limited(
    root: &Path,
    path: &Path,
    cap: u64,
) -> Result<String, ProjectContextError> {
    let file = codewhale_util::fs_confined::open_read(root, path).map_err(|source| {
        ProjectContextError::Read {
            path: path.to_path_buf(),
            source,
        }
    })?;
    read_context_file(file, path, cap)
}

fn read_context_file(file: fs::File, path: &Path, cap: u64) -> Result<String, ProjectContextError> {
    let metadata = file
        .metadata()
        .map_err(|source| ProjectContextError::Metadata {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.len() > cap {
        return Err(ProjectContextError::TooLarge {
            path: path.to_path_buf(),
            size: metadata.len(),
            max: usize::try_from(cap).unwrap_or(usize::MAX),
        });
    }

    let mut bytes = Vec::new();
    file.take(cap.min(DEFAULT_FILE_MAX_BYTES).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ProjectContextError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > cap {
        return Err(ProjectContextError::TooLarge {
            path: path.to_path_buf(),
            size: bytes.len() as u64,
            max: cap as usize,
        });
    }
    let content = String::from_utf8(bytes).map_err(|error| ProjectContextError::Read {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    })?;

    // Basic validation
    if content.trim().is_empty() {
        return Err(ProjectContextError::Empty {
            path: path.to_path_buf(),
        });
    }

    Ok(content)
}

fn context_candidate_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        let file_type = metadata.file_type();
        file_type.is_file() || file_type.is_symlink()
    })
}

/// Scan a rules directory for `.md` files and load them in filename order.
/// Missing or unreadable directories return nothing (no error).
/// Each file is verified through `load_context_file` (size check, symlink safety).
///
/// A directory beyond [`MAX_RULES_FILES`] used to be truncated with a
/// `tracing::warn!` only, which made an over-cap workspace look like it had
/// fewer rules than it does. The overflow is now a reported skip.
fn load_rules_from_dir(
    workspace: &Path,
    rules_dir_name: &str,
    file_max_bytes: u64,
) -> RulesDirLoad {
    let rules_dir = workspace.join(rules_dir_name);
    let mut out = RulesDirLoad {
        entries: Vec::new(),
        notices: Vec::new(),
    };

    // Refuse a symlinked rules directory: the real .md files behind it
    // would pass per-file is_symlink checks and be read from outside the
    // workspace subtree — same escape class as #417.
    if fs::symlink_metadata(&rules_dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        tracing::warn!(
            target: "project_context",
            dir = %rules_dir.display(),
            "Refusing symlinked rules directory"
        );
        out.notices.push(InstructionNotice {
            path: Some(rules_dir),
            scope: InstructionScope::Project,
            origin: InstructionOrigin::Rules,
            action: InstructionAction::Skipped,
            bytes_kept: 0,
            bytes_dropped: 0,
            tokens_kept: 0,
            tokens_dropped: 0,
            limit: InstructionLimit::None,
            fallback_selected: None,
            detail: Some(InstructionDetail::SymlinkedRulesDir),
        });
        return out;
    }

    let dir_iter = match fs::read_dir(&rules_dir) {
        Ok(iter) => iter,
        Err(_) => return out,
    };

    let mut file_paths: Vec<PathBuf> = Vec::new();
    for entry in dir_iter.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") && context_candidate_exists(&path) {
            file_paths.push(path);
        }
    }

    // Sort by filename for deterministic order
    file_paths.sort_by(|a, b| {
        a.file_name()
            .unwrap_or_default()
            .cmp(b.file_name().unwrap_or_default())
    });

    // Enforce per-directory cap, reporting the files it drops.
    if file_paths.len() > MAX_RULES_FILES {
        tracing::warn!(
            target: "project_context",
            dir = %rules_dir.display(),
            total = file_paths.len(),
            cap = MAX_RULES_FILES,
            "Truncating rules directory to cap"
        );
        for path in file_paths.iter().skip(MAX_RULES_FILES) {
            out.notices.push(InstructionNotice {
                path: Some(path.clone()),
                scope: InstructionScope::Project,
                origin: InstructionOrigin::Rules,
                action: InstructionAction::Skipped,
                bytes_kept: 0,
                bytes_dropped: file_len(path).unwrap_or(0),
                tokens_kept: 0,
                tokens_dropped: 0,
                limit: InstructionLimit::RulesCap,
                fallback_selected: None,
                detail: Some(InstructionDetail::RulesCap {
                    max_files: MAX_RULES_FILES,
                }),
            });
        }
        file_paths.truncate(MAX_RULES_FILES);
    }

    for path in file_paths {
        match load_context_file_limited(workspace, &path, file_max_bytes) {
            Ok(content) => {
                tracing::info!(
                    "Loaded project rule from {} ({} bytes)",
                    path.display(),
                    content.len()
                );
                let content = retain_file_content(
                    content,
                    InstructionScope::Project,
                    InstructionOrigin::Rules,
                    Some(&path),
                    &mut out.notices,
                );
                out.entries.push((path, content));
            }
            Err(error) => {
                tracing::warn!(
                    target: "project_context",
                    ?error,
                    ?path,
                    "Skipping unreadable rules file"
                );
                out.notices.push(InstructionNotice {
                    path: Some(path.clone()),
                    scope: InstructionScope::Project,
                    origin: InstructionOrigin::Rules,
                    action: InstructionAction::Skipped,
                    bytes_kept: 0,
                    bytes_dropped: file_len(&path).unwrap_or(0),
                    tokens_kept: 0,
                    tokens_dropped: 0,
                    limit: error.kind(),
                    fallback_selected: None,
                    detail: error.detail(),
                });
            }
        }
    }

    out
}

/// User-owned global instructions may intentionally resolve through links.
#[cfg(all(test, unix))]
fn load_global_context_file(path: &Path) -> Result<String, ProjectContextError> {
    load_global_context_file_limited(path, DEFAULT_FILE_MAX_BYTES)
}

fn load_global_context_file_limited(path: &Path, cap: u64) -> Result<String, ProjectContextError> {
    let file = codewhale_util::fs_confined::open_user_read(path).map_err(|source| {
        ProjectContextError::Read {
            path: path.to_path_buf(),
            source,
        }
    })?;
    read_context_file(file, path, cap)
}

/// Check if this project is marked as trusted
fn check_trust_status(workspace: &Path) -> bool {
    if codewhale_runtime::config::is_workspace_trusted(workspace) {
        return true;
    }

    // Check for trust markers
    let trust_markers = [
        workspace.join(".deepseek").join("trusted"),
        workspace.join(".deepseek").join("trust.json"),
    ];

    for marker in &trust_markers {
        if marker.exists() {
            return true;
        }
    }

    false
}

/// Create a default AGENTS.md file for a project
pub fn create_default_agents_md(workspace: &Path) -> std::io::Result<PathBuf> {
    let agents_path = workspace.join("AGENTS.md");

    let default_content = r#"# Project Agent Instructions

This file provides guidance to AI agents (Codewhale, Claude Code, etc.) when working with code in this repository.

## File Location

Save this file as `AGENTS.md` in your project root so the CLI can load it automatically.

## Build and Development Commands

```bash
# Build
# cargo build              # Rust projects
# npm run build            # Node.js projects
# python -m build          # Python projects

# Test
# cargo test               # Rust
# npm test                 # Node.js
# pytest                   # Python

# Lint and Format
# cargo fmt && cargo clippy  # Rust
# npm run lint               # Node.js
# ruff check .               # Python
```

## Architecture Overview

<!-- Describe your project's high-level architecture here -->
<!-- Focus on the "big picture" that requires reading multiple files to understand -->

### Key Components

<!-- List and describe the main components/modules -->

### Data Flow

<!-- Describe how data flows through the system -->

## Configuration Files

<!-- List important configuration files and their purposes -->

## Extension Points

<!-- Describe how to extend the codebase (add new features, tools, etc.) -->

## Commit Messages

Use conventional commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`
"#;

    fs::write(&agents_path, default_content)?;
    Ok(agents_path)
}

// === Effective instruction-source listing (#6168) ===

/// Which layer of the instruction stack a source belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionSourceKind {
    /// Chain candidate (`AGENTS.md`, `.codewhale/instructions.md`, opted-in
    /// foreign files) in a repository-root → workspace scope directory.
    Project,
    /// `.md` file inside a rules directory (`.codewhale/rules`, opted-in
    /// foreign rules dirs) at workspace scope.
    Rule,
    /// User-level fallback (`~/.codewhale/AGENTS.md` and friends).
    Global,
    /// File selected by the bounded foreign-fragment loader
    /// (`codewhale_core::fragments`) for an opted-in format.
    Fragment,
    /// Configured `instructions = [...]` file (#454).
    Configured,
    /// `.codewhale/constitution.json` authority policy.
    Constitution,
    /// Present but deliberately never loaded (deprecated `WHALE.md`).
    Ignored,
}

/// Whether the prompt assembly actually consumed the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionSourceStatus {
    /// Read and merged into the assembled instructions.
    Loaded,
    /// Exists, but a higher-priority candidate in the same scope won.
    Shadowed,
    /// Exists but the loader refused, could not read, or skipped it
    /// (empty, oversized, over the per-directory rules cap).
    Skipped,
    /// Candidate path the loader checks; nothing is there.
    Missing,
}

/// One instruction-source candidate or loaded file, for the
/// workspace-instructions listing route.
#[derive(Debug, Clone)]
pub struct InstructionSourceInfo {
    pub kind: InstructionSourceKind,
    /// Scope directory the candidate belongs to: the chain directory for
    /// `Project`, the workspace for `Rule`/`Fragment`/`Configured`, the home
    /// directory for `Global`.
    pub scope_dir: PathBuf,
    /// Candidate or actual file path, as the loader resolves it.
    pub path: PathBuf,
    /// `symlink_metadata` saw a file or symlink (same check the loader runs).
    pub exists: bool,
    pub status: InstructionSourceStatus,
    /// File size in bytes when it could be stat'd.
    pub bytes: Option<u64>,
    /// Loader warning produced for this path, when one exists.
    pub warning: Option<String>,
}

fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

fn push_candidate_entry(
    sources: &mut Vec<InstructionSourceInfo>,
    kind: InstructionSourceKind,
    scope_dir: &Path,
    path: PathBuf,
    scope_loaded: &mut bool,
    loaded: Option<&mut Vec<PathBuf>>,
    load: impl FnOnce(&Path) -> Result<String, ProjectContextError>,
) {
    let exists = context_candidate_exists(&path);
    let (status, warning) = if !exists {
        (InstructionSourceStatus::Missing, None)
    } else if *scope_loaded {
        (InstructionSourceStatus::Shadowed, None)
    } else if loaded
        .as_deref()
        .is_some_and(|loaded| is_duplicate(&path, loaded))
    {
        *scope_loaded = true;
        (
            InstructionSourceStatus::Skipped,
            Some("already loaded by a wider scope".to_string()),
        )
    } else {
        match load(&path) {
            Ok(_) => {
                *scope_loaded = true;
                if let Some(loaded) = loaded
                    && let Ok(canonical) = fs::canonicalize(&path)
                {
                    loaded.push(canonical);
                }
                (InstructionSourceStatus::Loaded, None)
            }
            Err(error) => (InstructionSourceStatus::Skipped, Some(error.to_string())),
        }
    };
    sources.push(InstructionSourceInfo {
        kind,
        scope_dir: scope_dir.to_path_buf(),
        exists,
        bytes: file_len(&path),
        path,
        status,
        warning,
    });
}

/// Enumerate the effective instruction sources for `workspace` (#6168).
///
/// Mirrors the loaders above — same candidate order, same existence and
/// readability checks — so the listing reports what prompt assembly actually
/// picks up: precedence, shadowing, opt-in imports, and refusal warnings.
/// Read-only; it never creates or modifies files. `configured` is the
/// resolved `instructions = [...]` array (`Config::instructions_paths`);
/// `home_dir` gates the global layer exactly as
/// `load_project_context_with_parents_and_home` does.
///
/// Known limit: statuses describe file selection, not the per-scope token
/// budget — `assemble_instruction_block` can still trim or drop a loaded
/// source at render time without changing its `Loaded` status here. The
/// trim/skip notices on `ProjectContext` are the budget-side truth.
#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn project_instruction_sources(
    workspace: &Path,
    home_dir: Option<&Path>,
    configured: &[PathBuf],
) -> Vec<InstructionSourceInfo> {
    project_instruction_sources_with_settings(
        workspace,
        &InstructionSettings {
            home: home_dir.map(Path::to_path_buf),
            ..Default::default()
        },
        configured,
    )
}

pub fn project_instruction_sources_with_settings(
    workspace: &Path,
    settings: &InstructionSettings,
    configured: &[PathBuf],
) -> Vec<InstructionSourceInfo> {
    let home_dir = settings.home.as_deref();
    let imports = &settings.imports;
    let workspace = canonicalize_workspace_or_keep(workspace);
    let mut sources = Vec::new();
    let mut loaded = Vec::new();
    let _ = load_global_agents_context(
        &workspace,
        home_dir,
        settings.claude_fallback() || imports.is_enabled(ForeignInstructionFormat::Claude),
        settings.file_max_bytes(),
        &mut loaded,
    );

    // Repository-root → workspace instruction chain. Same dir order the
    // loader assembles, so wider scopes list first.
    let repo_root = find_git_root(&workspace);
    for dir in context_chain_dirs(&workspace, repo_root.as_deref(), home_dir) {
        let whale = dir.join(DEPRECATED_WHALE_FILENAME);
        if context_candidate_exists(&whale) {
            sources.push(InstructionSourceInfo {
                kind: InstructionSourceKind::Ignored,
                scope_dir: dir.clone(),
                path: whale.clone(),
                exists: true,
                status: InstructionSourceStatus::Skipped,
                bytes: file_len(&whale),
                warning: Some(WHALE_IGNORED_WARNING.to_string()),
            });
        }
        let claude_imported = imports.is_enabled(ForeignInstructionFormat::Claude);
        let mut scope_loaded = false;
        for filename in context_files_for(imports)
            .into_iter()
            .filter(|filename| !CLAUDE_CONTEXT_FILES.contains(filename))
        {
            push_candidate_entry(
                &mut sources,
                InstructionSourceKind::Project,
                &dir,
                dir.join(filename),
                &mut scope_loaded,
                Some(&mut loaded),
                |path| load_context_file_limited(&dir, path, settings.file_max_bytes()),
            );
        }
        let mut claude_loaded = !claude_imported && scope_loaded;
        if claude_imported || settings.claude_fallback() {
            for filename in CLAUDE_CONTEXT_FILES {
                push_candidate_entry(
                    &mut sources,
                    InstructionSourceKind::Project,
                    &dir,
                    dir.join(filename),
                    &mut claude_loaded,
                    Some(&mut loaded),
                    |path| load_context_file_limited(&dir, path, settings.file_max_bytes()),
                );
            }
        }
    }

    // Workspace-scope rules directories — the chain loader never reads
    // ancestor rules, so only the workspace contributes them.
    for rules_dir_name in rules_dirs_for(imports) {
        let rules_dir = workspace.join(rules_dir_name);
        if fs::symlink_metadata(&rules_dir)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            // Same refusal as `load_rules_from_dir`, surfaced rather than
            // logged-and-dropped.
            sources.push(InstructionSourceInfo {
                kind: InstructionSourceKind::Rule,
                scope_dir: workspace.clone(),
                path: rules_dir,
                exists: true,
                status: InstructionSourceStatus::Skipped,
                bytes: None,
                warning: Some("refusing symlinked rules directory".to_string()),
            });
            continue;
        }
        let Ok(dir_entries) = fs::read_dir(&rules_dir) else {
            continue;
        };
        let mut file_paths: Vec<PathBuf> = dir_entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "md") && context_candidate_exists(path)
            })
            .collect();
        file_paths.sort_by(|a, b| {
            a.file_name()
                .unwrap_or_default()
                .cmp(b.file_name().unwrap_or_default())
        });
        for (index, path) in file_paths.into_iter().enumerate() {
            let (status, warning) = if index >= MAX_RULES_FILES {
                (
                    InstructionSourceStatus::Skipped,
                    Some(format!(
                        "beyond the per-directory rules cap of {MAX_RULES_FILES} files"
                    )),
                )
            } else {
                match load_context_file_limited(&workspace, &path, settings.file_max_bytes()) {
                    Ok(_) => (InstructionSourceStatus::Loaded, None),
                    Err(error) => (InstructionSourceStatus::Skipped, Some(error.to_string())),
                }
            };
            sources.push(InstructionSourceInfo {
                kind: InstructionSourceKind::Rule,
                scope_dir: workspace.clone(),
                path: path.clone(),
                exists: true,
                status,
                bytes: file_len(&path),
                warning,
            });
        }
    }

    // User-level fallback layer.
    if let Some(home) = home_dir {
        for relative in legacy_global_whale_relative_paths() {
            let path = join_relative_components(home, relative);
            if context_candidate_exists(&path) {
                sources.push(InstructionSourceInfo {
                    kind: InstructionSourceKind::Ignored,
                    scope_dir: home.to_path_buf(),
                    path: path.clone(),
                    exists: true,
                    status: InstructionSourceStatus::Skipped,
                    bytes: file_len(&path),
                    warning: Some(WHALE_IGNORED_WARNING.to_string()),
                });
            }
        }
        let mut scope_loaded = false;
        for relative in global_context_relative_paths() {
            push_candidate_entry(
                &mut sources,
                InstructionSourceKind::Global,
                home,
                join_relative_components(home, relative),
                &mut scope_loaded,
                None,
                |path| load_global_context_file_limited(path, settings.file_max_bytes()),
            );
        }
        if settings.claude_fallback() || imports.is_enabled(ForeignInstructionFormat::Claude) {
            push_candidate_entry(
                &mut sources,
                InstructionSourceKind::Global,
                home,
                home.join(".claude/CLAUDE.md"),
                &mut scope_loaded,
                None,
                |path| load_global_context_file_limited(path, settings.file_max_bytes()),
            );
        }
    }

    // Opted-in foreign fragments — enumerate the declared candidates, then
    // mark the files the bounded loader's own selection walk picks.
    let fragment_candidates = fragment_candidates_for(imports);
    let fragment_loaded = |path: &Path| {
        codewhale_util::fs_confined::open_read(&workspace, path)
            .ok()
            .and_then(codewhale_core::fragments::read_project_instruction_file)
            .is_some()
    };
    let selected: std::collections::BTreeSet<PathBuf> =
        codewhale_core::fragments::selected_project_instruction_candidate_files(
            &workspace,
            &fragment_candidates,
        )
        .into_iter()
        .collect();
    let mut emitted = std::collections::BTreeSet::new();
    for candidate in &fragment_candidates {
        let path = workspace.join(candidate);
        if !path.exists() {
            sources.push(InstructionSourceInfo {
                kind: InstructionSourceKind::Fragment,
                scope_dir: workspace.clone(),
                path,
                exists: false,
                status: InstructionSourceStatus::Missing,
                bytes: None,
                warning: None,
            });
            continue;
        }
        // A directory candidate's loadable files are the selected entries
        // underneath it; report those rather than the directory itself.
        let members: Vec<PathBuf> = selected
            .iter()
            .filter(|file| file.starts_with(&path))
            .cloned()
            .collect();
        if path.is_dir() {
            if members.is_empty() {
                sources.push(InstructionSourceInfo {
                    kind: InstructionSourceKind::Fragment,
                    scope_dir: workspace.clone(),
                    path,
                    exists: true,
                    status: InstructionSourceStatus::Skipped,
                    bytes: None,
                    warning: Some(
                        "no loadable .md files selected under this directory".to_string(),
                    ),
                });
            }
            for file in members {
                emitted.insert(file.clone());
                let loaded = fragment_loaded(&file);
                sources.push(InstructionSourceInfo {
                    kind: InstructionSourceKind::Fragment,
                    scope_dir: workspace.clone(),
                    path: file.clone(),
                    exists: true,
                    status: if loaded {
                        InstructionSourceStatus::Loaded
                    } else {
                        InstructionSourceStatus::Skipped
                    },
                    bytes: file_len(&file),
                    warning: None,
                });
            }
        } else {
            emitted.insert(path.clone());
            let selected_file = selected.contains(&path);
            let loaded = selected_file && fragment_loaded(&path);
            sources.push(InstructionSourceInfo {
                kind: InstructionSourceKind::Fragment,
                scope_dir: workspace.clone(),
                path: path.clone(),
                exists: true,
                status: if loaded {
                    InstructionSourceStatus::Loaded
                } else if selected_file {
                    InstructionSourceStatus::Skipped
                } else {
                    // Exists but not selected — e.g. a symlink the walk refuses.
                    InstructionSourceStatus::Skipped
                },
                bytes: file_len(&path),
                warning: if selected_file && !loaded {
                    Some("selected but empty or unreadable".to_string())
                } else {
                    None
                },
            });
        }
    }
    // Selected files not attributed to a listed candidate (nested rules dir
    // members) still get reported.
    for file in selected.iter().filter(|file| !emitted.contains(*file)) {
        let loaded = fragment_loaded(file);
        sources.push(InstructionSourceInfo {
            kind: InstructionSourceKind::Fragment,
            scope_dir: workspace.clone(),
            path: file.clone(),
            exists: true,
            status: if loaded {
                InstructionSourceStatus::Loaded
            } else {
                InstructionSourceStatus::Skipped
            },
            bytes: file_len(file),
            warning: None,
        });
    }

    // Configured `instructions = [...]` files (#454) — resolved paths, in
    // declared order. The renderer skips missing/empty files with a warning.
    for path in configured {
        let exists = context_candidate_exists(path);
        let (status, warning) = if is_duplicate(path, &loaded) {
            (
                InstructionSourceStatus::Skipped,
                Some("already loaded by another instruction source".to_string()),
            )
        } else {
            match load_global_context_file_limited(path, settings.file_max_bytes()) {
                Ok(content) if !content.trim().is_empty() => {
                    if let Ok(canonical) = fs::canonicalize(path) {
                        loaded.push(canonical);
                    }
                    (InstructionSourceStatus::Loaded, None)
                }
                Ok(_) => (
                    InstructionSourceStatus::Skipped,
                    Some("empty instructions file".to_string()),
                ),
                Err(error) => (
                    InstructionSourceStatus::Skipped,
                    Some(format!("unreadable instructions file: {error}")),
                ),
            }
        };
        sources.push(InstructionSourceInfo {
            kind: InstructionSourceKind::Configured,
            scope_dir: workspace.clone(),
            path: path.clone(),
            exists,
            status,
            bytes: file_len(path),
            warning,
        });
    }

    // `.codewhale/constitution.json`, workspace → repository root. The loader
    // stops at the first existing candidate whether it parses or not, so
    // later candidates — even ones that exist — are never evaluated.
    let (_block, constitution_source, _warnings) = load_repo_constitution_block(&workspace);
    let mut seen_existing = false;
    for path in repo_constitution_candidate_paths(&workspace) {
        let exists = context_candidate_exists(&path);
        let status = if !exists {
            InstructionSourceStatus::Missing
        } else if seen_existing {
            InstructionSourceStatus::Shadowed
        } else {
            seen_existing = true;
            if constitution_source.as_deref() == Some(path.as_path()) {
                InstructionSourceStatus::Loaded
            } else {
                InstructionSourceStatus::Skipped
            }
        };
        sources.push(InstructionSourceInfo {
            kind: InstructionSourceKind::Constitution,
            scope_dir: workspace.clone(),
            path: path.clone(),
            exists,
            status,
            bytes: file_len(&path),
            warning: None,
        });
    }

    sources
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn confined_global_context_refuses_a_directory_target() {
        let home = tempdir().unwrap();
        let path = home.path().join("AGENTS.md");
        std::os::unix::fs::symlink("/", &path).unwrap();
        assert!(load_global_context_file(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn confined_context_refuses_linked_parent_directories() {
        for directory in [".codewhale", ".deepseek"] {
            let workspace = tempdir().unwrap();
            let outside = tempdir().unwrap();
            fs::write(
                outside.path().join("instructions.md"),
                "separate instructions",
            )
            .unwrap();
            fs::create_dir(outside.path().join("rules")).unwrap();
            fs::write(outside.path().join("rules/rule.md"), "separate rule").unwrap();
            fs::write(
                outside.path().join("constitution.json"),
                r#"{"purpose":"separate purpose"}"#,
            )
            .unwrap();
            std::os::unix::fs::symlink(outside.path(), workspace.path().join(directory)).unwrap();
            let context = load_project_context_with_imports(
                workspace.path(),
                &ForeignInstructionImports::none(),
            );
            assert!(context.instructions.is_none());
            assert!(context.rules_block.is_none());
            assert!(load_repo_constitution_block(workspace.path()).0.is_none());
        }
    }

    #[test]
    fn test_load_project_context_empty() {
        let tmp = tempdir().expect("tempdir");
        let ctx = load_project_context(tmp.path());

        assert!(!ctx.has_instructions());
        assert!(ctx.source_path.is_none());
    }

    #[test]
    fn test_load_project_context_agents_md() {
        let tmp = tempdir().expect("tempdir");
        let agents_path = tmp.path().join("AGENTS.md");
        fs::write(&agents_path, "# Test Instructions\n\nFollow these rules.").expect("write");

        let ctx = load_project_context(tmp.path());

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Test Instructions")
        );
        assert_eq!(ctx.source_path, Some(agents_path));
    }

    #[cfg(unix)]
    #[test]
    fn project_context_rejects_symlinked_agents_md() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        let outside_agents = outside.path().join("AGENTS.md");
        fs::write(&outside_agents, "outside instructions").expect("write outside agents");
        std::os::unix::fs::symlink(&outside_agents, workspace.path().join("AGENTS.md"))
            .expect("symlink agents");

        let ctx = load_project_context(workspace.path());

        assert!(
            !ctx.has_instructions(),
            "symlinked project instructions must not be loaded: {:?}",
            ctx.instructions
        );
        assert!(
            ctx.warnings.iter().any(|w| w.contains("symlinked")),
            "expected symlink warning, got {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn test_load_project_context_priority() {
        let tmp = tempdir().expect("tempdir");

        // Create both files - AGENTS.md should take priority
        fs::write(tmp.path().join("AGENTS.md"), "AGENTS content").expect("write");
        let claude_dir = tmp.path().join(".claude");
        fs::create_dir(&claude_dir).expect("mkdir");
        fs::write(claude_dir.join("instructions.md"), "CLAUDE content").expect("write");

        let ctx = load_project_context(tmp.path());

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("AGENTS content")
        );
    }

    #[test]
    fn test_load_project_context_hidden_dir() {
        let tmp = tempdir().expect("tempdir");
        let hidden_dir = tmp.path().join(".deepseek");
        fs::create_dir(&hidden_dir).expect("mkdir");
        fs::write(hidden_dir.join("instructions.md"), "Hidden instructions").expect("write");

        let ctx = load_project_context(tmp.path());

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Hidden instructions")
        );
    }

    #[test]
    fn test_as_system_block() {
        let tmp = tempdir().expect("tempdir");
        let agents_path = tmp.path().join("AGENTS.md");
        fs::write(&agents_path, "Test content").expect("write");

        let ctx = load_project_context(tmp.path());
        let block = ctx.as_system_block().expect("block");

        assert!(block.contains("<project_instructions"));
        assert!(block.contains("Test content"));
        assert!(block.contains("</project_instructions>"));
    }

    #[test]
    fn project_instructions_source_label_falls_back_to_project() {
        use crate::project_context::project_instructions_source_label;
        assert_eq!(
            project_instructions_source_label(None),
            "project",
            "a missing source path keeps the historical literal label"
        );
        assert_eq!(
            project_instructions_source_label(Some(std::path::Path::new("/etc/AGENTS.md"))),
            "AGENTS.md"
        );
    }

    #[test]
    fn project_instructions_source_is_file_name_not_absolute_path() {
        // The source label sits inside the pinned system prompt: loading the
        // same AGENTS.md from a different directory must leave the
        // instructions block byte-identical, so a move emits no spurious
        // `<context_update>` history append and no absolute path enters a
        // provider-bound label. Scope note: ancestor-chain and project-rule
        // labels are repo-relative for the same reason; this test pins the
        // file-name spelling of the workspace-level block.
        let dir_a = tempdir().expect("tempdir a");
        let dir_b = tempdir().expect("tempdir b");
        fs::write(dir_a.path().join("AGENTS.md"), "Pinned content").expect("write a");
        fs::write(dir_b.path().join("AGENTS.md"), "Pinned content").expect("write b");

        let block_a = load_project_context(dir_a.path())
            .as_system_block()
            .expect("block a");
        let block_b = load_project_context(dir_b.path())
            .as_system_block()
            .expect("block b");
        assert_eq!(
            block_a, block_b,
            "a directory move must not change the project instructions block"
        );
        assert!(block_a.contains("source=\"AGENTS.md\""));
        assert!(
            !block_a.contains(&dir_a.path().display().to_string()),
            "absolute paths must not enter prompt source labels"
        );
    }

    #[test]
    fn repo_relative_source_label_falls_back_when_path_is_outside_root() {
        use crate::project_context::repo_relative_source_label;
        use std::path::Path;
        // The absolute fallback is an escape hatch for a path the loader
        // cannot place under the checkout — unreachable by construction —
        // so it must degrade to a displayable spelling, never panic or
        // invent a wrong relative label.
        assert_eq!(
            repo_relative_source_label(
                Path::new("/repo/crates/tui/AGENTS.md"),
                Some(Path::new("/repo"))
            ),
            "crates/tui/AGENTS.md"
        );
        // On unix the backslashes are literal filename bytes, on Windows they
        // are separators — either way the label must come out forward-slashed,
        // and this assertion is the only check that can see it off Windows.
        assert_eq!(
            repo_relative_source_label(
                Path::new("/repo/crates\\tui\\AGENTS.md"),
                Some(Path::new("/repo"))
            ),
            "crates/tui/AGENTS.md",
            "backslash separators are normalized to forward slashes"
        );
        assert_eq!(
            repo_relative_source_label(Path::new("/elsewhere/AGENTS.md"), Some(Path::new("/repo"))),
            "/elsewhere/AGENTS.md",
            "a path outside the root falls back to the absolute spelling"
        );
        assert_eq!(
            repo_relative_source_label(Path::new("/repo/AGENTS.md"), None),
            "/repo/AGENTS.md",
            "a missing root falls back to the absolute spelling"
        );
    }

    #[test]
    fn chain_segment_labels_are_repo_relative_not_absolute() {
        let tmp = tempdir().expect("tempdir");
        let home = tempdir().expect("home tempdir");

        // A two-level chain inside one checkout: the repo root carries the
        // wide scope, `crates/tui` the workspace scope.
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        fs::write(repo.join("AGENTS.md"), "ROOT-SCOPE instructions").expect("write root agents");

        let workspace = repo.join("crates").join("tui");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        fs::write(workspace.join("AGENTS.md"), "WORKSPACE-SCOPE instructions")
            .expect("write workspace agents");

        let ctx = load_project_context_with_parents_and_home(&workspace, Some(home.path()));
        let instructions = ctx.instructions.as_ref().expect("chain instructions");

        assert!(instructions.contains("ROOT-SCOPE instructions"));
        assert!(instructions.contains("WORKSPACE-SCOPE instructions"));
        assert!(
            instructions.contains("<!-- scoped instructions: crates/tui/AGENTS.md"),
            "the chain segment label must be repo-relative, got: {instructions}"
        );
        assert!(
            !instructions.contains(&tmp.path().display().to_string()),
            "absolute paths must not enter chain segment labels: {instructions}"
        );
    }

    #[test]
    fn project_rule_labels_are_repo_relative_not_absolute() {
        let tmp = tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        let rules_dir = repo.join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(
            rules_dir.join("security.md"),
            "# Security\nNo hardcoded secrets.",
        )
        .expect("write rule");

        let ctx = load_project_context(&repo);
        let rules = ctx.rules_block.as_ref().expect("rules block");

        assert!(
            rules.contains("<project_rule source=\".codewhale/rules/security.md\">"),
            "the rule label must be repo-relative, got: {rules}"
        );
        assert!(
            !rules.contains(&tmp.path().display().to_string()),
            "absolute paths must not enter rule source labels: {rules}"
        );
    }

    #[test]
    fn prompt_block_is_pinned_across_checkout_moves() {
        // The whole reason for repo-relative labels: the same repository
        // tree checked out at two different absolute locations must render
        // byte-identical system blocks (chain segment + rules included), so
        // a move or recase neither appends a spurious `<context_update>`
        // history entry nor invalidates the provider's prompt KV prefix.
        let mut moved_block: Option<String> = None;
        for location in ["checkout-a", "checkout-b"] {
            let root = tempdir().expect(location);
            let repo = root.path().join("repo");
            fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
            fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n")
                .expect("write HEAD");
            fs::write(repo.join("AGENTS.md"), "ROOT-SCOPE instructions").expect("write root");

            let workspace = repo.join("nested");
            fs::create_dir_all(&workspace).expect("mkdir nested");
            fs::write(workspace.join("AGENTS.md"), "NESTED-SCOPE instructions")
                .expect("write nested");
            // Rules are discovered at workspace level, so the checkout tree
            // must carry them under the session's workspace too.
            let rules_dir = workspace.join(".codewhale/rules");
            fs::create_dir_all(&rules_dir).expect("mkdir rules");
            fs::write(rules_dir.join("security.md"), "No hardcoded secrets.").expect("write rule");

            let ctx = load_project_context_with_parents_and_home(&workspace, None);
            let block = ctx.as_system_block().expect("system block");

            assert!(block.contains("<!-- scoped instructions: nested/AGENTS.md"));
            assert!(
                block.contains("<project_rule source=\"nested/.codewhale/rules/security.md\">"),
                "rule label must be repo-relative, got: {block}"
            );
            assert!(
                !block.contains(&root.path().display().to_string()),
                "absolute paths must not enter the pinned prompt block: {block}"
            );
            if location == "checkout-a" {
                moved_block = Some(block);
            } else {
                assert_eq!(
                    moved_block.as_deref(),
                    Some(block.as_str()),
                    "the same tree at two locations must render byte-identical blocks"
                );
            }
        }
    }

    #[test]
    fn project_rule_labels_fall_back_to_workspace_relative_without_git_root() {
        // Outside any repository there is no root to relativize against;
        // the label falls back to workspace-relative spelling, which stays
        // stable as long as the workspace tree itself is unchanged.
        let tmp = tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(rules_dir.join("security.md"), "No hardcoded secrets.").expect("write rule");

        let ctx = load_project_context(tmp.path());
        let rules = ctx.rules_block.as_ref().expect("rules block");

        assert!(
            rules.contains("<project_rule source=\".codewhale/rules/security.md\">"),
            "the rule label must fall back to workspace-relative, got: {rules}"
        );
        assert!(
            !rules.contains(&tmp.path().display().to_string()),
            "absolute paths must not enter rule source labels: {rules}"
        );
    }

    #[test]
    fn test_empty_file_warning() {
        let tmp = tempdir().expect("tempdir");
        let agents_path = tmp.path().join("AGENTS.md");
        fs::write(&agents_path, "   \n  \n  ").expect("write"); // Only whitespace

        let ctx = load_project_context(tmp.path());

        assert!(!ctx.has_instructions());
        assert!(!ctx.warnings.is_empty());
    }

    #[test]
    fn test_check_trust_status() {
        let tmp = tempdir().expect("tempdir");

        // Not trusted by default
        assert!(!check_trust_status(tmp.path()));

        // Create trust marker
        let deepseek_dir = tmp.path().join(".deepseek");
        fs::create_dir(&deepseek_dir).expect("mkdir");
        fs::write(deepseek_dir.join("trusted"), "").expect("write");

        assert!(check_trust_status(tmp.path()));
    }

    #[test]
    fn test_create_default_agents_md() {
        let tmp = tempdir().expect("tempdir");
        let path = create_default_agents_md(tmp.path()).expect("create");

        assert!(path.exists());
        let content = fs::read_to_string(&path).expect("read");
        assert!(content.contains("Project Agent Instructions"));
    }

    #[test]
    fn test_load_with_parents() {
        let tmp = tempdir().expect("tempdir");
        let home = tempdir().expect("home tempdir");

        // Create a nested structure
        let subdir = tmp.path().join("subproject");
        fs::create_dir(&subdir).expect("mkdir");

        // Put AGENTS.md in parent
        fs::write(tmp.path().join("AGENTS.md"), "Parent instructions").expect("write");
        // Also create a real .git marker to make the parent the repo root
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).expect("mkdir .git");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");

        // Load from subdir should find parent's AGENTS.md
        let ctx = load_project_context_with_parents_and_home(&subdir, Some(home.path()));

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Parent instructions")
        );
    }

    #[test]
    fn parent_search_stops_at_the_repository_root() {
        let tmp = tempdir().expect("tempdir");
        let home = tempdir().expect("home tempdir");

        // AGENTS.md exists above the repository root. It belongs to another
        // scope (often another checkout) and must not leak into this one.
        fs::write(tmp.path().join("AGENTS.md"), "Organization instructions").expect("write");

        // Mark repository root one level below.
        let repo_root = tmp.path().join("repo");
        fs::create_dir(&repo_root).expect("mkdir repo");
        let git_dir = repo_root.join(".git");
        fs::create_dir(&git_dir).expect("mkdir .git");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");

        let workspace = repo_root.join("apps").join("client");
        fs::create_dir_all(&workspace).expect("mkdir workspace");

        let ctx = load_project_context_with_parents_and_home(&workspace, Some(home.path()));
        assert!(
            !ctx.instructions
                .as_deref()
                .is_some_and(|text| text.contains("Organization instructions")),
            "instruction files above the repository root must not be loaded: {:?}",
            ctx.instructions
        );
        assert_eq!(
            ctx.source_path, None,
            "no in-repo instruction file exists, so no source may be claimed"
        );
        assert!(
            !project_context_cache_candidate_paths(&workspace, Some(home.path()))
                .iter()
                .any(|(candidate, _)| candidate == &tmp.path().join("AGENTS.md")),
            "cache candidates must respect the repository boundary too"
        );
    }

    #[test]
    fn instruction_chain_assembles_worktree_root_to_cwd_in_order_within_budget() {
        let tmp = tempdir().expect("tempdir");
        let home = tempdir().expect("home tempdir");

        // A main checkout with its own AGENTS.md — it must stay out of the
        // nested worktree's instruction chain.
        let main = tmp.path().join("main-checkout");
        fs::create_dir_all(main.join(".git")).expect("mkdir main .git");
        fs::write(main.join(".git").join("HEAD"), "ref: refs/heads/main\n")
            .expect("write main HEAD");
        fs::write(main.join("AGENTS.md"), "MAIN-CHECKOUT-ONLY instructions")
            .expect("write main agents");

        // A linked worktree: `.git` is a `gitdir:` pointer file, so the
        // worktree is its own repository root for instruction assembly.
        let lane = tmp.path().join("worktrees").join("lane");
        fs::create_dir_all(&lane).expect("mkdir lane");
        fs::write(
            lane.join(".git"),
            format!("gitdir: {}/.git/worktrees/lane\n", main.display()),
        )
        .expect("write lane gitdir pointer");
        fs::write(lane.join("AGENTS.md"), "WORKTREE-ROOT instructions").expect("write lane agents");

        let nested = lane.join("crates").join("tui");
        fs::create_dir_all(&nested).expect("mkdir nested");
        fs::write(nested.join("AGENTS.md"), "NESTED-DIR instructions")
            .expect("write nested agents");

        let ctx = load_project_context_with_parents_and_home(&nested, Some(home.path()));

        let instructions = ctx.instructions.as_deref().unwrap_or("");
        assert!(
            instructions.contains("WORKTREE-ROOT instructions")
                && instructions.contains("NESTED-DIR instructions"),
            "worktree root and nested AGENTS.md must both assemble:\n{instructions}"
        );
        let root_at = instructions
            .find("WORKTREE-ROOT instructions")
            .expect("root");
        let nested_at = instructions
            .find("NESTED-DIR instructions")
            .expect("nested");
        assert!(
            root_at < nested_at,
            "repository root must read before the current directory (root={root_at}, nested={nested_at})"
        );
        assert!(
            !instructions.contains("MAIN-CHECKOUT-ONLY instructions"),
            "the main checkout's file is outside the worktree's chain:\n{instructions}"
        );
        let expected_source = fs::canonicalize(&nested)
            .expect("canonicalize nested")
            .join("AGENTS.md");
        assert_eq!(
            ctx.source_path.as_deref(),
            Some(expected_source.as_path()),
            "source_path points at the most specific (current-directory) file"
        );
    }

    #[test]
    fn directory_outside_any_repository_loads_no_instruction_chain() {
        let tmp = tempdir().expect("tempdir");
        let home = tempdir().expect("home tempdir");

        // No `.git` anywhere: the parent's AGENTS.md is outside any chain.
        fs::write(
            tmp.path().join("AGENTS.md"),
            "PARENT-OUTSIDE-REPO instructions",
        )
        .expect("write parent agents");
        let child = tmp.path().join("project");
        fs::create_dir_all(&child).expect("mkdir child");
        fs::write(child.join("AGENTS.md"), "CHILD-ONLY instructions").expect("write child agents");

        let ctx = load_project_context_with_parents_and_home(&child, Some(home.path()));

        let instructions = ctx.instructions.as_deref().unwrap_or("");
        assert!(
            instructions.contains("CHILD-ONLY instructions"),
            "the workspace's own file still loads outside a repository:\n{instructions}"
        );
        assert!(
            !instructions.contains("PARENT-OUTSIDE-REPO instructions"),
            "no ancestor chain may be assembled outside a repository:\n{instructions}"
        );
        let expected_source = fs::canonicalize(&child)
            .expect("canonicalize child")
            .join("AGENTS.md");
        assert_eq!(ctx.source_path.as_deref(), Some(expected_source.as_path()));
    }

    #[test]
    fn agents_md_used_while_whale_md_is_ignored() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("AGENTS.md"), "AGENTS canonical").expect("write agents");
        fs::write(tmp.path().join("WHALE.md"), "WHALE legacy").expect("write whale");

        let ctx = load_project_context(tmp.path());
        let instructions = ctx.instructions.expect("instructions loaded");
        assert!(instructions.contains("AGENTS canonical"), "{instructions}");
        assert!(!instructions.contains("WHALE legacy"), "{instructions}");
        assert!(
            ctx.warnings
                .iter()
                .any(|w| w.contains("WHALE.md is ignored")),
            "{:?}",
            ctx.warnings
        );
    }

    #[test]
    fn whale_md_alone_is_ignored_with_migration_warning() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("WHALE.md"), "WHALE legacy body").expect("write whale");

        let ctx = load_project_context(tmp.path());
        assert!(
            ctx.instructions.is_none(),
            "legacy WHALE.md must not be read"
        );
        assert!(
            ctx.warnings
                .iter()
                .any(|w| w.contains("WHALE.md is ignored")),
            "expected ignored-file warning, got {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn constitution_json_renders_authority_block() {
        let tmp = tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        fs::create_dir(tmp.path().join(".codewhale")).expect("mkdir .codewhale");
        fs::write(
            tmp.path().join(".codewhale").join("constitution.json"),
            r#"{
                "schema_version": 1,
                "authority": ["current user request", "live code and tests", "AGENTS.md"],
                "protected_invariants": ["keep the tool-catalog head byte-stable"],
                "branch_policy": "Start from live branch truth; open PRs into main",
                "verification_policy": { "before_claiming_done": ["run focused tests"] },
                "escalate_when": ["a destructive action was not authorized"]
            }"#,
        )
        .expect("write constitution");

        let ctx = load_project_context_with_parents(tmp.path());
        let block = ctx
            .constitution_block
            .as_deref()
            .expect("constitution block rendered");
        assert!(block.contains("<codewhale_repo_constitution"));
        // Same origin-label convention as project_instructions: file name
        // only, no absolute path in the provider-bound label (the locator
        // stays available via constitution_source_path and /constitution).
        assert!(block.contains("source=\"constitution.json\""));
        assert!(
            !block.contains(&tmp.path().display().to_string()),
            "the constitution prompt label must not carry the absolute path"
        );
        assert!(block.contains("current user request"));
        assert!(block.contains("run focused tests"));
        assert!(block.contains("keep the tool-catalog head byte-stable"));
        assert!(block.contains("Start from live branch truth"));
        assert!(block.contains("a destructive action was not authorized"));
        assert!(block.contains("WHALE.md is ignored and should be migrated"));
        assert!(
            ctx.constitution_source_path
                .as_ref()
                .is_some_and(|path| path.ends_with(".codewhale/constitution.json")),
            "constitution source path should be visible: {:?}",
            ctx.constitution_source_path
        );
        // It also surfaces through the system block.
        assert!(
            ctx.as_system_block()
                .expect("system block")
                .contains("codewhale_repo_constitution")
        );
    }

    #[test]
    fn stale_constitution_branch_policy_warns() {
        let tmp = tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        fs::create_dir(tmp.path().join(".codewhale")).expect("mkdir .codewhale");
        fs::write(
            tmp.path().join(".codewhale").join("constitution.json"),
            r#"{
                "schema_version": 1,
                "authority": ["current user request"],
                "branch_policy": "v0.8.53 work targets the codex/v0.8.53 integration branch, not main"
            }"#,
        )
        .expect("write constitution");

        let ctx = load_project_context_with_parents(tmp.path());
        assert!(
            ctx.constitution_block.is_some(),
            "stale policy should warn but still render"
        );
        assert!(
            ctx.warnings
                .iter()
                .any(|warning| warning.contains("branch_policy appears stale")),
            "expected stale branch_policy warning, got {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn malformed_constitution_warns_without_crashing() {
        let tmp = tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        fs::create_dir(tmp.path().join(".codewhale")).expect("mkdir .codewhale");
        fs::write(
            tmp.path().join(".codewhale").join("constitution.json"),
            "{ not valid json",
        )
        .expect("write bad constitution");

        let ctx = load_project_context_with_parents(tmp.path());
        assert!(
            ctx.constitution_block.is_none(),
            "no block for invalid JSON"
        );
        assert!(
            ctx.warnings.iter().any(|w| w.contains("Failed to parse")),
            "expected parse warning, got {:?}",
            ctx.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn constitution_json_rejects_symlinked_file() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        fs::create_dir(workspace.path().join(".git")).expect("mkdir .git");
        fs::create_dir(workspace.path().join(".codewhale")).expect("mkdir .codewhale");
        let outside_constitution = outside.path().join("constitution.json");
        fs::write(
            &outside_constitution,
            r#"{"schema_version":1,"authority":["outside authority"]}"#,
        )
        .expect("write outside constitution");
        std::os::unix::fs::symlink(
            &outside_constitution,
            workspace
                .path()
                .join(".codewhale")
                .join("constitution.json"),
        )
        .expect("symlink constitution");

        let ctx =
            load_project_context_with_parents_and_home(workspace.path(), Some(outside.path()));

        assert!(
            ctx.constitution_block.is_none(),
            "symlinked constitution must not be loaded: {:?}",
            ctx.constitution_block
        );
        assert!(
            !ctx.as_system_block()
                .unwrap_or_default()
                .contains("outside authority"),
            "symlink target content must not reach the system block"
        );
        assert!(
            ctx.warnings.iter().any(|w| w.contains("symlinked")),
            "expected symlink warning, got {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn generated_context_is_bounded_and_ephemeral_for_many_file_workspace() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let noisy = workspace.path().join("aaa-many-files");
        fs::create_dir_all(&noisy).expect("mkdir noisy");
        for i in 0..1000 {
            fs::write(noisy.join(format!("file-{i:04}.rs")), "fn noisy() {}").expect("write noisy");
        }
        fs::create_dir_all(workspace.path().join("zzz-important")).expect("mkdir important");
        fs::write(
            workspace.path().join("zzz-important").join("main.rs"),
            "fn important() {}",
        )
        .expect("write important");

        // Boundedness is a structural contract below; wall-clock time depends
        // on host load and is not a reliable assertion in the full suite.
        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));
        assert!(ctx.has_instructions());

        let generated_path = workspace.path().join(".codewhale").join("instructions.md");
        assert_eq!(ctx.source_path, None);
        assert!(
            !generated_path.exists(),
            "generated project context should stay ephemeral"
        );
        assert!(
            !workspace.path().join(".codewhale").exists(),
            "loading context should not create a .codewhale directory"
        );
        let generated = ctx.instructions.as_ref().expect("generated instructions");
        assert!(generated.contains("Project Context (Auto-generated, ephemeral)"));
        assert!(generated.contains("Bounded Project Overview"));
        assert!(!generated.contains("<project_context_pack>"));
        assert!(
            generated.contains("\"zzz-important/\""),
            "later top-level project areas should remain visible:\n{generated}"
        );
        let noisy_count = generated.matches("aaa-many-files/file-").count();
        assert!(
            noisy_count < 300,
            "generated context should not list the whole noisy directory; saw {noisy_count}"
        );
        assert!(
            !generated.contains("file-0999.rs"),
            "bounded context should omit the tail of the noisy directory"
        );
    }

    #[test]
    fn explicit_home_bounds_parent_search_without_process_environment() {
        let home = tempdir().expect("home tempdir");
        fs::write(
            home.path().join("AGENTS.md"),
            "must not be loaded as project context",
        )
        .expect("write home AGENTS.md");
        let workspace = home.path().join("projects").join("demo");
        fs::create_dir_all(&workspace).expect("mkdir workspace");

        let ctx = load_project_context_with_parents_and_home(&workspace, Some(home.path()));

        assert_eq!(
            ctx.source_path, None,
            "the explicit home is the parent-search boundary, not project context"
        );
        assert!(
            ctx.instructions
                .as_deref()
                .is_some_and(|text| text.contains("Project Context (Auto-generated, ephemeral)")),
            "expected generated context, got {:?}",
            ctx.instructions
        );
        assert!(
            project_context_cache_candidate_paths(&workspace, Some(home.path()))
                .iter()
                .all(|(candidate, _)| candidate != &home.path().join("AGENTS.md")),
            "cache candidates must use the same explicit parent-search boundary"
        );
    }

    #[test]
    fn cached_context_reflects_overwritten_agents_md() {
        crate::project_context_cache::clear();
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let agents = workspace.path().join("AGENTS.md");
        fs::write(&agents, "alpha").expect("write alpha");

        let first =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));
        assert!(
            first
                .instructions
                .as_deref()
                .is_some_and(|s| s.contains("alpha")),
            "expected alpha instructions: {:?}",
            first.instructions
        );

        fs::write(&agents, "bravo").expect("write bravo");
        let second =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));

        assert!(
            second
                .instructions
                .as_deref()
                .is_some_and(|s| s.contains("bravo")),
            "cache must invalidate on same-length content overwrite: {:?}",
            second.instructions
        );
    }

    #[test]
    fn cached_context_reflects_constitution_json_change() {
        crate::project_context_cache::clear();
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        fs::create_dir(workspace.path().join(".git")).expect("mkdir git");
        fs::create_dir(workspace.path().join(".codewhale")).expect("mkdir codewhale");
        let constitution = workspace
            .path()
            .join(".codewhale")
            .join("constitution.json");
        fs::write(
            &constitution,
            r#"{"schema_version":1,"authority":["alpha authority"]}"#,
        )
        .expect("write alpha constitution");

        let first =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));
        assert!(
            first
                .constitution_block
                .as_deref()
                .is_some_and(|s| s.contains("alpha authority")),
            "expected alpha constitution block: {:?}",
            first.constitution_block
        );

        fs::write(
            &constitution,
            r#"{"schema_version":1,"authority":["bravo authority"]}"#,
        )
        .expect("write bravo constitution");
        let second =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));

        assert!(
            second
                .constitution_block
                .as_deref()
                .is_some_and(|s| s.contains("bravo authority")),
            "cache must invalidate when constitution changes: {:?}",
            second.constitution_block
        );
    }

    #[test]
    fn cached_generated_context_stays_ephemeral() {
        crate::project_context_cache::clear();
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");

        let first =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));
        assert!(first.has_instructions());
        let generated_path = workspace.path().join(".codewhale").join("instructions.md");
        assert!(
            !generated_path.exists(),
            "first load should not write generated instructions"
        );

        let second =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));
        assert!(second.has_instructions());
        assert!(
            !generated_path.exists(),
            "cached generated context should remain in memory-only state"
        );
    }

    #[test]
    fn cached_context_reflects_trust_marker_created() {
        crate::project_context_cache::clear();
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        fs::write(workspace.path().join("AGENTS.md"), "instructions").expect("write agents");

        let first =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));
        assert!(!first.is_trusted);

        let trust_dir = workspace.path().join(".deepseek");
        fs::create_dir(&trust_dir).expect("mkdir trust dir");
        fs::write(trust_dir.join("trusted"), "").expect("write trust marker");

        let second =
            load_project_context_with_parents_cached_and_home(workspace.path(), Some(home.path()));
        assert!(
            second.is_trusted,
            "cache must invalidate when trust marker appears"
        );
    }

    #[test]
    fn test_load_global_agents_when_project_has_no_context() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let global_dir = home.path().join(".deepseek");
        fs::create_dir(&global_dir).expect("mkdir .deepseek");
        let global_agents = global_dir.join("AGENTS.md");
        fs::write(&global_agents, "Global instructions").expect("write global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Global instructions")
        );
        assert_eq!(ctx.source_path, Some(global_agents));
    }

    #[test]
    fn test_load_global_agents_falls_back_to_vendor_neutral_path() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let global_dir = home.path().join(".agents");
        fs::create_dir(&global_dir).expect("mkdir .agents");
        let global_agents = global_dir.join("AGENTS.md");
        fs::write(&global_agents, "Vendor-neutral instructions").expect("write global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Vendor-neutral instructions")
        );
        assert_eq!(ctx.source_path, Some(global_agents));
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_global_agents_is_followed() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let shared = home.path().join("AGENTS.md");
        fs::write(&shared, "Shared global instructions").expect("write shared agents");
        fs::hard_link(&shared, home.path().join("shared-copy.md"))
            .expect("hard link global agents");
        let global_dir = home.path().join(".deepseek");
        fs::create_dir(&global_dir).expect("mkdir .deepseek");
        let link = global_dir.join("AGENTS.md");
        std::os::unix::fs::symlink(&shared, &link).expect("symlink global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Shared global instructions"),
            "a symlinked user-level AGENTS.md must be read: {:?}",
            ctx.warnings
        );
        assert_eq!(ctx.source_path, Some(link));
        assert!(
            !ctx.warnings.iter().any(|w| w.contains("symlink")),
            "following the user-level link must not warn: {:?}",
            ctx.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_workspace_agents_is_still_refused() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let outside = tempdir().expect("outside tempdir");
        let secret = outside.path().join("secret.md");
        fs::write(&secret, "outside content").expect("write outside file");
        std::os::unix::fs::symlink(&secret, workspace.path().join("AGENTS.md"))
            .expect("symlink workspace agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(
            ctx.instructions.is_none()
                || !ctx
                    .instructions
                    .as_ref()
                    .unwrap()
                    .contains("outside content"),
            "a workspace AGENTS.md symlink must not be followed"
        );
    }

    #[test]
    fn test_codewhale_specific_path_wins_over_agents_path() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");

        let codewhale_dir = home.path().join(".codewhale");
        fs::create_dir(&codewhale_dir).expect("mkdir .codewhale");
        let codewhale_agents = codewhale_dir.join("AGENTS.md");
        fs::write(&codewhale_agents, "Codewhale-specific instructions")
            .expect("write codewhale agents");

        let agents_dir = home.path().join(".agents");
        fs::create_dir(&agents_dir).expect("mkdir .agents");
        fs::write(agents_dir.join("AGENTS.md"), "Vendor-neutral instructions")
            .expect("write vendor-neutral agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        let instructions = ctx.instructions.as_ref().unwrap();
        assert!(
            instructions.contains("Codewhale-specific instructions"),
            "Codewhale-specific global file should win:\n{instructions}"
        );
        assert!(
            !instructions.contains("Vendor-neutral instructions"),
            "lower-priority .agents file should be skipped:\n{instructions}"
        );
        assert_eq!(ctx.source_path, Some(codewhale_agents));
    }

    #[test]
    fn test_global_agents_wins_over_global_whale_across_paths() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");

        let codewhale_dir = home.path().join(".codewhale");
        fs::create_dir(&codewhale_dir).expect("mkdir .codewhale");
        fs::write(codewhale_dir.join("WHALE.md"), "Global WHALE legacy")
            .expect("write codewhale whale");

        let agents_dir = home.path().join(".agents");
        fs::create_dir(&agents_dir).expect("mkdir .agents");
        let global_agents = agents_dir.join("AGENTS.md");
        fs::write(&global_agents, "Global AGENTS canonical").expect("write global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        let instructions = ctx.instructions.as_ref().unwrap();
        assert!(
            instructions.contains("Global AGENTS canonical"),
            "global AGENTS.md should win:\n{instructions}"
        );
        assert!(
            !instructions.contains("Global WHALE legacy"),
            "global WHALE.md content should be skipped when any global AGENTS.md exists:\n{instructions}"
        );
        assert!(
            ctx.warnings
                .iter()
                .any(|warning| warning.contains("WHALE.md is ignored")),
            "ignored WHALE.md should emit migration warning: {:?}",
            ctx.warnings
        );
        assert_eq!(ctx.source_path, Some(global_agents));
    }

    #[test]
    fn test_global_whale_is_ignored_when_no_global_agents_exists() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");

        let codewhale_dir = home.path().join(".codewhale");
        fs::create_dir(&codewhale_dir).expect("mkdir .codewhale");
        let global_whale = codewhale_dir.join("WHALE.md");
        fs::write(&global_whale, "Global WHALE legacy").expect("write codewhale whale");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        let instructions = ctx.instructions.as_deref().unwrap_or("");
        assert!(
            !instructions.contains("Global WHALE legacy"),
            "legacy WHALE.md must not be read when no global AGENTS.md exists:\n{instructions}"
        );
        assert!(
            ctx.warnings
                .iter()
                .any(|warning| warning.contains("WHALE.md is ignored")),
            "expected global WHALE.md ignored warning, got {:?}",
            ctx.warnings
        );
        assert_ne!(ctx.source_path, Some(global_whale));
    }

    #[test]
    fn test_global_instructions_md_is_autoloaded_while_whale_is_ignored() {
        // #3012: a global ~/.codewhale/instructions.md should be auto-loaded as
        // a fallback context layer while legacy WHALE.md remains ignored.
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");

        let codewhale_dir = home.path().join(".codewhale");
        fs::create_dir(&codewhale_dir).expect("mkdir .codewhale");
        fs::write(codewhale_dir.join("WHALE.md"), "Global WHALE legacy")
            .expect("write codewhale whale");
        let global_instructions = codewhale_dir.join("instructions.md");
        fs::write(&global_instructions, "Global instructions body")
            .expect("write global instructions");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        let instructions = ctx.instructions.as_ref().unwrap();
        assert!(
            instructions.contains("Global instructions body"),
            "global instructions.md should be auto-loaded:\n{instructions}"
        );
        assert!(
            !instructions.contains("Global WHALE legacy"),
            "instructions.md should load without reading ignored WHALE.md:\n{instructions}"
        );
        assert!(
            ctx.warnings
                .iter()
                .any(|warning| warning.contains("WHALE.md is ignored")),
            "ignored WHALE.md should emit migration warning: {:?}",
            ctx.warnings
        );
        assert_eq!(ctx.source_path, Some(global_instructions));
    }

    #[test]
    fn test_global_agents_outranks_global_instructions() {
        // #3012 precedence: AGENTS.md > instructions.md.
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");

        let codewhale_dir = home.path().join(".codewhale");
        fs::create_dir(&codewhale_dir).expect("mkdir .codewhale");
        let global_agents = codewhale_dir.join("AGENTS.md");
        fs::write(&global_agents, "Global AGENTS canonical").expect("write global agents");
        fs::write(
            codewhale_dir.join("instructions.md"),
            "Global instructions body",
        )
        .expect("write global instructions");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        let instructions = ctx.instructions.as_ref().unwrap();
        assert!(
            instructions.contains("Global AGENTS canonical"),
            "global AGENTS.md should outrank instructions.md:\n{instructions}"
        );
        assert!(
            !instructions.contains("Global instructions body"),
            "instructions.md should be skipped when a global AGENTS.md exists:\n{instructions}"
        );
        assert_eq!(ctx.source_path, Some(global_agents));
    }

    #[test]
    fn test_local_and_global_agents_merge_when_both_exist() {
        // #1157: when both `~/.deepseek/AGENTS.md` and a project AGENTS.md
        // exist, the prompt should carry user-wide preferences AND the
        // project's overrides — not silently drop the global file.
        let workspace = tempdir().expect("workspace tempdir");
        fs::write(workspace.path().join("AGENTS.md"), "Local instructions")
            .expect("write local agents");

        let home = tempdir().expect("home tempdir");
        let global_dir = home.path().join(".deepseek");
        fs::create_dir(&global_dir).expect("mkdir .deepseek");
        fs::write(global_dir.join("AGENTS.md"), "Global instructions")
            .expect("write global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        let instructions = ctx.instructions.as_ref().unwrap();
        assert!(
            instructions.contains("Global instructions"),
            "global block missing from merged instructions:\n{instructions}"
        );
        assert!(
            instructions.contains("Local instructions"),
            "project block missing from merged instructions:\n{instructions}"
        );
        // Global block precedes the project block so project rules read
        // last and win "last word" precedence with the model.
        let global_at = instructions.find("Global instructions").unwrap();
        let local_at = instructions.find("Local instructions").unwrap();
        assert!(
            global_at < local_at,
            "global block must come before project block, got global={global_at} local={local_at}"
        );
        // The merged block is labelled so the model can tell the layers
        // apart when it needs to explain which rule it followed.
        assert!(
            instructions.contains("project (overrides global where they conflict)"),
            "expected labelled separator between global and project blocks"
        );
        // `source_path` keeps pointing at the more-specific file so the
        // user knows where to edit the workspace-level override.
        assert_eq!(
            ctx.source_path,
            Some(canonicalize_workspace_or_keep(workspace.path()).join("AGENTS.md"))
        );
    }

    #[test]
    fn test_global_agents_only_no_project_unchanged_fallback() {
        // Sanity: when only the global file exists, the historical
        // fallback behaviour is preserved — no merge framing leaks in.
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let global_dir = home.path().join(".deepseek");
        fs::create_dir(&global_dir).expect("mkdir .deepseek");
        let global_agents = global_dir.join("AGENTS.md");
        fs::write(&global_agents, "Just the global instructions").expect("write global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(ctx.has_instructions());
        let instructions = ctx.instructions.as_ref().unwrap();
        assert!(instructions.contains("Just the global instructions"));
        assert!(
            !instructions.contains("project (overrides global"),
            "merge-framing label should not appear when there's nothing to merge"
        );
        assert_eq!(ctx.source_path, Some(global_agents));
    }

    #[test]
    fn test_invalid_global_agents_warns_and_falls_back_to_generated_context() {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let global_dir = home.path().join(".deepseek");
        fs::create_dir(&global_dir).expect("mkdir .deepseek");
        fs::write(global_dir.join("AGENTS.md"), "   \n  ").expect("write empty global agents");

        let ctx = load_project_context_with_parents_and_home(workspace.path(), Some(home.path()));

        assert!(
            ctx.warnings
                .iter()
                .any(|warning| warning.contains("Context file") && warning.contains("is empty")),
            "expected empty global AGENTS.md warning, got {:?}",
            ctx.warnings
        );
        assert!(ctx.has_instructions());
        assert!(
            ctx.instructions
                .as_ref()
                .unwrap()
                .contains("Project Context (Auto-generated, ephemeral)")
        );
    }

    // ── Rules directory auto-discovery tests ──

    #[test]
    fn rules_from_codewhale_dir_are_loaded_as_project_context() {
        let tmp = tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(
            rules_dir.join("security.md"),
            "# Security\nNo hardcoded secrets.",
        )
        .expect("write");

        let ctx = load_project_context(tmp.path());

        let rules = ctx.rules_block.as_ref().expect("rules_block should be set");
        assert!(
            rules.contains("Security"),
            "expected rules content, got: {rules}"
        );
        assert!(
            rules.contains("<project_rule source="),
            "expected <project_rule> wrapper, got: {rules}"
        );
    }

    #[test]
    fn rules_are_loaded_in_filename_order() {
        let tmp = tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(rules_dir.join("zzz.md"), "last").expect("write");
        fs::write(rules_dir.join("aaa.md"), "first").expect("write");
        fs::write(rules_dir.join("mmm.md"), "middle").expect("write");

        let ctx = load_project_context(tmp.path());
        let rules = ctx.rules_block.as_ref().unwrap();

        let pos_aaa = rules.find("first").unwrap();
        let pos_mmm = rules.find("middle").unwrap();
        let pos_zzz = rules.find("last").unwrap();
        assert!(pos_aaa < pos_mmm, "aaa should come before mmm");
        assert!(pos_mmm < pos_zzz, "mmm should come before zzz");
    }

    #[test]
    fn claude_rules_load_only_when_explicitly_imported() {
        let tmp = tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".claude/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(rules_dir.join("style.md"), "Use tabs").expect("write");

        // Default: another tool's rules directory is not Codewhale's law.
        let ctx = load_project_context_with_imports(tmp.path(), &ForeignInstructionImports::none());
        assert!(
            ctx.rules_block.is_none(),
            "an un-imported .claude/rules must contribute nothing: {:?}",
            ctx.rules_block
        );
        assert!(
            ctx.warnings
                .iter()
                .any(|w| w.contains("claude") && w.contains("project_instruction_imports")),
            "the user must be told the files exist and how to import them: {:?}",
            ctx.warnings
        );

        // Opted in by name: loaded, and ranked after Codewhale's own.
        let (imports, unknown) = ForeignInstructionImports::from_config(&["claude".to_string()]);
        assert!(unknown.is_empty());
        let ctx = load_project_context_with_imports(tmp.path(), &imports);
        let rules = ctx.rules_block.as_ref().expect("rules should be loaded");
        assert!(rules.contains("Use tabs"), "expected .claude/rules/ import");
    }

    #[test]
    fn fragment_backed_foreign_instructions_warn_until_imported() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join(".cursorrules"), "Cursor-only law").expect("write cursor");
        let copilot_dir = tmp.path().join(".github");
        fs::create_dir_all(&copilot_dir).expect("mkdir github");
        fs::write(
            copilot_dir.join("copilot-instructions.md"),
            "Copilot-only law",
        )
        .expect("write copilot");

        let ctx = load_project_context_with_imports(tmp.path(), &ForeignInstructionImports::none());
        let warning = ctx
            .warnings
            .iter()
            .find(|warning| warning.contains("project_instruction_imports"))
            .expect("foreign fragment warning");
        assert!(warning.contains("cursor"), "{warning}");
        assert!(warning.contains("copilot"), "{warning}");

        let (imports, unknown) =
            ForeignInstructionImports::from_config(&["cursor".to_string(), "copilot".to_string()]);
        assert!(unknown.is_empty());
        let ctx = load_project_context_with_imports(tmp.path(), &imports);
        assert!(
            ctx.warnings
                .iter()
                .all(|warning| !warning.contains("project_instruction_imports")),
            "opted-in formats must not keep warning: {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn unusable_foreign_fragments_do_not_claim_importable_instructions() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join(".cursorrules"), "   \n").expect("write empty cursor file");
        let cursor_rules = tmp.path().join(".cursor/rules");
        fs::create_dir_all(&cursor_rules).expect("mkdir cursor rules");
        fs::write(cursor_rules.join("settings.json"), "{}").expect("write non-markdown file");

        let ctx = load_project_context_with_imports(tmp.path(), &ForeignInstructionImports::none());
        assert!(
            ctx.warnings
                .iter()
                .all(|warning| !warning.contains("project_instruction_imports")),
            "empty and non-Markdown fragments are not importable: {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn unusable_direct_foreign_instructions_do_not_claim_importable_content() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("CLAUDE.md"), "\n\t ").expect("write empty claude file");
        let claude_rules = tmp.path().join(".claude/rules");
        fs::create_dir_all(&claude_rules).expect("mkdir claude rules");
        fs::write(claude_rules.join("settings.json"), "{}").expect("write non-markdown file");

        let ctx = load_project_context_with_imports(tmp.path(), &ForeignInstructionImports::none());
        assert!(
            ctx.warnings
                .iter()
                .all(|warning| !warning.contains("project_instruction_imports")),
            "empty files and rules directories without Markdown are not importable: {:?}",
            ctx.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_foreign_fragment_does_not_claim_importable_instructions() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside tempdir");
        let outside_claude = outside.path().join("CLAUDE.md");
        fs::write(&outside_claude, "outside Claude law").expect("write outside Claude file");
        let outside_rules = outside.path().join("rules");
        fs::create_dir_all(&outside_rules).expect("mkdir outside rules");
        fs::write(outside_rules.join("law.md"), "outside law").expect("write outside rule");
        fs::create_dir_all(tmp.path().join(".cursor")).expect("mkdir cursor");
        fs::create_dir_all(tmp.path().join(".claude")).expect("mkdir claude");
        symlink(&outside_rules, tmp.path().join(".cursor/rules")).expect("symlink cursor rules");
        symlink(&outside_rules, tmp.path().join(".claude/rules")).expect("symlink claude rules");
        symlink(&outside_claude, tmp.path().join("CLAUDE.md")).expect("symlink Claude file");

        let ctx = load_project_context_with_imports(tmp.path(), &ForeignInstructionImports::none());
        assert!(
            ctx.warnings
                .iter()
                .all(|warning| !warning.contains("project_instruction_imports")),
            "the bounded loader rejects symlinked foreign fragments: {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn foreign_instruction_imports_parse_names_and_report_typos() {
        let (imports, unknown) = ForeignInstructionImports::from_config(&[]);
        assert!(imports.is_empty(), "default imports nothing");
        assert!(unknown.is_empty());

        let (imports, unknown) = ForeignInstructionImports::from_config(&[
            "Claude".to_string(),
            " cursor ".to_string(),
            "clawed".to_string(),
        ]);
        assert!(imports.is_enabled(ForeignInstructionFormat::Claude));
        assert!(imports.is_enabled(ForeignInstructionFormat::Cursor));
        assert!(!imports.is_enabled(ForeignInstructionFormat::Gemini));
        assert_eq!(unknown, vec!["clawed".to_string()], "typos are reported");

        let (all, _) = ForeignInstructionImports::from_config(&["all".to_string()]);
        for format in ForeignInstructionFormat::ALL {
            assert!(
                all.is_enabled(*format),
                "{} missing from `all`",
                format.key()
            );
        }
        assert_eq!(all.keys().len(), ForeignInstructionFormat::ALL.len());
    }

    #[test]
    fn codewhale_instructions_outrank_an_imported_claude_file() {
        // On the previous default list CLAUDE.md sat at rank 3 and
        // .codewhale/instructions.md at rank 4, so another tool's file won.
        let tmp = tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".codewhale")).expect("mkdir codewhale");
        fs::write(
            tmp.path().join(".codewhale/instructions.md"),
            "CODEWHALE-OWN",
        )
        .expect("write codewhale");
        fs::write(tmp.path().join("CLAUDE.md"), "CLAUDE-FILE").expect("write claude");

        let (imports, _) = ForeignInstructionImports::from_config(&["claude".to_string()]);
        let files = context_files_for(&imports);
        let cw = files
            .iter()
            .position(|f| *f == ".codewhale/instructions.md")
            .expect("codewhale file in list");
        let cl = files
            .iter()
            .position(|f| *f == "CLAUDE.md")
            .expect("claude file in list");
        assert!(
            cl > cw,
            "Codewhale's own instruction file must outrank an imported CLAUDE.md: {files:?}"
        );
    }

    #[test]
    fn rules_directory_missing_does_not_crash() {
        let tmp = tempdir().expect("tempdir");
        // No .codewhale/rules/ or .claude/rules/ directories exist
        let ctx = load_project_context(tmp.path());
        // Rules block should be None when no rules directories exist
        assert!(
            ctx.rules_block.is_none(),
            "rules_block should be None when no rules exist"
        );
    }

    #[test]
    fn rules_coexist_with_agents_md() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("AGENTS.md"), "Main project instructions").expect("write");
        let rules_dir = tmp.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(rules_dir.join("extra.md"), "Extra rule").expect("write");

        let ctx = load_project_context(tmp.path());
        let instructions = ctx.instructions.as_ref().unwrap();
        let rules = ctx.rules_block.as_ref().unwrap();

        assert!(
            instructions.contains("Main project instructions"),
            "AGENTS.md content missing"
        );
        assert!(rules.contains("Extra rule"), "rules content missing");
        // AGENTS.md should come first in system block
        let block = ctx.as_system_block().unwrap();
        let pos_agents = block.find("Main project instructions").unwrap();
        let pos_rule = block.find("Extra rule").unwrap();
        assert!(pos_agents < pos_rule, "AGENTS.md should precede rules");
    }

    #[test]
    fn non_md_files_in_rules_dir_are_ignored() {
        let tmp = tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");
        fs::write(rules_dir.join("notes.txt"), "should be ignored").expect("write");
        fs::write(rules_dir.join("valid.md"), "loaded").expect("write");

        let ctx = load_project_context(tmp.path());
        let rules = ctx.rules_block.as_ref().unwrap();

        assert!(rules.contains("loaded"), "valid .md should be loaded");
        assert!(
            !rules.contains("should be ignored"),
            ".txt should be ignored"
        );
    }

    #[test]
    fn rules_cap_truncates_excess_files() {
        let tmp = tempdir().expect("tempdir");
        let rules_dir = tmp.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");

        // Create more files than the cap
        for i in 0..60 {
            fs::write(
                rules_dir.join(format!("rule_{i:04}.md")),
                format!("content {i}"),
            )
            .expect("write");
        }

        let ctx = load_project_context(tmp.path());
        let rules = ctx.rules_block.as_ref().unwrap();

        // The last file (by sorted name) should NOT be present
        assert!(
            !rules.contains("content 59"),
            "rule_0059 should be above cap"
        );
        // The first file should be present
        assert!(
            rules.contains("content 0"),
            "rule_0000 should be within cap"
        );
        // Count <project_rule> blocks
        let count = rules.matches("<project_rule source=").count();
        assert_eq!(
            count, MAX_RULES_FILES,
            "exactly {MAX_RULES_FILES} rules should be loaded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rules_rejects_symlinked_files() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        let rules_dir = workspace.path().join(".codewhale/rules");
        fs::create_dir_all(&rules_dir).expect("mkdir rules");

        let outside_rule = outside.path().join("outside.md");
        fs::write(&outside_rule, "outside content").expect("write outside");
        std::os::unix::fs::symlink(&outside_rule, rules_dir.join("outside.md"))
            .expect("symlink rule");

        let ctx = load_project_context(workspace.path());

        // Symlinked rules must not be loaded
        assert!(
            ctx.rules_block.is_none()
                || !ctx
                    .rules_block
                    .as_ref()
                    .unwrap()
                    .contains("outside content"),
            "symlinked rules must not be loaded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rules_rejects_symlinked_directory() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        let outside_dir = outside.path().join("real_rules");
        fs::create_dir_all(&outside_dir).expect("mkdir outside dir");
        fs::write(outside_dir.join("secret.md"), "outside content").expect("write outside");
        fs::create_dir_all(workspace.path().join(".codewhale")).expect("mkdir codewhale");

        // Symlink the directory itself, not individual files
        std::os::unix::fs::symlink(&outside_dir, workspace.path().join(".codewhale/rules"))
            .expect("symlink rules dir");

        let ctx = load_project_context(workspace.path());

        // Symlinked rules directory must be refused at the directory level
        assert!(
            ctx.rules_block.is_none()
                || !ctx
                    .rules_block
                    .as_ref()
                    .unwrap()
                    .contains("outside content"),
            "symlinked rules directory must be refused"
        );
    }

    #[test]
    fn rules_from_both_dirs_are_loaded_together() {
        let tmp = tempdir().expect("tempdir");
        let codewhale_rules = tmp.path().join(".codewhale/rules");
        let claude_rules = tmp.path().join(".claude/rules");
        fs::create_dir_all(&codewhale_rules).expect("mkdir codewhale rules");
        fs::create_dir_all(&claude_rules).expect("mkdir claude rules");
        fs::write(codewhale_rules.join("cw.md"), "codewhale-rule").expect("write");
        fs::write(claude_rules.join("claude.md"), "claude-rule").expect("write");

        let (imports, _) = ForeignInstructionImports::from_config(&["claude".to_string()]);
        let ctx = load_project_context_with_imports(tmp.path(), &imports);
        let rules = ctx.rules_block.as_ref().unwrap();

        assert!(
            rules.contains("codewhale-rule"),
            ".codewhale/rules/ should be loaded"
        );
        assert!(
            rules.contains("claude-rule"),
            "an imported .claude/rules/ should be loaded"
        );
        // .codewhale/rules/ content should precede an imported foreign dir
        let pos_cw = rules.find("codewhale-rule").unwrap();
        let pos_claude = rules.find("claude-rule").unwrap();
        assert!(
            pos_cw < pos_claude,
            ".codewhale/rules/ should precede .claude/rules/"
        );
    }

    #[test]
    fn instruction_sources_report_precedence_shadowing_and_ignored_files() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).expect("mkdir git");
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("head");
        fs::write(root.join("AGENTS.md"), "primary\n").expect("agents");
        fs::create_dir_all(root.join(".codewhale")).expect("mkdir codewhale");
        fs::write(
            root.join(".codewhale").join("instructions.md"),
            "secondary\n",
        )
        .expect("workspace instructions");
        fs::write(root.join("WHALE.md"), "deprecated\n").expect("whale");
        fs::create_dir_all(root.join(".codewhale").join("rules")).expect("mkdir rules");
        fs::write(
            root.join(".codewhale").join("rules").join("style.md"),
            "keep it small\n",
        )
        .expect("rule");

        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("mkdir home");
        let configured = vec![root.join("team").join("extra.md")];
        fs::create_dir_all(root.join("team")).expect("mkdir team");
        fs::write(root.join("team").join("extra.md"), "configured\n").expect("configured");

        let sources = project_instruction_sources(root, Some(&home), &configured);
        let find = |path_suffix: &str| {
            sources
                .iter()
                .find(|source| source.path.ends_with(path_suffix))
                .unwrap_or_else(|| panic!("missing entry for {path_suffix}: {sources:?}"))
        };

        let agents = find("AGENTS.md");
        assert_eq!(agents.kind, InstructionSourceKind::Project);
        assert_eq!(agents.status, InstructionSourceStatus::Loaded);
        assert_eq!(agents.bytes, Some(8));

        // The lower-priority candidate in the same scope exists but is
        // shadowed, not loaded.
        let secondary = find(".codewhale/instructions.md");
        assert_eq!(secondary.status, InstructionSourceStatus::Shadowed);

        let whale = find("WHALE.md");
        assert_eq!(whale.kind, InstructionSourceKind::Ignored);
        assert_eq!(whale.status, InstructionSourceStatus::Skipped);
        assert!(whale.warning.is_some());

        let rule = find("style.md");
        assert_eq!(rule.kind, InstructionSourceKind::Rule);
        assert_eq!(rule.status, InstructionSourceStatus::Loaded);

        let configured_entry = find("extra.md");
        assert_eq!(configured_entry.kind, InstructionSourceKind::Configured);
        assert_eq!(configured_entry.status, InstructionSourceStatus::Loaded);

        // Unchecked-but-real candidates are enumerated so clients can render
        // the full precedence map.
        assert!(
            sources
                .iter()
                .any(|s| s.status == InstructionSourceStatus::Missing),
            "missing candidates must appear: {sources:?}"
        );
        // Global candidates under the (empty) home are reported as missing.
        assert!(
            sources
                .iter()
                .any(|s| s.kind == InstructionSourceKind::Global
                    && s.status == InstructionSourceStatus::Missing),
            "global candidates must appear: {sources:?}"
        );
    }

    #[test]
    fn instruction_sources_mark_unreadable_winner_as_skipped_not_loaded() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).expect("mkdir git");
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("head");
        // An empty AGENTS.md fails the load check, so the next candidate wins.
        fs::write(root.join("AGENTS.md"), "   \n").expect("empty agents");
        fs::create_dir_all(root.join(".codewhale")).expect("mkdir codewhale");
        fs::write(root.join(".codewhale").join("instructions.md"), "wins\n")
            .expect("workspace instructions");

        let sources = project_instruction_sources(root, None, &[]);
        let agents = sources
            .iter()
            .find(|s| s.path.ends_with("AGENTS.md"))
            .expect("agents entry");
        assert_eq!(agents.status, InstructionSourceStatus::Skipped);
        assert!(agents.warning.is_some(), "refusal must carry a warning");

        let winner = sources
            .iter()
            .find(|s| s.path.ends_with(".codewhale/instructions.md"))
            .expect("instructions entry");
        assert_eq!(winner.status, InstructionSourceStatus::Loaded);
    }
}
