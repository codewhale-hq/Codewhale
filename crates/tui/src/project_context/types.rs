//! Shared project-context types: the load-error enum and the
//! `ProjectContext` value that carries loaded instructions, rules, and the
//! rendered repo-constitution block into the system prompt.

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::budget::{InstructionNotice, InstructionSegment};

// === Errors ===

#[derive(Debug, Error)]
pub(crate) enum ProjectContextError {
    #[error("Failed to read context metadata for {path}: {source}")]
    Metadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Context file {path} is too large ({size} bytes, max {max})")]
    TooLarge {
        path: PathBuf,
        size: u64,
        max: usize,
    },
    #[error("Failed to read context file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Context file {path} is empty")]
    Empty { path: PathBuf },
}

impl ProjectContextError {
    /// Machine-readable reason, so a skip can name the limit that produced it.
    pub(crate) fn kind(&self) -> super::budget::InstructionLimit {
        match self {
            Self::TooLarge { .. } => super::budget::InstructionLimit::FileCeiling,
            Self::Empty { .. } => super::budget::InstructionLimit::EmptyFile,
            Self::Metadata { .. } | Self::Read { .. } => super::budget::InstructionLimit::None,
        }
    }

    pub(crate) fn detail(&self) -> Option<super::budget::InstructionDetail> {
        match self {
            Self::TooLarge { max, .. } => Some(super::budget::InstructionDetail::OverFileCeiling {
                max_bytes: *max as u64,
            }),
            Self::Empty { .. } => None,
            Self::Metadata { source, .. } | Self::Read { source, .. } => {
                Some(super::budget::InstructionDetail::unreadable(source))
            }
        }
    }
}

/// Result of loading project context
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// The loaded instructions content.
    ///
    /// This is the rendered join of [`Self::instruction_segments`] — global
    /// scope first, then the project chain — and is rebuilt after every
    /// budget trim, so a consumer that only reads the joined string always
    /// sees post-budget content.
    pub instructions: Option<String>,
    /// Raw instruction content kept per source, untrimmed.
    ///
    /// Segment-level trimming needs to know where one file ends and the next
    /// begins; a flattened string cannot answer that. The cached project
    /// context is therefore stored raw and trimmed per request, so a context
    /// window change never invalidates the file-keyed cache and a content
    /// change never reuses trimmed text.
    pub instruction_segments: Vec<InstructionSegment>,
    /// Every instruction source that was trimmed, skipped, or refused while
    /// loading or budgeting, for the startup notice and `/context report`.
    pub notices: Vec<InstructionNotice>,
    /// Auto-discovered rules from `.codewhale/rules/` / `.claude/rules/`.
    /// Kept separate from `instructions` so rules alone don't block
    /// parent-directory AGENTS.md discovery via `has_instructions()`.
    pub rules_block: Option<String>,
    /// Path to the loaded file (for display)
    pub source_path: Option<PathBuf>,
    /// Any warnings during loading
    pub warnings: Vec<String>,
    /// Rendered `.codewhale/constitution.json` authority block, if present.
    /// Codewhale-specific repo authority/prioritization policy — distinct from
    /// the cross-agent prose in `instructions`.
    pub constitution_block: Option<String>,
    /// Path to the repo constitution file that produced `constitution_block`.
    pub constitution_source_path: Option<PathBuf>,
    /// Project root directory
    #[allow(dead_code)] // Part of ProjectContext public interface
    pub project_root: PathBuf,
    /// Whether this is a trusted project
    pub is_trusted: bool,
}

impl ProjectContext {
    /// Create an empty project context
    pub fn empty(project_root: PathBuf) -> Self {
        Self {
            instructions: None,
            instruction_segments: Vec::new(),
            notices: Vec::new(),
            rules_block: None,
            source_path: None,
            warnings: Vec::new(),
            constitution_block: None,
            constitution_source_path: None,
            project_root,
            is_trusted: false,
        }
    }

    /// Check if any instructions were loaded
    pub fn has_instructions(&self) -> bool {
        self.instructions.is_some()
    }

    /// Rebuild [`Self::instructions`] from the current segments.
    ///
    /// Called after assembly and after every trim. Global scope renders
    /// first so the workspace keeps the last word, exactly as the single
    /// merged block did before segments existed.
    pub(crate) fn rebuild_joined_instructions(&mut self) {
        self.instructions = super::render_joined_instructions(&self.instruction_segments);
    }

    /// Get the instructions as a formatted block for system prompt.
    ///
    /// The Codewhale repo constitution (`.codewhale/constitution.json`), when
    /// present, is emitted first as a higher-authority block, followed by the
    /// cross-agent `<project_instructions>` prose. Either may be absent.
    pub fn as_system_block(&self) -> Option<String> {
        let instructions_block = self.instructions.as_ref().map(|content| {
            let source = project_instructions_source_label(self.source_path.as_deref());

            let mut block = format!(
                "<project_instructions source=\"{source}\">\n{content}\n</project_instructions>"
            );
            // Append rules after instructions, inside the same logical block.
            // Rules are kept separate from `instructions` so they don't block
            // parent-directory AGENTS.md discovery via `has_instructions()`.
            if let Some(rules) = &self.rules_block {
                block.push('\n');
                block.push_str(rules);
            }
            block
        });

        match (self.constitution_block.as_ref(), instructions_block) {
            (Some(constitution), Some(instructions)) => {
                Some(format!("{constitution}\n\n{instructions}"))
            }
            (Some(constitution), None) => {
                // Constitution present but no main instructions — still emit rules if any
                if let Some(rules) = &self.rules_block {
                    Some(format!("{constitution}\n\n{rules}"))
                } else {
                    Some(constitution.clone())
                }
            }
            (None, Some(instructions)) => Some(instructions),
            (None, None) => {
                // No main instructions, but rules may exist on their own
                self.rules_block.clone()
            }
        }
    }
}

/// The `source` label for `<project_instructions>` (and repo constitution)
/// blocks: the context file's name only, never its absolute path. The label
/// sits inside the pinned system prompt, so keeping it stable across
/// directory moves and recasings means an unchanged file does not emit a
/// spurious `<context_update>` history append after a move, and absolute
/// project paths stay out of provider-bound prompt labels. Directory
/// identity stays discoverable via the shell; the label names the origin,
/// it is not a locator.
pub(crate) fn project_instructions_source_label(source_path: Option<&Path>) -> String {
    source_path
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string())
}

/// The `source` label for prompt blocks that must keep sibling scopes
/// distinguishable (ancestor instruction-chain segments, `<project_rule>`
/// blocks): the file's path relative to `root` — the containing checkout —
/// rendered with forward slashes. When `root` is `None` or `path` is not
/// inside it, falls back to the absolute spelling rather than guessing.
///
/// Like [`project_instructions_source_label`], the label sits inside the
/// pinned system prompt, so keeping it stable across directory moves and
/// recasings means an unchanged file does not emit a spurious
/// `<context_update>` history append after a move, and absolute project
/// paths stay out of provider-bound prompt labels. Root-relative spelling
/// (rather than the bare file name) is deliberate: chain segments and rule
/// files legitimately share basenames across scopes, and the label exists
/// precisely to disambiguate them.
pub(crate) fn repo_relative_source_label(path: &Path, root: Option<&Path>) -> String {
    if let Some(root) = root
        && let Ok(relative) = path.strip_prefix(root)
    {
        return relative.to_string_lossy().replace('\\', "/");
    }
    path.display().to_string()
}

/// Merge multiple project contexts (e.g., from nested directories)
#[allow(dead_code)] // Public API for monorepo context merging
pub fn merge_contexts(contexts: &[ProjectContext]) -> Option<String> {
    let non_empty: Vec<_> = contexts
        .iter()
        .filter_map(ProjectContext::as_system_block)
        .collect();

    if non_empty.is_empty() {
        None
    } else {
        Some(non_empty.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_contexts() {
        let mut ctx1 = ProjectContext::empty(PathBuf::from("/a"));
        ctx1.instructions = Some("Instructions A".to_string());
        ctx1.source_path = Some(PathBuf::from("/a/AGENTS.md"));

        let mut ctx2 = ProjectContext::empty(PathBuf::from("/b"));
        ctx2.instructions = Some("Instructions B".to_string());
        ctx2.source_path = Some(PathBuf::from("/b/AGENTS.md"));

        let merged = merge_contexts(&[ctx1, ctx2]).expect("merge");

        assert!(merged.contains("Instructions A"));
        assert!(merged.contains("Instructions B"));
    }
}
