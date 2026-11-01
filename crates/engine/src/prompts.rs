#![allow(dead_code)]
//! System prompt composition.
//!
//! Prompts are assembled from composable layers loaded at compile time from
//! the single [`text`] module:
//!   constitution → `message[0]` (byte-stable).
//!   approval policy → request-time runtime metadata.
//! Tool availability comes only from the per-turn model catalog.
//!
//! Keeping every layer's text in one module makes prompt tuning a
//! single-file operation.

use codewhale_config::AppMode;
use codewhale_models::{SystemBlock, SystemPrompt, Tool};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub mod base_preview;
pub mod text;

#[derive(Debug, Clone)]
pub struct PromptSessionContext<'a> {
    pub user_memory_block: Option<&'a str>,
    pub goal_objective: Option<&'a str>,
    pub project_context_pack_enabled: bool,
    /// Resolved BCP-47 locale tag for the `## Environment` block in
    /// the system prompt (e.g. `"en"`, `"zh-Hans"`, `"ja"`). The
    /// caller is responsible for resolving this from `Settings`; no
    /// disk I/O happens inside the prompt builder, so the workspace-
    /// static portion of the system prompt stays cache-friendly.
    pub locale_tag: &'a str,
    /// When true, a ## Language Output Requirement block is appended
    /// to the system prompt instructing the model to respond in
    /// the resolved session locale.
    pub translation_enabled: bool,
    /// Active model identifier. The bundled constitution is model-agnostic,
    /// but embedders may still provide a prompt override containing
    /// `{model_id}`. Defaults to `"codewhale"` when the caller doesn't supply one.
    pub model_id: &'a str,
    /// Route-effective context window, when known. Prompt composition no
    /// longer prints context-window facts, but the field remains part of the
    /// session context contract for embedders and future runtime metadata.
    pub context_window_override: Option<u32>,
    /// Optional output-verbosity mode. `concise` appends a short output
    /// discipline block; unset keeps the normal conversational prompt.
    pub verbosity: Option<&'a str>,
    /// One-line notice that a prior session in this workspace left a
    /// recovery checkpoint (#5715). KV effect: frozen-prefix contributor —
    /// computed at engine construction and stable for the session; absent
    /// entirely when no interrupted session exists, so clean sessions share
    /// the same prefix bytes.
    pub recovery_hint: Option<&'a str>,
    /// Session-pinned skill discovery, bundle policy and ambient index limits.
    pub skills_config: std::sync::Arc<codewhale_runtime::config::SkillsConfig>,
    /// Immutable plugin snapshot owned by this App/Engine workspace context.
    /// Never sourced from process-global mutable state.
    pub plugin_registry: Option<&'a codewhale_plugins::PluginRegistry>,
    /// Active runtime mode. Retained in the session contract for embedders;
    /// bundled prompt text deliberately ignores it because policy and the live
    /// tool catalog already express the mode.
    pub mode: AppMode,
}

impl Default for PromptSessionContext<'_> {
    fn default() -> Self {
        Self {
            user_memory_block: None,
            goal_objective: None,
            project_context_pack_enabled: false,
            locale_tag: "en",
            translation_enabled: false,
            model_id: "codewhale",
            context_window_override: None,
            verbosity: None,
            recovery_hint: None,
            skills_config: Default::default(),
            plugin_registry: None,
            mode: AppMode::Agent,
        }
    }
}

/// Conventional location for the structured session relay artifact (#32).
/// A previous session writes it on exit / `/compact`; the next session reads
/// it back on startup and prepends it to the system prompt so a fresh agent
/// doesn't have to re-discover open blockers from scratch.
pub const HANDOFF_RELATIVE_PATH: &str = ".codewhale/handoff.md";
/// Legacy handoff path for reading from existing installs.
const LEGACY_HANDOFF_RELATIVE_PATH: &str = ".deepseek/handoff.md";

/// The relay artifact a new session reads: the primary path when a file
/// exists there, otherwise the legacy fallback. Returns the file to read and
/// its workspace-relative name, so every caller names the path actually read.
pub fn resolve_handoff_path(workspace: &Path) -> (PathBuf, &'static str) {
    let primary = workspace.join(HANDOFF_RELATIVE_PATH);
    if primary.exists() {
        (primary, HANDOFF_RELATIVE_PATH)
    } else {
        (
            workspace.join(LEGACY_HANDOFF_RELATIVE_PATH),
            LEGACY_HANDOFF_RELATIVE_PATH,
        )
    }
}

/// Per-file size cap for the workspace relay artifact (`/relay`, compaction).
///
/// Deliberately separate from the instruction-file ceiling: the relay is a
/// workspace-writable checkpoint with its own trim marker, and
/// `[instruction_budget] policy = "refuse"` must not change how the relay is
/// read.
const RELAY_ARTIFACT_MAX_BYTES: usize = 100 * 1024;

fn read_prompt_file_bounded(path: &Path, cap: usize) -> std::io::Result<(String, usize)> {
    read_prompt_text_bounded(std::fs::File::open(path)?, cap)
}

/// Read at most `cap` bytes of a prompt file plus slack for the leading
/// whitespace the renderers trim, so an oversized file is never read whole
/// into memory just to be truncated. Returns the text and the file's full
/// length (for the truncation note). A file that is not UTF-8 within the read
/// window is an error, as a whole-file `read_to_string` would report; only a
/// character cut by the window's end is dropped.
pub fn read_prompt_text_bounded(
    file: std::fs::File,
    cap: usize,
) -> std::io::Result<(String, usize)> {
    use std::io::Read as _;

    const LEADING_WHITESPACE_SLACK: usize = 4 * 1024;
    let full_len = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
    let mut bytes = Vec::new();
    file.take((cap + LEADING_WHITESPACE_SLACK) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() < full_len;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            // Only a multi-byte character split by the read window is
            // forgiven; invalid bytes anywhere else still fail the read.
            if !truncated || bytes.len() - valid > 3 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                ));
            }
            bytes.truncate(valid);
            String::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
        }
    };
    Ok((text, full_len))
}

/// Cap `trimmed` at `cap` bytes on a character boundary, noting what was
/// omitted out of `total` (the file length when the read itself was bounded).
pub fn cap_prompt_text(trimmed: &str, total: usize, cap: usize, hint: &str) -> String {
    if trimmed.len() <= cap && total <= trimmed.len() {
        return trimmed.to_string();
    }
    let head_end = (0..=cap.min(trimmed.len()))
        .rev()
        .find(|&i| trimmed.is_char_boundary(i))
        .unwrap_or(0);
    let total = total.max(trimmed.len());
    format!(
        "{}\n[…truncated: {} of {} bytes omitted — {hint}]",
        &trimmed[..head_end],
        total - head_end,
        total
    )
}

/// System prompt block appended when `translation_enabled` is true.
/// Instructs the model to respond in the resolved session locale for all
/// natural-language output — explanations, summaries, conversation.
/// Code identifiers, untranslatable technical terms, and explicitly
/// requested English code blocks are exempt.
fn translation_output_instruction(locale_tag: &str) -> String {
    let target_language = translation_target_language_for_tag(locale_tag);
    format!(
        "\
## Language Output Requirement\n\
\n\
The user requires all responses in {target_language}. \
Always respond in {target_language} — use natural, professional language for all \
explanations, code comments, summaries, and conversational turns. \
Only output English for:\n\
- Code identifiers (variable names, function names, file paths)\n\
- Technical terms that lack a standard translation in {target_language}\n\
- Code blocks the user explicitly requests in English\n\n\
This is a hard display requirement: the user does not read English, \
so any English prose in your response will block their decision-making."
    )
}

fn concise_output_discipline_instruction() -> &'static str {
    "\
## Concise Output Discipline

To minimize token usage and optimize speed:
- Output only direct, actionable code, technical steps, or final answers.
- Eliminate all conversational filler, fluff, introductions, transitions, or summarizing conclusions.
- Do NOT explain what you are about to do or what you have just completed.
- Do NOT provide conversational status updates before or after running tools.
- Keep explanations and comments extremely brief and technical, explaining only non-obvious reasoning."
}

fn is_concise_verbosity(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("concise"))
}

fn translation_target_language_for_tag(locale_tag: &str) -> &'static str {
    let normalized = locale_tag.trim().to_ascii_lowercase();
    if normalized.starts_with("ja") {
        "Japanese (日本語)"
    } else if normalized.starts_with("zh-hant")
        || normalized.contains("-tw")
        || normalized.contains("-hk")
        || normalized.contains("-mo")
    {
        "Traditional Chinese (繁體中文)"
    } else if normalized.starts_with("zh") {
        "Simplified Chinese (简体中文)"
    } else if normalized.starts_with("pt") {
        "Brazilian Portuguese (Português do Brasil)"
    } else if normalized.starts_with("es") {
        "Latin American Spanish (Español latinoamericano)"
    } else if normalized.starts_with("vi") {
        "Vietnamese (Tiếng Việt)"
    } else if normalized.starts_with("ko") {
        "Korean (한국어)"
    } else if normalized.starts_with("ca") {
        "Catalan (Català)"
    } else if normalized.starts_with("de") {
        "German (Deutsch)"
    } else if normalized.starts_with("fr") {
        "French (Français)"
    } else if normalized.starts_with("id") {
        "Indonesian (Bahasa Indonesia)"
    } else if normalized.starts_with("hi") {
        "Hindi (हिन्दी)"
    } else if normalized.starts_with("ru") {
        "Russian (Русский)"
    } else if normalized.starts_with("uk") {
        "Ukrainian (Українська)"
    } else {
        "English"
    }
}

/// Render a `## Environment` block listing the resolved locale tag and the
/// actionable host facts that affect command syntax.
///
/// The block is appended to the workspace-static portion of the system
/// prompt (after the shared constitution + project context, before configured
/// instructions / skills). `locale_tag` is resolved by the caller from
/// `Settings` so this function stays I/O-free.
///
/// `platform` and `shell` remain because they change how commands must be
/// written and are stable for the life of the process. The release version was
/// removed by the turn-meta diet: it is telemetry the model cannot act on and
/// churned the otherwise-static prefix on every release. The live workspace
/// path is delivered per-turn via `<turn_meta>` (see `turn_metadata_block`).
pub fn render_environment_block(_workspace: &Path, locale_tag: &str) -> String {
    let (platform, shell) = environment_host_facts();
    format_environment_block(locale_tag, &platform, &shell, npm_launcher_fact())
}

/// Format the `## Environment` block from already-resolved facts. Pure, so
/// the plain and npm-launcher shapes can both be tested on any host.
fn format_environment_block(
    locale_tag: &str,
    platform: &str,
    shell: &str,
    launcher: Option<&str>,
) -> String {
    let mut block = format!(
        "## Environment\n\
         \n\
         - lang: {locale_tag}\n\
         - platform: {platform}\n\
         - shell: {shell}"
    );
    if let Some(launcher) = launcher {
        block.push_str("\n- launcher: ");
        block.push_str(launcher);
    }
    block
}

/// On Windows an npm install keeps the launcher's `node.exe` as this
/// process's parent for the whole session, and Windows ends `codewhale.exe`
/// with it (#6827). Like `platform` and `shell`, this is stable for the life
/// of the process and changes how commands must be written, so it belongs in
/// the static block. It names no PID, so the prefix stays cacheable.
const NPM_LAUNCHER_FACT: &str = "npm (node.exe is this session's parent; killing node.exe by name ends this and other npm-launched Codewhale sessions, so stop servers by PID or port)";

fn launcher_fact(
    is_windows: bool,
    method: codewhale_release::InstallMethod,
) -> Option<&'static str> {
    (is_windows && method == codewhale_release::InstallMethod::Npm).then_some(NPM_LAUNCHER_FACT)
}

/// The launcher line for this process. Install detection reads the
/// executable's path, so it runs once and only on Windows; recorded
/// conformance environments never carry it.
fn npm_launcher_fact() -> Option<&'static str> {
    #[cfg(any(test, feature = "test-support"))]
    if RECORDED_ENVIRONMENT.with(|cell| cell.borrow().is_some()) {
        return None;
    }
    if !cfg!(windows) {
        return None;
    }
    static METHOD: std::sync::OnceLock<codewhale_release::InstallMethod> =
        std::sync::OnceLock::new();
    launcher_fact(
        true,
        *METHOD.get_or_init(codewhale_release::current_install_method),
    )
}

#[cfg(test)]
mod npm_launcher_fact_tests {
    use super::*;
    use codewhale_release::InstallMethod;

    #[test]
    fn plain_environment_block_is_unchanged() {
        assert_eq!(
            format_environment_block("en", "windows", "powershell", None),
            "## Environment\n\n- lang: en\n- platform: windows\n- shell: powershell"
        );
    }

    #[test]
    fn windows_npm_install_names_the_node_launcher() {
        let block = format_environment_block(
            "en",
            "windows",
            "powershell",
            launcher_fact(true, InstallMethod::Npm),
        );
        assert!(
            block.ends_with(&format!("\n- launcher: {NPM_LAUNCHER_FACT}")),
            "{block}"
        );
        assert!(block.contains("stop servers by PID or port"), "{block}");
    }

    #[test]
    fn launcher_fact_is_windows_npm_only() {
        assert_eq!(launcher_fact(false, InstallMethod::Npm), None);
        for method in [
            InstallMethod::Binary,
            InstallMethod::Cargo,
            InstallMethod::Homebrew,
            InstallMethod::Omarchy,
        ] {
            assert_eq!(launcher_fact(true, method), None);
        }
    }
}

/// The host facts the `## Environment` block names: this process's OS and
/// the shell commands run under. Conformance goldens recorded on one host
/// replay that host's facts on the recording thread
/// ([`pin_recorded_environment`]); production always reports this host.
fn environment_host_facts() -> (String, String) {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(recorded) = RECORDED_ENVIRONMENT.with(|cell| cell.borrow().clone()) {
        return recorded;
    }
    (
        std::env::consts::OS.to_string(),
        codewhale_sandbox::shell_dispatcher::global_dispatcher()
            .kind()
            .binary()
            .to_string(),
    )
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static RECORDED_ENVIRONMENT: std::cell::RefCell<Option<(String, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// Replay a recorded host's OS and shell in this thread's environment block
/// until the guard drops. The only caller, the scripted conformance families,
/// is Unix-only and runs its engine on this thread's runtime.
#[cfg(all(any(test, feature = "test-support"), unix))]
pub fn pin_recorded_environment(os: &str, shell: &str) -> RecordedEnvironmentGuard {
    RECORDED_ENVIRONMENT
        .with(|cell| *cell.borrow_mut() = Some((os.to_string(), shell.to_string())));
    RecordedEnvironmentGuard
}

/// The replayed shell on this thread, when a recorded environment is pinned.
/// Tool descriptions that name the shell use it too, so a replay never
/// depends on which shell first initialized their process-wide caches.
#[cfg(all(any(test, feature = "test-support"), unix))]
pub fn recorded_shell() -> Option<String> {
    RECORDED_ENVIRONMENT.with(|cell| cell.borrow().as_ref().map(|(_, shell)| shell.clone()))
}

#[cfg(all(any(test, feature = "test-support"), unix))]
pub struct RecordedEnvironmentGuard;

#[cfg(all(any(test, feature = "test-support"), unix))]
impl Drop for RecordedEnvironmentGuard {
    fn drop(&mut self) {
        RECORDED_ENVIRONMENT.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Source for an `EngineConfig.instructions` entry. Either a disk file (loaded
/// at render time, original semantics) or an inline string (content baked into
/// `EngineConfig`, no disk I/O at render time).
///
/// The inline variant is useful for embedders that compute instructions at
/// runtime (e.g. rendering a template with workspace-specific substitutions)
/// and don't want to stage the content to a disk file just to satisfy a path
/// API. Staging adds two problems the inline path avoids:
///
///   1. The disk file looks like editable config but gets overwritten on
///      every launch — confusing for users browsing the install dir.
///   2. Multi-engine setups need per-engine paths to avoid `rehydrate`
///      reading another session's instructions; with inline sources the
///      content lives in the per-engine `EngineConfig` and the race
///      surface goes away.
///
/// `From<PathBuf>` is provided so existing callers passing `Vec<PathBuf>` can
/// keep working with a `.into()` upgrade at the call site.
#[derive(Debug, Clone)]
pub enum InstructionSource {
    /// Read this file from disk when the instruction assembly runs.
    /// Missing, empty, and over-`file_max_bytes` files are skipped with a
    /// notice; a file that fits is charged to the global scope budget.
    File(PathBuf),
    /// Use the provided string directly. `name` becomes the
    /// `<instructions source="…">` attribute (typically a synthetic
    /// identifier like `embedded:my-template` or a logical path).
    Inline { name: String, content: String },
}

impl From<PathBuf> for InstructionSource {
    fn from(path: PathBuf) -> Self {
        InstructionSource::File(path)
    }
}

impl From<&PathBuf> for InstructionSource {
    fn from(path: &PathBuf) -> Self {
        InstructionSource::File(path.clone())
    }
}

/// Read the workspace-local relay artifact, if present, and format it as a
/// system-prompt block. Returns `None` when the file is absent or empty so
/// callers can keep the default-uncluttered prompt for fresh workspaces.
fn load_handoff_block(workspace: &Path) -> Option<String> {
    let (path, shown) = resolve_handoff_path(workspace);
    // The relay is workspace-writable, so it gets the same per-file cap as
    // an instructions file rather than an unbounded read into the prompt.
    let (raw, full_len) = read_prompt_file_bounded(&path, RELAY_ARTIFACT_MAX_BYTES).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let total = if raw.len() < full_len {
        full_len
    } else {
        trimmed.len()
    };
    let relay = cap_prompt_text(
        trimmed,
        total,
        RELAY_ARTIFACT_MAX_BYTES,
        "shorten the relay artifact",
    );
    Some(format!(
        "## Previous Session Relay\n\nThe previous session in this workspace left a relay artifact at `{shown}`. Consider it the first artifact to read on this turn — open blockers, in-flight changes, and recent decisions live there. Update or rewrite it before exiting if state changes materially.\n\n{relay}"
    ))
}

/// Load the structured user-global constitution, if present, and render it as
/// its own model-facing block.
pub fn load_user_constitution_block() -> Option<String> {
    load_active_user_constitution()?.render_block(None)
}

pub fn load_active_user_constitution() -> Option<Box<codewhale_config::UserConstitution>> {
    if user_constitution_disabled_by_setup_state() {
        return None;
    }

    let path = match codewhale_config::UserConstitution::path() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                target: "prompts",
                "could not resolve user-global constitution path: {err:#}"
            );
            return None;
        }
    };

    match codewhale_config::UserConstitution::load_from(&path) {
        codewhale_config::UserConstitutionLoad::Loaded(constitution) => Some(constitution),
        codewhale_config::UserConstitutionLoad::Missing
        | codewhale_config::UserConstitutionLoad::Empty => None,
        codewhale_config::UserConstitutionLoad::Invalid(err) => {
            tracing::warn!(
                target: "prompts",
                "skipping invalid user-global constitution {}: {err}",
                path.display()
            );
            None
        }
        codewhale_config::UserConstitutionLoad::Unreadable(err) => {
            tracing::warn!(
                target: "prompts",
                "skipping unreadable user-global constitution {}: {err}",
                path.display()
            );
            None
        }
    }
}

fn user_constitution_disabled_by_setup_state() -> bool {
    match codewhale_config::SetupState::load() {
        Ok(Some(state)) => !state.constitution_choice.applies_user_global(),
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(
                target: "prompts",
                "could not resolve setup-state path while loading user constitution: {err:#}"
            );
            false
        }
    }
}

// ── Prompt layers loaded at compile time ──────────────────────────────
//
// Every bundled prompt layer lives in `prompts/text.rs` as a compile-time
// constant (consolidated from the retired per-layer `prompts/*.md` files;
// each constant is byte-identical to the file it replaced, trailing newline
// included). The constants are re-exported here so the existing
// `crate::prompts::NAME` paths used across the crate are unchanged. Edit
// prompt text in `text.rs` directly; the test suite below guards content
// and ordering invariants (constitution structure and binding gates #4032,
// byte-stable prefix ordering, prefix privacy #4632).
pub use text::{
    BASE_PROMPT, COMPACT_TEMPLATE, CORE_EXECUTION_PROFILE_PROMPT, GOAL_CONTINUATION_PROMPT,
    LANGUAGE_PROMPT, MEMORY_GUIDANCE, OUTPUT_PROMPT,
};

// ── Embedder prompt overrides ──
// Let an embedder replace these compile-time prompt constants at startup,
// so brand / slimming customizations live in the embedder crate instead of
// editing these files in-tree. Unset → the bundled constant (fully
// backward compatible). Intended to be set once at process start, before
// any engine spawns; later sets return the rejected override string.
static BASE_PROMPT_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_ZH_HANS_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_JA_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_PT_BR_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_PREAMBLE_VI_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_ZH_HANS_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_JA_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_PT_BR_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static LOCALE_CLOSER_VI_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static AUTHORITY_RECAP_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static STATIC_PROMPT_COMPOSER: std::sync::OnceLock<Box<StaticPromptComposer>> =
    std::sync::OnceLock::new();
static PROMPT_OVERRIDE_NOTICES: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Context passed to an embedder-provided static prompt composer.
///
/// This hook only replaces the byte-stable base prompt segment.
/// Approval policy, Core Execution, and action-specific relay formatting stay
/// owned by Codewhale.
#[non_exhaustive]
#[derive(Debug)]
pub struct StaticPromptCtx<'a> {
    /// Active model identifier after caller-side routing.
    pub model_id: &'a str,
    /// Default base prompt layers that would be used without an
    /// override.
    pub default_layers: &'a str,
}

/// Embedder hook for replacing Codewhale's byte-stable base prompt
/// segment.
pub type StaticPromptComposer = dyn Fn(&StaticPromptCtx<'_>) -> String + Send + Sync + 'static;

/// Replace `BASE_PROMPT` for all subsequent prompt composition. First call
/// wins; later calls return the rejected string. Set before spawning any
/// engine.
pub fn set_base_prompt_override(s: String) -> Result<(), String> {
    set_prompt_override(&BASE_PROMPT_OVERRIDE, s)
}

// ── Config-directory prompt overrides (issue #3638) ──
// Bridge the embedder override hooks above to a user-facing source: an
// optional file in the Codewhale config directory. This lets users repurpose
// the TUI for non-software use cases (e.g. long-form writing) by swapping the
// constitutional base prompt, without editing in-tree files or shipping a
// custom embedder build.
//
// Scope is deliberately narrow: only the byte-stable base prompt segment is
// user-overridable. Approval policy, Core Execution, and
// action-specific relay formatting stay owned by the runtime assembly (see
// `StaticPromptCtx`), so an override cannot strip safety-relevant guidance.
// A missing or empty file is a no-op — the bundled constant is used — so this
// is fully backward compatible.
//
// Because replacing the base prompt is a trust-boundary action (per maintainer
// review on #3638), the override file alone is NOT sufficient: the user must
// also set an explicit opt-in flag (`CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE`).
// This keeps replacing the global Constitution a deliberate, auditable act
// rather than something a stray file can do.

/// Target lane for the once-per-version constitution checkpoint. Bumped per
/// release when the bundled constitution materially changes, so existing users
/// re-acknowledge it once. 0.9.4 re-ships the Fleet/operate constitution.
pub const CONSTITUTION_CHECKPOINT_VERSION: &str = "0.9.4";

/// Relative path, under the config directory, of the optional base-prompt
/// (constitution) override file.
pub const CONSTITUTION_OVERRIDE_FILE: &str = "prompts/constitution.md";

/// Env flag that must be set (`1`/`true`/`on`/`yes`) to enable config-dir base
/// prompt overrides. Required in addition to the override file so the global
/// base prompt can never be replaced by file presence alone.
pub const BASE_PROMPT_OVERRIDE_OPT_IN_ENV: &str = "CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE";

/// Whether the user has explicitly opted in to base-prompt overrides.
pub fn base_prompt_override_opt_in() -> bool {
    match std::env::var(BASE_PROMPT_OVERRIDE_OPT_IN_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

/// Read an optional prompt-override file rooted at `config_dir`.
///
/// Returns the file contents when it exists and is non-empty after trimming;
/// otherwise `None` so the caller falls back to the embedded default. Pure
/// over `config_dir`, so it is unit-testable without touching the global
/// override cells.
fn read_prompt_override_file(config_dir: &Path, relative: &str) -> Option<String> {
    let path = config_dir.join(relative);
    let raw = std::fs::read_to_string(&path).ok()?;
    if raw.trim().is_empty() {
        tracing::warn!(
            target: "prompts",
            "ignoring empty prompt override file {}",
            path.display(),
        );
        return None;
    }
    tracing::info!(
        target: "prompts",
        "loaded prompt override from {}",
        path.display(),
    );
    Some(raw)
}

fn push_prompt_override_notice(message: String) {
    if let Ok(mut notices) = PROMPT_OVERRIDE_NOTICES.lock() {
        notices.push(message);
    }
}

pub fn take_prompt_override_notices() -> Vec<String> {
    PROMPT_OVERRIDE_NOTICES
        .lock()
        .map(|mut notices| std::mem::take(&mut *notices))
        .unwrap_or_default()
}

/// Load user prompt overrides from `config_dir` and install them through the
/// existing override hooks. Returns the names of the overrides that were
/// applied (for logging/diagnostics).
///
/// Call once at startup, before any engine spawns, because the underlying
/// override cells are first-call-wins. Missing files are a no-op, preserving
/// the bundled defaults.
pub fn load_config_dir_prompt_overrides(config_dir: &Path) -> Vec<&'static str> {
    let mut applied = Vec::new();
    if let Some(text) = read_prompt_override_file(config_dir, CONSTITUTION_OVERRIDE_FILE) {
        if !base_prompt_override_opt_in() {
            // A file exists but the user hasn't opted in. Don't silently
            // replace the base prompt — surface the gate instead.
            let warning = format!(
                "Custom Constitution override found at {}/{} but {} is not set; using the bundled Constitution. Set {}=1 to opt in.",
                config_dir.display(),
                CONSTITUTION_OVERRIDE_FILE,
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
            );
            tracing::warn!(
                target: "prompts",
                "{warning}",
            );
            push_prompt_override_notice(warning);
        } else if set_base_prompt_override(text).is_ok() {
            applied.push("constitution");
        }
    }
    applied
}

/// Resolve the Codewhale config directory and load any prompt overrides found
/// there. Convenience wrapper around [`load_config_dir_prompt_overrides`] for
/// startup wiring; silently does nothing when the config home cannot be
/// resolved.
pub fn load_prompt_overrides_from_config_home() {
    let Ok(home) = codewhale_config::codewhale_home() else {
        return;
    };
    let applied = load_config_dir_prompt_overrides(&home);
    if !applied.is_empty() {
        tracing::info!(
            target: "prompts",
            "applied {} config-directory prompt override(s): {}",
            applied.len(),
            applied.join(", "),
        );
    }
}

fn set_prompt_override(cell: &std::sync::OnceLock<String>, s: String) -> Result<(), String> {
    cell.set(s)
}

fn effective_prompt_override<'a>(
    cell: &'a std::sync::OnceLock<String>,
    fallback: &'static str,
) -> &'a str {
    cell.get().map(String::as_str).unwrap_or(fallback)
}

fn effective_base_prompt() -> &'static str {
    effective_prompt_override(&BASE_PROMPT_OVERRIDE, BASE_PROMPT)
}

/// Where the base-prompt bytes used by this process actually came from.
///
/// #3928: diagnostics used to cite `crates/engine/src/prompts/text.rs`, which is
/// a source-tree path that does not exist on an installed binary and says
/// nothing about whether an override replaced the constant at startup. This
/// reports the runtime truth instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasePromptOrigin {
    /// The `BASE_PROMPT` constant compiled into this binary.
    Bundled,
    /// An opted-in `prompts/constitution.md` override installed at startup.
    ConfigOverride,
}

impl BasePromptOrigin {
    /// Short, user-facing provenance label. Contains no filesystem paths.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled in this codewhale-tui build (BASE_PROMPT, compiled in)",
            Self::ConfigOverride => concat!(
                "config-directory override installed at startup ",
                "(prompts/constitution.md, opt-in enabled)"
            ),
        }
    }
}

/// Runtime provenance of the base prompt for this process.
pub fn base_prompt_origin() -> BasePromptOrigin {
    if BASE_PROMPT_OVERRIDE.get().is_some() {
        BasePromptOrigin::ConfigOverride
    } else {
        BasePromptOrigin::Bundled
    }
}

/// The exact base-prompt bytes this process will compose into the system
/// prompt — the override when one is installed, the bundled constant
/// otherwise.
pub fn effective_base_prompt_text() -> &'static str {
    effective_base_prompt()
}

/// Where the effective base prompt actually comes from right now (#3928).
///
/// Reads the same cells composition reads, so a preview cannot claim "bundled"
/// while an override is live. `config_dir` only supplies the path shown in the
/// override label; it does not decide whether an override is in effect.
#[must_use]
pub fn effective_base_prompt_source(config_dir: Option<&Path>) -> base_preview::BasePromptSource {
    if STATIC_PROMPT_COMPOSER.get().is_some() {
        // An embedder composer wraps or replaces the whole static layer set, so
        // it outranks the base-prompt cell as the honest answer.
        return base_preview::BasePromptSource::EmbedderComposer;
    }
    if BASE_PROMPT_OVERRIDE.get().is_some() {
        return base_preview::BasePromptSource::ConfigOverride {
            path: config_dir.map_or_else(
                || CONSTITUTION_OVERRIDE_FILE.to_string(),
                |dir| dir.join(CONSTITUTION_OVERRIDE_FILE).display().to_string(),
            ),
        };
    }
    base_preview::BasePromptSource::Bundled
}

fn effective_static_prompt_composer() -> Option<&'static StaticPromptComposer> {
    STATIC_PROMPT_COMPOSER.get().map(Box::as_ref)
}

fn effective_locale_preamble_zh_hans() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_ZH_HANS_OVERRIDE, LOCALE_PREAMBLE_ZH_HANS)
}

fn effective_locale_preamble_ja() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_JA_OVERRIDE, LOCALE_PREAMBLE_JA)
}

fn effective_locale_preamble_pt_br() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_PT_BR_OVERRIDE, LOCALE_PREAMBLE_PT_BR)
}

fn effective_locale_preamble_vi() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_VI_OVERRIDE, LOCALE_PREAMBLE_VI)
}

fn effective_locale_closer_zh_hans() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_ZH_HANS_OVERRIDE, LOCALE_CLOSER_ZH_HANS)
}

fn effective_locale_closer_ja() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_JA_OVERRIDE, LOCALE_CLOSER_JA)
}

fn effective_locale_closer_pt_br() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_PT_BR_OVERRIDE, LOCALE_CLOSER_PT_BR)
}

fn effective_locale_closer_vi() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_VI_OVERRIDE, LOCALE_CLOSER_VI)
}

pub fn effective_authority_recap() -> &'static str {
    effective_prompt_override(&AUTHORITY_RECAP_OVERRIDE, AUTHORITY_RECAP)
}

/// Optional locale-native reinforcement preamble prepended to the system
/// prompt when the user's UI locale is non-English.
///
/// `constitution.md` itself stays English (single source of truth, model is
/// natively multilingual, prefix-cache stable across users in the same
/// locale). For non-English locales we prepend a short locale-native
/// passage so the model's first exposure to the prompt overrides the
/// "match user message language" English directive with an explicit
/// "use {locale}" instruction in the user's own writing system. Reduces
/// the model's reliance on inferring intent from `## Environment.lang`
/// — which previously got overpowered by overwhelmingly English task
/// context, the symptom reported in #1118 and visible in the WeChat
/// screenshot that prompted this change.
///
/// The list is intentionally short (`zh-Hans`, `ja`, `pt-BR`, `vi`) even
/// though the TUI ships UI packs for many more locales. Other locales fall
/// through to `None` and get the English-only directive, which is the same
/// behavior as before this change; the test
/// `v092_locales_add_no_prompt_bookends_so_prompt_bytes_stay_stable` locks
/// that set so adding a UI pack never silently changes prompt bytes.
///
/// ## Design philosophy: why a bookend, not a full translation
///
/// Community feedback on the WeChat thread that prompted this work
/// pointed out — correctly — that DeepSeek V4 is a Chinese-first
/// multilingual model, not an English-only model with multilingual
/// veneer. Its tokenizer is co-trained on Chinese; `你好` typically
/// encodes to ~1 token, not 2 — the "Chinese is expensive in tokens"
/// folk wisdom from Western-LLM commentary doesn't apply here.
///
/// The naïve translation of that argument would be: ship a fully
/// translated `constitution.md` per locale. We deliberately stop short of
/// that for v0.8.29. The reasons, ranked:
///
///   1. **Drift risk.** A 200+ line technical prompt has subtle
///      phrasing that drives subtle behavior. Every rule change has
///      to land in N translated copies, kept in lockstep. The class
///      of bug that arises (Chinese users see slightly different
///      agent behavior than English users) is hard to reproduce and
///      hard to triage from bug reports.
///   2. **Cache stability.** With one English `constitution.md` and a
///      per-locale preamble+closer, the largest cacheable chunk
///      (shared constitution + project context + environment) stays
///      byte-stable within a session and across users in the same
///      locale. A fully translated per-locale `constitution.md` keeps cache
///      per-locale but doesn't share with English users.
///   3. **Translation QA is expensive.** Each prompt-language pair
///      needs a native speaker reviewing tone, register, and rule
///      preservation. Getting it 95% right is bad, because the
///      missing 5% becomes silent behavior divergence.
///
/// What we DO instead — the bookend pattern @MuMu described from
/// their other project — is reinforce the locale directive in
/// native script at BOTH ends of the prompt. The opening anchors
/// behavior at session start; the closing reinforcement
/// (`locale_reinforcement_closer`) sits at the maximum-recency
/// position right before the user's next message. Empirically this
/// is sufficient to keep `reasoning_content` in the target locale
/// even as English code accumulates in context turn-over-turn.
///
/// If at some future point the bookend proves insufficient — or if
/// the maintenance cost of per-locale `constitution.md` files becomes
/// preferable to whatever's blocking it — full translation is the
/// natural next step. The locale tags here, the test invariants,
/// and the closer position would all carry over unchanged.
pub fn locale_reinforcement_preamble(locale_tag: &str) -> Option<&'static str> {
    match locale_tag {
        "zh-Hans" | "zh-CN" | "zh" => Some(effective_locale_preamble_zh_hans()),
        "ja" | "ja-JP" => Some(effective_locale_preamble_ja()),
        "pt-BR" | "pt" => Some(effective_locale_preamble_pt_br()),
        "vi" | "vi-VN" => Some(effective_locale_preamble_vi()),
        _ => None,
    }
}

/// Locale-native closing reinforcement appended to the very end of the
/// system prompt — the bookend MuMu described in the WeChat thread that
/// prompted #1118 follow-up work.
///
/// The opening preamble alone is not enough: as the model accumulates
/// English context turn-over-turn (code, error logs, search results,
/// file listings), the recency bias of the transformer's attention
/// drifts thinking back toward English even when the user keeps writing
/// in their own language. A closing native-script reinforcement sits at
/// the position closest to the user's next message — where attention
/// weight is highest — and re-asserts the language rule right before
/// the model generates `reasoning_content` for the turn.
///
/// Like the opening preamble, English (and unknown) locales return
/// `None` and the system prompt is byte-identical to the pre-bookend
/// behavior.
pub fn locale_reinforcement_closer(locale_tag: &str) -> Option<&'static str> {
    match locale_tag {
        "zh-Hans" | "zh-CN" | "zh" => Some(effective_locale_closer_zh_hans()),
        "ja" | "ja-JP" => Some(effective_locale_closer_ja()),
        "pt-BR" | "pt" => Some(effective_locale_closer_pt_br()),
        "vi" | "vi-VN" => Some(effective_locale_closer_vi()),
        _ => None,
    }
}

const LOCALE_PREAMBLE_ZH_HANS: &str = "## 语言要求\n\n\
你正在 codewhale 中运行。无论任务上下文（代码、错误日志、文件名）\
是英文，无论系统提示的其余部分是英文，你都必须用简体中文进行 \
`reasoning_content`（内部思考）和最终回复。代码、文件路径、工具名称\
（例如 `read`、`bash`）、环境变量、命令行参数和 URL \
保持原样 —— 只有自然语言散文要切换到简体中文。\n\n\
如果用户在会话中切换到另一种语言，从下一轮开始跟随切换。\
如果用户明确要求（例如 \"think in English\"），则覆盖此规则。";

const LOCALE_PREAMBLE_JA: &str = "## 言語要件\n\n\
codewhale を実行しています。タスクコンテキスト（コード、エラーログ、\
ファイル名）が英語であっても、システムプロンプトの他の部分が英語で\
あっても、`reasoning_content`（内部思考）と最終的な返信は日本語で\
行ってください。コード、ファイルパス、ツール名（例：`read`、\
`bash`）、環境変数、コマンドライン引数、URL は元のまま —— \
自然言語の文章のみ日本語に切り替えます。\n\n\
ユーザーがセッション中に別の言語に切り替えた場合は、次のターンから\
それに従ってください。ユーザーが明示的に要求した場合（例：\
\"think in English\"）はこのルールを上書きします。";

const LOCALE_PREAMBLE_PT_BR: &str = "## Requisito de Idioma\n\n\
Você está rodando dentro do codewhale. Escreva tanto \
`reasoning_content` (seu pensamento interno) quanto a resposta final \
em português do Brasil, mesmo quando o contexto da tarefa (código, \
logs de erro, nomes de arquivos) estiver em inglês e mesmo quando o \
resto do system prompt for em inglês. Mantenha código, caminhos de \
arquivos, nomes de ferramentas (por exemplo `read`, \
`bash`), variáveis de ambiente, flags de linha de comando e \
URLs no formato original — apenas a prosa em linguagem natural muda \
para português do Brasil.\n\n\
Se o usuário mudar de idioma no meio da sessão, mude no próximo turno. \
Se o usuário pedir explicitamente (por exemplo, \"think in English\"), \
isso sobrescreve esta regra.";

// ── Closing bookends (appended to the very end of the system prompt) ──

const LOCALE_CLOSER_ZH_HANS: &str = "## 语言再次提醒\n\n\
**重要：你的 `reasoning_content`（内部思考）和最终回复必须保持简体中文。** \
无论你在这次会话中读到了多少英文代码、错误日志或文档，无论项目上下文 \
是英文，思考过程不能漂移到英文。这是会话级硬性要求 —— 用户的语言决定 \
你的语言，与上下文中累积的英文内容无关。除非用户明确要求切换（例如 \
\"think in English\"），否则继续用简体中文思考和回答。";

const LOCALE_CLOSER_JA: &str = "## 言語再確認\n\n\
**重要：`reasoning_content`（内部思考）と最終的な返信は日本語で行ってください。** \
このセッションで読み込んだ英語のコード、エラーログ、ドキュメントの量に \
関係なく、プロジェクトコンテキストが英語であっても、思考プロセスを \
英語に逸らさないでください。これはセッションレベルの厳格な要件であり、 \
ユーザーの言語があなたの言語を決定します。ユーザーが明示的に切り替えを \
要求しない限り（例：\"think in English\"）、日本語で思考し、回答し続けて \
ください。";

const LOCALE_CLOSER_PT_BR: &str = "## Reforço de Idioma\n\n\
**Importante: seu `reasoning_content` (pensamento interno) e a resposta \
final devem permanecer em português do Brasil.** Independentemente de \
quanto código em inglês, logs de erro ou documentação você ler nesta \
sessão, e independentemente de o contexto do projeto ser em inglês, o \
processo de pensamento não pode derivar para o inglês. Este é um \
requisito rígido em nível de sessão — o idioma do usuário define seu \
idioma. A menos que o usuário peça explicitamente a troca (por exemplo, \
\"think in English\"), continue pensando e respondendo em português do \
Brasil.";

const LOCALE_PREAMBLE_VI: &str = "## Yêu cầu ngôn ngữ\n\n\
Bạn đang chạy trong codewhale. Cho dù ngữ cảnh tác vụ (mã nguồn, nhật ký lỗi, tên tệp) \
là tiếng Anh, cho dù phần còn lại của system prompt là tiếng Anh, bạn đều phải sử dụng \
tiếng Việt cho phần `reasoning_content` (suy nghĩ nội bộ) và câu trả lời cuối cùng. Các từ \
mã nguồn, đường dẫn tệp, tên công cụ (ví dụ `read`, `bash`), biến môi trường, \
tham số dòng lệnh và URL giữ nguyên dạng gốc —— chỉ các văn bản giải thích bằng ngôn ngữ \
tự nhiên mới được chuyển sang tiếng Việt.\n\n\
Nếu người dùng chuyển sang ngôn ngữ khác trong phiên làm việc, hãy chuyển theo từ lượt tiếp theo. \
Nếu người dùng yêu cầu rõ ràng (ví dụ \"think in English\"), hãy ghi đè quy tắc này.";

const LOCALE_CLOSER_VI: &str = "## Nhắc nhở ngôn ngữ một lần nữa\n\n\
**Quan trọng: phần `reasoning_content` (suy nghĩ nội bộ) và phản hồi cuối cùng của bạn phải được viết bằng tiếng Việt.** \
Dù bạn có đọc bao nhiêu mã nguồn tiếng Anh, nhật ký lỗi hay tài liệu trong phiên làm việc này, và dù ngữ cảnh \
dự án có là tiếng Anh, quá trình suy nghĩ của bạn cũng không được chuyển sang tiếng Anh. Đây là yêu cầu cứng \
ở cấp phiên làm việc —— ngôn ngữ của người dùng quyết định ngôn ngữ của bạn, không phụ thuộc vào nội dung tiếng Anh \
tích lũy trong ngữ cảnh. Trừ khi người dùng yêu cầu rõ ràng việc chuyển đổi (ví dụ \"think in English\"), \
hãy tiếp tục suy nghĩ và trả lời bằng tiếng Việt.";

// ── Composition ───────────────────────────────────────────────────────

/// Substitute the model id for embedder-supplied prompt overrides that still
/// template it. The bundled constitution is deliberately model-agnostic and
/// carries no model-fact placeholders.
fn apply_model_template(
    prompt: &str,
    model_id: &str,
    _context_window_override: Option<u32>,
) -> String {
    prompt.replace("{model_id}", model_id)
}

/// Authority recap block — appended at the end of the system prompt,
/// just before the user's first message. Uses recency bias constructively
/// without restating ranks: precedence is stated only in `BASE_PROMPT`
/// § Whose word wins (#4777).
const AUTHORITY_RECAP: &str = "\
## Authority Recap

Codewhale's constitution governs your behavior. Ground truth underlies the
whole list: the user may override a fact, but no one may invent one. When
guidance conflicts, consult ### Whose word wins — that is the only place
precedence is stated.";

pub fn compose_prompt_with_approval_model_and_shell(model_id: &str) -> String {
    let default_layers = compose_default_static_layers(model_id);
    apply_static_prompt_composer(
        effective_static_prompt_composer(),
        model_id,
        &default_layers,
    )
}

pub fn compose_default_static_layers(model_id: &str) -> String {
    compose_default_static_layers_with_context(model_id, None)
}

fn compose_default_static_layers_with_context(
    model_id: &str,
    context_window_override: Option<u32>,
) -> String {
    // Voice and tone live in the constitutional preamble. Language and output
    // rules are split into their own static segments so the constitution
    // stays compact.
    let layers = format!(
        "{}\n\n{}\n\n{}",
        effective_base_prompt().trim(),
        LANGUAGE_PROMPT.trim(),
        OUTPUT_PROMPT.trim()
    );
    apply_model_template(&layers, model_id, context_window_override)
}

/// Host surface selecting the bundled constitution size.
///
/// Modes never select prompt doctrine. Their permissions and capabilities are
/// expressed by runtime policy and the live tool catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptHost {
    Interactive,
    Headless,
}

fn apply_static_prompt_composer(
    composer: Option<&StaticPromptComposer>,
    model_id: &str,
    default_layers: &str,
) -> String {
    match composer {
        Some(composer) => composer(&StaticPromptCtx {
            model_id,
            default_layers,
        }),
        None => default_layers.to_string(),
    }
}

// Every host shares BASE_PROMPT — one constitution, one stance. Host selects
// only which ceremony layers follow it; tool availability is enforced by the
// catalog and execution layer, never by mode-specific prompt text.

// ── Public API ────────────────────────────────────────────────────────

/// Get the system prompt for a specific mode with project context.
pub fn system_prompt_for_mode_with_context(
    workspace: &Path,
    working_set_summary: Option<&str>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_and_skills(workspace, working_set_summary, None, None, None)
}

/// Get the system prompt for a specific mode with project and skills context.
///
/// **Volatile-content-last invariant.** Blocks are appended in order from
/// most-static to most-volatile so DeepSeek's KV prefix cache hits the
/// longest possible byte prefix turn-over-turn:
///
///   1. shared constitution (compile-time constant)
///   2. project context / fallback (workspace-static)
///   3. skills block (skills-dir-static)
///   4. `## Core Execution` (compile-time constant)
///   5. compaction relay template (compile-time constant)
///   6. relay block — file-backed; rewritten by `/compact` and on exit
///
/// Anything appended after a volatile block forfeits the cache for the rest
/// of the request. New blocks belong above the relay boundary unless they
/// themselves are turn-volatile. Working-set metadata is now injected into the
/// latest user message as per-turn metadata instead of this system prompt.
pub fn system_prompt_for_mode_with_context_and_skills(
    workspace: &Path,
    working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    user_memory_block: Option<&str>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_skills_and_session(
        workspace,
        working_set_summary,
        skills_dir,
        instructions,
        PromptSessionContext {
            user_memory_block,
            goal_objective: None,
            project_context_pack_enabled: false,
            locale_tag: "en",
            translation_enabled: false,
            model_id: "codewhale",
            context_window_override: None,
            verbosity: None,
            recovery_hint: None,
            skills_config: Default::default(),
            plugin_registry: None,
            mode: AppMode::Agent,
        },
    )
}

pub fn system_prompt_for_mode_with_context_skills_and_session(
    workspace: &Path,
    _working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_skills_session_and_approval(
        workspace,
        _working_set_summary,
        skills_dir,
        instructions,
        session_context,
    )
}

pub fn system_prompt_for_mode_with_context_skills_session_and_approval(
    workspace: &Path,
    _working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
) -> SystemPrompt {
    system_prompt_for_mode_with_context_skills_session_and_approval_for_host(
        workspace,
        _working_set_summary,
        skills_dir,
        instructions,
        session_context,
        PromptHost::Interactive,
    )
}

pub fn system_prompt_for_mode_with_context_skills_session_and_approval_for_host(
    workspace: &Path,
    _working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
    prompt_host: PromptHost,
) -> SystemPrompt {
    system_prompt_with_instruction_assembly_for_host(
        workspace,
        _working_set_summary,
        skills_dir,
        instructions,
        session_context,
        prompt_host,
        None,
    )
    .0
}

/// Compose the system prompt **and** the instruction assembly that produced
/// its instruction block.
///
/// The assembly carries the per-scope budgets and every trim/skip notice, so
/// the engine can enforce the static-prefix guard and surface the losses to
/// the user, the model, and `/context` from one measurement.
pub fn system_prompt_with_instruction_assembly_for_host(
    workspace: &Path,
    _working_set_summary: Option<&str>,
    skills_dir: Option<&Path>,
    instructions: Option<&[InstructionSource]>,
    session_context: PromptSessionContext<'_>,
    prompt_host: PromptHost,
    instruction_input: Option<(
        &crate::project_context::ProjectContext,
        &crate::project_context::InstructionSettings,
        crate::project_context::InstructionBudgets,
    )>,
) -> (SystemPrompt, crate::project_context::InstructionAssembly) {
    // One base prompt for every host (AGENTS.md: `BASE_PROMPT` is the sole
    // base prompt). Headless still skips interactive ceremony layers below —
    // the execution profile and authority recap — which are host chrome, not
    // doctrine.
    let headless = prompt_host == PromptHost::Headless;
    let default_layers = compose_default_static_layers_with_context(
        session_context.model_id,
        session_context.context_window_override,
    );
    let composed = apply_static_prompt_composer(
        effective_static_prompt_composer(),
        session_context.model_id,
        &default_layers,
    );

    let default_settings = crate::project_context::InstructionSettings::default();
    let settings = instruction_input
        .as_ref()
        .map_or(&default_settings, |input| input.1);
    let raw_context = instruction_input.as_ref().map_or_else(
        || {
            crate::project_context::capture_instruction_context(
                workspace,
                instructions.unwrap_or_default(),
                settings,
            )
        },
        |input| input.0.clone(),
    );
    let mut project_context = raw_context;
    let budgets = instruction_input.map_or_else(
        || {
            crate::project_context::InstructionBudgets::defaults_for(
                session_context
                    .context_window_override
                    .map(|window| window as usize),
            )
        },
        |input| input.2,
    );
    let mut assembly =
        crate::project_context::assemble_instruction_block(&mut project_context, &[], &budgets);

    // 0. Locale-native reinforcement preamble (#1118 follow-up). When the
    // user's UI locale is non-English we prepend a short native-script
    // passage so the model's first exposure to the prompt is an explicit
    // "think and reply in {locale}" directive in the user's own writing
    // system — defeats the "task context is English, so the model thinks
    // in English even though `lang: zh-Hans` is set" failure mode that
    // PR #1398 partially addressed. English (and unknown) locales get
    // `None` and keep the previous behavior unchanged.
    let preamble = locale_reinforcement_preamble(session_context.locale_tag);

    // 1–2. Shared constitution + project context. Mode is deliberately absent:
    // permissions and capabilities come from runtime policy and the tool catalog.
    // `load_project_context_with_parents` generates an in-memory bounded
    // overview when no context file exists, so the fallback should usually be
    // available without writing project-local files.
    let mut full_prompt = if let Some(project_block) = assembly.block.as_deref() {
        let start = composed.trim().len();
        assembly.prompt_range = Some(start..start + 2 + project_block.len());
        format!("{}\n\n{project_block}", composed.trim())
    } else {
        // Extremely unlikely: context generation failed (e.g. filesystem error).
        // Use the shared constitution alone rather than panic.
        tracing::warn!("No project context available and auto-generation failed");
        composed.trim().to_string()
    };

    if let Some(preamble) = preamble {
        if let Some(range) = assembly.prompt_range.as_mut() {
            range.start += preamble.len() + 2;
            range.end += preamble.len() + 2;
        }
        full_prompt = format!("{preamble}\n\n{full_prompt}");
    }

    // Personal preferences are captured once at turn admission and recorded in
    // history, so live profile edits never mutate an in-flight prompt prefix.

    if session_context.project_context_pack_enabled
        && let Some(pack) = crate::project_context::generate_project_context_pack(workspace)
    {
        full_prompt = format!("{full_prompt}\n\n{pack}");
    }

    // 2.3a. Translation output instruction — when enabled, instruct
    // the model to respond in the resolved session locale. Stays
    // above the volatile-content boundary because it's a per-session
    // flag, not a per-turn one: enabling `/translate` is a session
    // toggle, so the prompt-prefix bytes don't drift turn-over-turn.
    if session_context.translation_enabled {
        full_prompt = format!(
            "{full_prompt}\n\n{}",
            translation_output_instruction(session_context.locale_tag)
        );
    }

    if is_concise_verbosity(session_context.verbosity) {
        full_prompt = format!(
            "{full_prompt}\n\n{}",
            concise_output_discipline_instruction()
        );
    }

    // 3. Skills block. #432: default discovery walks every compatible
    // workspace/global skill directory so skills installed for other AI-tool
    // conventions show up in the catalogue. Users can opt into a Codewhale-only
    // scan with `[skills] scan_codewhale_only = true`. When an explicit
    // `skills_dir` is configured, union it with the workspace view instead of
    // treating it as a fallback; the workspace view often returns Some and
    // would otherwise shadow the configured directory entirely.
    let skills_block = codewhale_skills::render_index(
        workspace,
        skills_dir,
        &session_context.skills_config,
        session_context.locale_tag,
        session_context.plugin_registry,
        session_context.context_window_override,
    );
    if let Some(block) = skills_block {
        full_prompt = format!("{full_prompt}\n\n{block}");
    }

    // 4. Lean, runtime-only coding discipline. Context pressure, prompt-cache
    // accounting, footer presentation, and automatic compaction are host
    // responsibilities; teaching their UI to the model dilutes the task.
    if !headless {
        full_prompt.push_str("\n\n");
        full_prompt.push_str(CORE_EXECUTION_PROFILE_PROMPT.trim());
    }

    // The compaction/relay format is action-specific context. Automatic
    // compaction owns its structured successor brief, while `/relay` appends
    // `COMPACT_TEMPLATE` to that command's user message. Keeping the template
    // out of every fresh session saves a stable-prefix block without removing
    // the capability.

    // ── Volatile-content boundary → WorldState fragments ──────────────────
    // Constitution (`full_prompt`) stays the cache-stable Blocks[0] prefix.
    // Everything below drifts mid-session and is assembled as marked
    // WorldState fragments so an env/memory/goal/handoff change can
    // `render_diff` without rebuilding unrelated material.

    // Workspace fragment: environment + mid-session memory/goal facts.
    let mut workspace_parts = vec![render_environment_block(
        workspace,
        session_context.locale_tag,
    )];
    if let Some(memory_block) = session_context.user_memory_block
        && !memory_block.trim().is_empty()
    {
        workspace_parts.push(format!("{memory_block}\n\n{MEMORY_GUIDANCE}"));
    }
    if prompt_host == PromptHost::Interactive
        && let Some(harness_block) = codewhale_runtime::continual_harness::prompt_block(workspace)
    {
        workspace_parts.push(harness_block);
    }
    if let Some(goal_objective) = session_context.goal_objective
        && !goal_objective.trim().is_empty()
    {
        workspace_parts.push(format!(
            "## Current Goal\n\n<session_goal>\n{}\n</session_goal>",
            goal_objective.trim()
        ));
    }
    // #5715: name an interrupted prior workspace session so the model can
    // offer recovery. Session-pinned: absent entirely on clean sessions so
    // they share identical prefix bytes.
    if let Some(hint) = session_context.recovery_hint
        && !hint.trim().is_empty()
    {
        workspace_parts.push(format!(
            "## Prior Session\n\n<session_recovery>\n{}\n</session_recovery>",
            hint.trim()
        ));
    }
    let workspace_body = workspace_parts.join("\n\n");

    // Configured `instructions = [...]` files (#454) are global-scope
    // instruction authority now: the assembly reads and charges them, and they
    // render inside the project-instructions block instead of as a separate
    // world-state fragment. Exactly one owner, one charge.
    let runtime_sections: Vec<_> = instructions
        .unwrap_or_default()
        .iter()
        .filter_map(|source| match source {
            InstructionSource::Inline { name, content } if name.starts_with("runtime:") => Some(
                format!("<instructions source=\"{name}\">\n{content}\n</instructions>"),
            ),
            _ => None,
        })
        .collect();
    let permissions_body = (!runtime_sections.is_empty()).then(|| runtime_sections.join("\n\n"));

    // Route fragment: verbosity / translation posture (the model id was
    // removed by the turn-meta diet — it is telemetry the model cannot act on).
    let route_body = render_route_fragment(&session_context);

    // Token-budget / continuity fragment: prior-session handoff relay.
    let token_budget_body = load_handoff_block(workspace);

    let mut world_state = world_state_from_session_facts(
        Some(workspace_body.as_str()),
        permissions_body.as_deref(),
        Some(route_body.as_str()),
        None, // AgentTopology is updated by runtime callers when available.
        None, // Skills stay in the constitution prefix (skills-dir-static).
        token_budget_body.as_deref(),
    );
    // Project-instruction import (#3978, #4079) as a typed fragment with
    // hard caps — unified with `codewhale_core::fragments`. This covers
    // `.cursorrules`, `.clinerules`, `.windsurf/rules/*`, `.gemini/*`,
    // `.github/copilot-instructions.md` etc., beyond the canonical
    // `AGENTS.md` already in the constitution prefix.
    if let Some(fragment) =
        codewhale_core::fragments::load_selected_project_instruction_fragment_with_reader(
            workspace,
            &crate::project_context::fragment_candidates_for(&settings.imports),
            |path| codewhale_util::fs_confined::open_read(workspace, path),
        )
    {
        // `BoundedFragment` already enforces `MAX_FRAGMENT_BYTES` (10K-token
        // ceiling) and per-fragment caps; WorldState's `with_*` also clamps.
        world_state = world_state.with_project_instructions(fragment.content);
        debug_assert!(world_state.validate_caps().is_ok());
    }

    let mut blocks = codewhale_runtime::model_context::WorldStateSnapshot {
        constitution: full_prompt,
        world_state,
    }
    .to_system_blocks();

    // Trailers keep recency bias after WorldState: authority, then locale.
    if !headless {
        blocks.push(SystemBlock {
            block_type: "text".to_string(),
            text: effective_authority_recap().trim().to_string(),
            cache_control: None,
        });
    }
    if let Some(closer) = locale_reinforcement_closer(session_context.locale_tag) {
        blocks.push(SystemBlock {
            block_type: "text".to_string(),
            text: closer.trim().to_string(),
            cache_control: None,
        });
    }

    (SystemPrompt::Blocks(blocks), assembly)
}

/// Estimated tokens of the request prefix before any conversation: every
/// system block — so inline authority appended after the constitution counts
/// — plus the tool schemas the request carries.
#[must_use]
pub fn measure_static_prefix(system: Option<&SystemPrompt>, tools: &[Tool]) -> usize {
    let text_tokens = match system {
        Some(SystemPrompt::Text(text)) => {
            crate::compaction::estimate_text_tokens_conservative(text)
        }
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| crate::compaction::estimate_text_tokens_conservative(&block.text))
            .sum(),
        None => 0,
    };
    let tool_tokens: usize = tools
        .iter()
        .map(|tool| {
            serde_json::to_string(tool).map_or(0, |json| {
                crate::compaction::estimate_text_tokens_conservative(&json)
            })
        })
        .sum();
    text_tokens.saturating_add(tool_tokens)
}

const GUARD_PASSES_PER_SCOPE: usize = 2;

type GuardInstructionResult = Result<
    (
        Option<SystemPrompt>,
        Option<crate::project_context::InstructionAssembly>,
        Vec<crate::project_context::InstructionNotice>,
    ),
    crate::project_context::InstructionRefusal,
>;

fn replace_installed_instruction_block(
    prompt: &mut Option<SystemPrompt>,
    raw: &crate::project_context::ProjectContext,
    installed: &crate::project_context::InstructionAssembly,
    budgets: &crate::project_context::InstructionBudgets,
) -> Result<
    Option<crate::project_context::InstructionAssembly>,
    crate::project_context::InstructionRefusal,
> {
    let Some(range) = installed.prompt_range.clone() else {
        return Ok(None);
    };
    let text = match prompt.as_mut() {
        Some(SystemPrompt::Text(text)) => text,
        Some(SystemPrompt::Blocks(blocks)) => match blocks.first_mut() {
            Some(block) => &mut block.text,
            None => return Ok(None),
        },
        None => return Ok(None),
    };
    let owned = |block: Option<&str>| {
        block
            .filter(|block| !block.is_empty())
            .map_or_else(String::new, |block| format!("\n\n{block}"))
    };
    if text.get(range.clone()) != Some(owned(installed.block.as_deref()).as_str()) {
        return Err(crate::project_context::InstructionRefusal::ReceiptMismatch);
    }
    let mut candidate = raw.clone();
    let mut next = crate::project_context::assemble_instruction_block(&mut candidate, &[], budgets);
    let replacement = owned(next.block.as_deref());
    text.replace_range(range.clone(), &replacement);
    next.prompt_range = Some(range.start..range.start + replacement.len());
    Ok(Some(next))
}

pub fn guard_instruction_prefix(
    mut prompt: Option<SystemPrompt>,
    raw: Option<&crate::project_context::ProjectContext>,
    mut assembly: Option<crate::project_context::InstructionAssembly>,
    budgets: crate::project_context::InstructionBudgets,
    tools: &[Tool],
) -> GuardInstructionResult {
    use crate::project_context::{
        InstructionAction, InstructionBudgetPolicy, InstructionLimit, InstructionNotice,
        InstructionRefusal, InstructionScope,
    };
    if let Some(assembly) = assembly.as_mut() {
        assembly
            .notices
            .retain(|notice| notice.limit != InstructionLimit::StaticGuard);
    }
    let refuse = budgets.policy == InstructionBudgetPolicy::Refuse;
    if let Some((raw, installed)) = raw.zip(assembly.as_ref())
        && installed.budgets != budgets
        && let Some(next) =
            replace_installed_instruction_block(&mut prompt, raw, installed, &budgets)?
    {
        assembly = Some(next);
    }
    if refuse
        && let Some(loss) = assembly.as_ref().and_then(|assembly| {
            assembly.notices.iter().find(|notice| {
                notice.is_loss()
                    && matches!(
                        notice.limit,
                        InstructionLimit::ScopeBudget
                            | InstructionLimit::RulesBudget
                            | InstructionLimit::FileContent
                    )
            })
        })
    {
        let mut refused = loss.clone();
        refused.action = InstructionAction::Refused;
        return Err(InstructionRefusal::Loss(refused));
    }
    let Some(guard) = budgets.static_guard_tokens else {
        return Ok((prompt, assembly, Vec::new()));
    };
    let mut measured = measure_static_prefix(prompt.as_ref(), tools);
    if measured <= guard {
        return Ok((prompt, assembly, Vec::new()));
    }
    if refuse {
        return Err(InstructionRefusal::OverGuard { measured, guard });
    }
    let mut guard_notices: Vec<InstructionNotice> = Vec::new();
    if let (Some(raw), Some(first)) = (raw, assembly.as_ref()) {
        let mut effective = first.budgets;
        let mut order = [
            (InstructionScope::Global, first.global_tokens),
            (InstructionScope::Project, first.project_tokens),
        ];
        order.sort_by_key(|&(_, retained)| std::cmp::Reverse(retained));
        'scopes: for (scope, initial) in order {
            let mut retained = initial;
            for _ in 0..GUARD_PASSES_PER_SCOPE {
                if measured <= guard || retained == 0 {
                    break;
                }
                effective =
                    effective.with_scope_ceiling(scope, retained.saturating_sub(measured - guard));
                let Some(installed) = assembly.as_ref() else {
                    break 'scopes;
                };
                let Some(next) =
                    replace_installed_instruction_block(&mut prompt, raw, installed, &effective)?
                else {
                    break 'scopes;
                };
                retained = match scope {
                    InstructionScope::Global => next.global_tokens,
                    InstructionScope::Project => next.project_tokens,
                };
                assembly = Some(next);
                measured = measure_static_prefix(prompt.as_ref(), tools);
            }
            if retained < initial {
                guard_notices.push(InstructionNotice::static_guard(
                    scope,
                    initial - retained,
                    false,
                ));
            }
        }
    }
    if measured > guard {
        guard_notices.push(InstructionNotice::static_guard(
            InstructionScope::Global,
            measured - guard,
            true,
        ));
    }
    if let Some(assembly) = assembly.as_mut() {
        assembly.notices.append(&mut guard_notices);
    }
    Ok((prompt, assembly, guard_notices))
}

/// Flatten a system prompt to joined text (tests + debug inspectors).
#[must_use]
pub fn system_prompt_flat_text(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn render_route_fragment(session_context: &PromptSessionContext<'_>) -> String {
    let verbosity = session_context
        .verbosity
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default");
    format!(
        "verbosity: {verbosity}\ntranslation: {}",
        if session_context.translation_enabled {
            "on"
        } else {
            "off"
        },
    )
}

/// Build a WorldState from the common volatile session facts.
///
/// Does not load constitution — callers keep that as the stable base.
pub fn world_state_from_session_facts(
    workspace_body: Option<&str>,
    permissions_body: Option<&str>,
    route_body: Option<&str>,
    agent_topology_body: Option<&str>,
    skills_tools_body: Option<&str>,
    token_budget_body: Option<&str>,
) -> codewhale_runtime::model_context::WorldState {
    let mut state = codewhale_runtime::model_context::WorldState::new();
    if let Some(body) = workspace_body.filter(|s| !s.trim().is_empty()) {
        state = state.with_workspace(body);
    }
    if let Some(body) = permissions_body.filter(|s| !s.trim().is_empty()) {
        state = state.with_permissions(body);
    }
    if let Some(body) = route_body.filter(|s| !s.trim().is_empty()) {
        state = state.with_route(body);
    }
    if let Some(body) = agent_topology_body.filter(|s| !s.trim().is_empty()) {
        state = state.with_agent_topology(body);
    }
    if let Some(body) = skills_tools_body.filter(|s| !s.trim().is_empty()) {
        state = state.with_skills_tools(body);
    }
    if let Some(body) = token_budget_body.filter(|s| !s.trim().is_empty()) {
        state = state.with_token_budget(body);
    }
    state
}

#[cfg(test)]
mod tests {
    // Don't assert on prose. If you wouldn't fail a code review for
    // changing the wording, don't fail a test for it.
    use super::*;
    use crate::tools::apply_patch::ApplyPatchTool;
    use crate::tools::file::{EditFileTool, WriteFileTool};
    use crate::tools::handle::HandleReadTool;
    use crate::tools::rlm::RlmTool;
    use crate::tools::shell::BashTool;
    use crate::tools::spec::ToolSpec;
    use tempfile::tempdir;

    /// Discriminator unique to the injected relay block (not present in the
    /// agent prompt's own discussion of the convention). It stops before the
    /// path because the block names whichever file was read; the tests that
    /// care about the path assert it themselves.
    const HANDOFF_BLOCK_MARKER: &str = "left a relay artifact at `";

    // Config-directory prompt override resolution (#3638). These exercise the
    // pure file resolver only; the global install path is intentionally not
    // unit-tested here because `set_base_prompt_override` writes a process-wide
    // `OnceLock` that would leak into sibling tests (same reason
    // `prompt_override_storage_reports_duplicate_sets` uses a local cell).

    #[test]
    fn config_override_reads_present_nonempty_file() {
        let tmp = tempdir().expect("tempdir");
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("mkdir");
        std::fs::write(
            prompts_dir.join("constitution.md"),
            "You are a long-form writing companion.\n",
        )
        .expect("write override");

        let got = read_prompt_override_file(tmp.path(), CONSTITUTION_OVERRIDE_FILE);
        assert_eq!(
            got.as_deref(),
            Some("You are a long-form writing companion.\n")
        );
    }

    #[test]
    fn config_override_absent_file_falls_back() {
        let tmp = tempdir().expect("tempdir");
        // No prompts/ directory at all → None so the embedded constant is used.
        assert!(read_prompt_override_file(tmp.path(), CONSTITUTION_OVERRIDE_FILE).is_none());
    }

    #[test]
    fn config_override_requires_explicit_opt_in() {
        // A present, non-empty override file must NOT replace the base prompt
        // unless the explicit opt-in flag is set. This test drains the shared
        // process-global PROMPT_OVERRIDE_NOTICES queue, so it must serialize
        // against the sibling test that also touches it
        // (`tui::ui::tests::prompt_override_notice_surfaces_in_transcript_and_toast`);
        // both take `lock_test_env()` for mutual exclusion under the multi-
        // threaded test binary.
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("mkdir");
        std::fs::write(
            prompts_dir.join("constitution.md"),
            "You are a long-form writing companion.\n",
        )
        .expect("write override");

        // The resolver still finds the file...
        assert!(read_prompt_override_file(tmp.path(), CONSTITUTION_OVERRIDE_FILE).is_some());
        // ...but without the opt-in flag, nothing is applied.
        if std::env::var(BASE_PROMPT_OVERRIDE_OPT_IN_ENV).is_err() {
            let _ = take_prompt_override_notices();
            assert!(
                load_config_dir_prompt_overrides(tmp.path()).is_empty(),
                "override must require the explicit opt-in flag, not just a file"
            );
            let notices = take_prompt_override_notices();
            assert!(
                notices
                    .iter()
                    .any(|notice| notice.contains(BASE_PROMPT_OVERRIDE_OPT_IN_ENV)
                        && notice.contains("using the bundled Constitution")),
                "gated override should record a visible notice, got {notices:?}"
            );
        }
    }

    #[test]
    fn config_override_empty_file_is_ignored() {
        let tmp = tempdir().expect("tempdir");
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("mkdir");
        std::fs::write(prompts_dir.join("constitution.md"), "   \n\t\n").expect("write blank");

        // Whitespace-only overrides are treated as absent so a stray empty file
        // can't silently blank the system prompt.
        assert!(read_prompt_override_file(tmp.path(), CONSTITUTION_OVERRIDE_FILE).is_none());
    }

    #[test]
    fn prompt_override_storage_reports_duplicate_sets() {
        let cell = std::sync::OnceLock::new();

        assert_eq!(effective_prompt_override(&cell, "fallback"), "fallback");
        assert!(set_prompt_override(&cell, "first".to_string()).is_ok());
        assert_eq!(effective_prompt_override(&cell, "fallback"), "first");
        assert_eq!(
            set_prompt_override(&cell, "second".to_string()),
            Err("second".to_string())
        );
        assert_eq!(effective_prompt_override(&cell, "fallback"), "first");
    }

    #[test]
    fn static_prompt_composer_unset_keeps_default_layers_byte_identical() {
        let default_layers = compose_default_static_layers("deepseek-v4-flash");
        let composed = apply_static_prompt_composer(None, "deepseek-v4-flash", &default_layers);

        assert_byte_identical("unset static prompt composer", &default_layers, &composed);
    }

    #[test]
    fn static_prompt_composer_receives_context_and_replaces_layers() {
        let default_layers = compose_default_static_layers("deepseek-v4-pro");
        let composer: Box<StaticPromptComposer> = Box::new(|ctx| {
            assert_eq!(ctx.model_id, "deepseek-v4-pro");
            // The core is model-agnostic ("You are Codewhale") and carries
            // tone in the preamble — no per-model id line.
            assert!(ctx.default_layers.contains("You are Codewhale"));
            assert!(
                ctx.default_layers
                    .contains("Take the work seriously. Don't take")
            );
            assert!(!ctx.default_layers.contains("## Core Tool Taxonomy"));
            assert!(!ctx.default_layers.contains("Approval Policy"));
            "embedder static prompt".to_string()
        });

        let composed = apply_static_prompt_composer(
            Some(composer.as_ref()),
            "deepseek-v4-pro",
            &default_layers,
        );

        assert_eq!(composed, "embedder static prompt");
    }

    fn contains_cjk(text: &str) -> bool {
        text.chars().any(|ch| {
            matches!(
                ch,
                '\u{3040}'..='\u{30ff}'
                    | '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{f900}'..='\u{faff}'
            )
        })
    }

    #[test]
    fn every_mode_shares_one_prompt_per_host() {
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("AGENTS.md"),
            "# Project instruction\nPreserve the blue-ocean marker.\n",
        )
        .expect("write project instruction");
        for host in [PromptHost::Interactive, PromptHost::Headless] {
            let prompts = [AppMode::Plan, AppMode::Agent, AppMode::Operate].map(|mode| {
                system_prompt_flat_text(
                    &system_prompt_for_mode_with_context_skills_session_and_approval_for_host(
                        tmp.path(),
                        None,
                        None,
                        None,
                        PromptSessionContext {
                            mode,
                            ..PromptSessionContext::default()
                        },
                        host,
                    ),
                )
            });
            assert_eq!(prompts[0], prompts[1]);
            assert_eq!(prompts[1], prompts[2]);
            assert!(prompts[0].contains("Preserve the blue-ocean marker"));
            assert!(!prompts[0].contains("##### Mode:"));
            if host == PromptHost::Headless {
                // One base prompt for every host; headless still omits the
                // interactive ceremony layers.
                assert!(prompts[0].contains("The A is already yours"));
                assert!(!prompts[0].contains("## Core Execution"));
                assert!(!prompts[0].contains("## Authority Recap"));
            }
        }
    }

    #[test]
    fn base_prompt_carries_constitutional_core() {
        for phrase in [
            "## Codewhale",
            "You are Codewhale",
            "The A is already yours",
            "Let the work speak",
            "### Ground truth",
            "### User intent and scope",
            "### Truthful completion",
            "### Put guarantees in mechanism",
            "### Whose word wins",
        ] {
            assert!(
                BASE_PROMPT.contains(phrase),
                "BASE_PROMPT missing Constitutional phrase {phrase:?}"
            );
        }
    }

    #[test]
    fn constitutional_kernel_keeps_first_turn_authority_safety_and_completion() {
        let fresh_prefix = compose_default_static_layers("deepseek-v4-pro");
        for phrase in [
            "Do what the user's current request asks, no more.",
            "require express user authorization in",
            "otherwise name the decision and ask.",
            "external publication, spending",
            "credentials, and material scope expansion",
            "prohibitions stay binding; convenience creates no exception",
            "never route around it or claim prose granted",
            "Nothing is done until checked.",
            "Read test output, not only exit status",
            "External actions are not complete until",
            "Work still running is not complete",
            "Never present a partial result as the whole.",
            "no one may tell you to invent one",
            "1. The user's request, this turn.",
            "2. This constitution.",
        ] {
            assert!(
                fresh_prefix.contains(phrase),
                "fresh constitution prefix missing kernel invariant {phrase:?}"
            );
        }
    }

    #[test]
    fn procedural_playbooks_are_not_eager_constitution() {
        let fresh_prefix = compose_default_static_layers("deepseek-v4-pro");
        for heading in [
            "### Keep momentum",
            "### Think in causes",
            "### Honor constraints before preferences",
            "### Skill and role constraints are binding",
            "### Restraint",
            "### Leave continuity",
        ] {
            assert!(
                !fresh_prefix.contains(heading),
                "procedural playbook should stay outside the full fresh prefix: {heading:?}"
            );
        }
        assert!(
            !BASE_PROMPT.contains("## STATUTES (Tier 2)")
                && !BASE_PROMPT.contains("## REGULATIONS (Tier 3)"),
            "the balanced Constitution must not restore the old procedural policy tail"
        );
    }

    #[test]
    fn base_prompt_carries_verify_then_stop_completion_contract() {
        // The completion contract behind "Truthful completion": verify with real
        // evidence, keep running work visible, and hand back exactly what
        // changed. These phrases encode the contract's semantics, not its
        // prose — a rewording that keeps the contract should keep these, and
        // one that drops them is a real behavior change worth failing review
        // for. (Constitution kernel rewrite in #5077 renamed the section and
        // condensed the prose; the contract stands.)
        for phrase in [
            "Nothing is done until checked.",
            "Read test output, not only exit status",
            "Work still running is not complete",
            "Never present a partial result as the whole.",
        ] {
            assert!(
                BASE_PROMPT.contains(phrase),
                "BASE_PROMPT missing completion-contract phrase {phrase:?}"
            );
        }
    }

    #[test]
    fn full_access_posture_uses_the_shared_completion_contract() {
        // `codewhale exec --auto` runs Act with the Full Access posture; the
        // verify-then-stop contract must survive composition into the prompt
        // that posture ships.
        let tmp = tempdir().expect("tempdir");
        let text = system_prompt_flat_text(
            &system_prompt_for_mode_with_context_skills_session_and_approval(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ),
        );
        for phrase in [
            "### Truthful completion",
            "Nothing is done until checked.",
            "Never present a partial result as the whole.",
        ] {
            assert!(
                text.contains(phrase),
                "YOLO-mode composed prompt missing completion-contract phrase {phrase:?}"
            );
        }
        assert!(!text.contains("##### Mode:"));
    }

    #[test]
    fn constitutional_hierarchy_keeps_user_turn_above_local_law() {
        let heading_at = BASE_PROMPT
            .find("### Whose word wins")
            .expect("Whose word wins heading present");
        let user_at = BASE_PROMPT
            .find("1. The user's request, this turn.")
            .expect("user request tier present");
        let constitution_at = BASE_PROMPT
            .find("2. This constitution.")
            .expect("constitution tier present");
        let project_at = BASE_PROMPT
            .find("3. Project law and instructions")
            .expect("project tier present");
        let preference_at = BASE_PROMPT
            .find("4. Your standing user-global preferences.")
            .expect("user-global preference tier present");
        let memory_at = BASE_PROMPT
            .find("5. Memory and previous-session handoffs.")
            .expect("memory/handoff tier present");

        assert!(
            heading_at < user_at
                && user_at < constitution_at
                && constitution_at < project_at
                && project_at < preference_at
                && preference_at < memory_at,
            "Whose word wins must rank the current user request above constitution, \
             project law, standing user-global preferences, then memory/handoffs"
        );
        assert!(
            BASE_PROMPT.contains("the user may override a fact, but no one may invent\none"),
            "Whose word wins must keep ground truth overridable but never inventable"
        );
        assert!(
            BASE_PROMPT.contains("A tie you cannot break is not yours to break"),
            "Whose word wins must keep tie-break escalation"
        );
    }

    #[test]
    fn base_prompt_is_model_fact_free() {
        for placeholder in [
            "{model_id}",
            "{context_window_note}",
            "{subagent_economics}",
            "{model_thinking_note}",
            "{model_characteristics}",
        ] {
            assert!(
                !BASE_PROMPT.contains(placeholder),
                "0.9.0 BASE_PROMPT must not contain model-fact placeholder {placeholder}"
            );
        }
        for forbidden in [
            "Your V4 Characteristics",
            "Model Characteristics",
            "one-million-token context window",
            "provider-dependent and not known",
        ] {
            assert!(
                !BASE_PROMPT.contains(forbidden),
                "0.9.0 BASE_PROMPT must not contain model-specific fact {forbidden:?}"
            );
        }
    }

    fn assert_no_unresolved_model_placeholders(prompt: &str) {
        for placeholder in [
            "{model_id}",
            "{context_window_note}",
            "{subagent_economics}",
            "{model_thinking_note}",
            "{model_characteristics}",
        ] {
            assert!(
                !prompt.contains(placeholder),
                "composed prompt must not contain unresolved {placeholder}"
            );
        }
    }

    #[test]
    fn compose_prompt_for_v4_model_stays_model_fact_free() {
        let prompt = compose_prompt_with_approval_model_and_shell("deepseek-v4-pro");
        assert!(prompt.contains("You are Codewhale"));
        assert!(!prompt.contains("Your V4 Characteristics"));
        assert!(!prompt.contains("one-million-token context window"));
        assert_no_unresolved_model_placeholders(&prompt);
    }

    #[test]
    fn compose_prompt_for_kimi_stays_model_fact_free() {
        let prompt = compose_prompt_with_approval_model_and_shell("moonshotai/kimi-k2.6");
        assert!(prompt.contains("You are Codewhale"));
        assert!(!prompt.contains("Your V4 Characteristics"));
        assert!(!prompt.contains("one-million"));
        assert!(!prompt.contains("$0.14"));
        assert!(!prompt.contains("262144-token context window"));
        assert!(!prompt.contains("Models may emit *thinking tokens*"));
        assert_no_unresolved_model_placeholders(&prompt);
    }

    #[test]
    fn compose_prompt_for_openai_api_gpt_55_stays_model_fact_free() {
        let prompt = compose_prompt_with_approval_model_and_shell("gpt-5.5");
        assert!(prompt.contains("You are Codewhale"));
        assert!(!prompt.contains("Your V4 Characteristics"));
        assert!(!prompt.contains("1050000-token context window"));
        assert!(!prompt.contains("Models may emit *thinking tokens*"));
        assert!(!prompt.contains("provider-dependent and not known"));
        assert_no_unresolved_model_placeholders(&prompt);
    }

    #[test]
    fn compose_prompt_for_unknown_model_stays_model_fact_free() {
        let prompt = compose_prompt_with_approval_model_and_shell("llama3.3:70b");
        assert!(prompt.contains("You are Codewhale"));
        assert!(!prompt.contains("Your V4 Characteristics"));
        assert!(!prompt.contains("one-million"));
        assert!(!prompt.contains("$0.14"));
        assert!(!prompt.contains("provider-dependent and not known"));
        assert!(!prompt.contains("Models may emit *thinking tokens*"));
        assert_no_unresolved_model_placeholders(&prompt);
    }

    #[test]
    fn apply_model_template_replaces_placeholder() {
        let result = apply_model_template("You are {model_id}", "deepseek-v4-pro", None);
        assert_eq!(result, "You are deepseek-v4-pro");
        assert!(!result.contains("{model_id}"));
    }

    #[test]
    fn apply_model_template_does_not_resolve_removed_model_fact_templates() {
        let result = apply_model_template("{context_window_note}", "gpt-5.5", Some(400_000));
        assert_eq!(result, "{context_window_note}");
        assert!(!result.contains("400000-token context window"));
        assert!(!result.contains("1050000-token context window"));
    }

    #[test]
    fn compose_prompt_is_model_agnostic_in_preamble() {
        // 0.9.0 keeps the preamble byte-for-byte the same regardless of
        // model id, and no {model_id} placeholder leaks.
        let flash = compose_prompt_with_approval_model_and_shell("deepseek-v4-flash");
        let kimi = compose_prompt_with_approval_model_and_shell("moonshotai/kimi-k2.6");
        assert!(
            flash.contains("You are Codewhale"),
            "0.9.0 preamble must open with the model-agnostic Codewhale stance"
        );
        assert!(
            !flash.contains("You are deepseek-v4-flash")
                && !kimi.contains("You are moonshotai/kimi-k2.6"),
            "0.9.0 preamble must not inject a per-model identity line"
        );
        assert!(
            !flash.contains("{model_id}") && !kimi.contains("{model_id}"),
            "composed prompt must not contain the raw {{model_id}} placeholder"
        );
    }

    #[test]
    fn locale_preambles_name_only_model_visible_tools() {
        for tag in ["zh-Hans", "ja", "pt-BR", "vi"] {
            let preamble = locale_reinforcement_preamble(tag).expect("preamble exists");
            assert!(
                preamble.contains("`read`") && preamble.contains("`bash`"),
                "{tag} preamble must use model-visible tool names: {preamble:?}"
            );
            assert!(
                !preamble.contains("`File`") && !preamble.contains("`Bash`"),
                "{tag} preamble must not teach hidden compatibility names: {preamble:?}"
            );
        }
    }

    #[test]
    fn visible_tool_descriptions_do_not_point_at_hidden_file_or_bash() {
        use crate::tools::pandoc::PandocConvertTool;
        use crate::tools::tasks::TaskShellWaitTool;
        let surfaces = [
            (
                "handle_read",
                format!(
                    "{} {}",
                    HandleReadTool.description(),
                    HandleReadTool.input_schema()
                ),
            ),
            (
                "pandoc_convert",
                format!(
                    "{} {}",
                    PandocConvertTool.description(),
                    PandocConvertTool.input_schema()
                ),
            ),
            (
                "task_shell_wait",
                format!(
                    "{} {}",
                    TaskShellWaitTool.description(),
                    TaskShellWaitTool.input_schema()
                ),
            ),
        ];
        for (name, text) in surfaces {
            assert!(
                !text.contains("File action=") && !text.contains("`Bash`"),
                "{name} must not point models at the hidden File/Bash tools: {text}"
            );
        }
        assert!(HandleReadTool.description().contains("`read` (path=...)"));
    }

    #[test]
    fn tool_descriptions_carry_edit_and_shell_guidance() {
        let write = WriteFileTool.description();
        assert!(
            write.contains("Unlike heredocs")
                && write.contains("`Bash`")
                && !write.contains("exec_shell"),
            "write guidance must name the live Bash tool and never the retired exec_shell name"
        );

        let edit = EditFileTool.description();
        // Every handler description must name the live `File` surface plus an
        // action. `read_file`/`write_file`/`apply_patch` are retired spellings
        // (crates/engine/src/tools/registry.rs:2066-2088).
        assert!(edit.contains("File `read`"));
        assert!(edit.contains("File `patch` handles structural"));
        assert!(!edit.contains("instead"), "{edit}");
        assert!(
            !edit.contains("read_file")
                && !edit.contains("write_file")
                && !edit.contains("apply_patch"),
            "edit guidance must not teach a retired tool name: {edit:?}"
        );

        let patch = ApplyPatchTool.description();
        assert!(patch.contains("unified-diff") && patch.contains("transactional"));

        let shell_tool = BashTool::new("Bash");
        let shell = shell_tool.description();
        assert!(shell.contains("background=true"));
        assert!(shell.contains(">5 seconds"));
    }

    #[test]
    fn composed_prompt_does_not_claim_tool_availability() {
        let prompt = compose_prompt_with_approval_model_and_shell("deepseek-v4-pro");
        assert!(!prompt.contains("## Core Tool Taxonomy"));
        assert!(!prompt.contains("## Toolbox"));
        assert!(prompt.contains("You are Codewhale"));
    }

    #[test]
    fn authority_recap_appears_in_full_prompt() {
        let tmp = tempdir().expect("tempdir");
        let text = system_prompt_flat_text(
            &system_prompt_for_mode_with_context_skills_session_and_approval(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext::default(),
            ),
        );
        assert!(
            text.contains("## Authority Recap"),
            "full system prompt must contain the authority recap"
        );
        assert!(
            text.contains("Codewhale's constitution governs your behavior"),
            "authority recap must reference the Constitution"
        );
        assert!(
            text.contains("consult ### Whose word wins"),
            "authority recap must point at 0.9.0's precedence section"
        );
    }

    #[test]
    fn system_prompt_merges_workspace_and_configured_skills_dir() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _home = ScopedHome::set(tmp.path().join("home"));
        let workspace = tmp.path().join("workspace");
        crate::test_support::trust_workspace(&workspace);
        let configured_dir = tmp.path().join("configured-skills");
        write_test_skill(
            &workspace.join(".claude").join("skills"),
            "workspace-skill",
            "workspace skill",
        );
        write_test_skill(&configured_dir, "configured-skill", "configured skill");

        let text = system_prompt_flat_text(&system_prompt_for_mode_with_context_and_skills(
            &workspace,
            None,
            Some(&configured_dir),
            None,
            None,
        ));

        assert!(text.contains("workspace-skill"));
        assert!(text.contains("configured-skill"));
    }

    struct ScopedHome {
        previous: Option<std::ffi::OsString>,
    }

    impl ScopedHome {
        fn set(path: std::path::PathBuf) -> Self {
            let previous = std::env::var_os("HOME");
            // Safety: this test serializes environment access with
            // lock_test_env and restores HOME in Drop.
            unsafe {
                std::env::set_var("HOME", path);
            }
            Self { previous }
        }
    }

    impl Drop for ScopedHome {
        fn drop(&mut self) {
            // Safety: this test serializes environment access with
            // lock_test_env and restores HOME in Drop.
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var("HOME", previous);
                } else {
                    std::env::remove_var("HOME");
                }
            }
        }
    }

    fn write_test_skill(root: &std::path::Path, name: &str, description: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .expect("skill file");
    }

    #[test]
    fn constitution_carries_tone_and_rejection_behavior() {
        // Voice and tone live in the compact constitution's preamble.
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(
            prompt.contains("Take the work seriously. Don't take"),
            "Preamble should carry tone guidance (take the work, not yourself, seriously)"
        );
        // Verify the preamble still carries the Codewhale identity.
        assert!(prompt.contains("You are Codewhale"));
        assert!(prompt.contains("Let the work speak"));
    }

    #[test]
    fn render_environment_block_keeps_actionable_host_facts_without_version() {
        let tmp = tempdir().expect("tempdir");
        let block = render_environment_block(tmp.path(), "zh-Hans");
        assert!(block.starts_with("## Environment"));
        assert!(block.contains("- lang: zh-Hans"));
        // The workspace remains per-turn and the release version is telemetry;
        // platform and shell still steer valid command syntax.
        assert!(!block.contains("- pwd:"));
        assert!(!block.contains("- codewhale_version:"));
        assert!(block.contains("- platform:"));
        assert!(block.contains("- shell:"));
    }

    #[test]
    fn locale_reinforcement_preamble_returns_native_script_for_supported_locales() {
        // English (and unknown locales) get None — the existing English
        // directive in `constitution.md` is sufficient.
        assert!(locale_reinforcement_preamble("en").is_none());
        assert!(locale_reinforcement_preamble("en-US").is_none());
        assert!(locale_reinforcement_preamble("fr-FR").is_none());
        assert!(locale_reinforcement_preamble("").is_none());

        // zh-Hans (and the de-facto equivalents the TUI accepts) get a
        // native-script preamble. The text must explicitly mention
        // `reasoning_content` (the V4 knob this is meant to steer) and
        // preserve tool-name immutability — those are the load-bearing
        // claims behind the #1118 fix that someone could quietly
        // delete in a future translation pass.
        for tag in ["zh-Hans", "zh-CN", "zh"] {
            let preamble =
                locale_reinforcement_preamble(tag).expect("zh-Hans preamble should exist");
            assert!(
                preamble.contains("简体中文"),
                "zh preamble must be in Simplified Chinese: {preamble:?}"
            );
            assert!(
                preamble.contains("reasoning_content"),
                "zh preamble must steer reasoning_content: {preamble:?}"
            );
            assert!(
                preamble.contains("`read`") && preamble.contains("`bash`"),
                "zh preamble must call out tool-name immutability with a model-visible \
                 tool name: {preamble:?}"
            );
            assert!(
                !preamble.contains("`File`") && !preamble.contains("`Bash`"),
                "zh preamble must not teach the hidden compatibility `File`/`Bash` \
                 names: {preamble:?}"
            );
            assert!(
                !preamble.contains("read_file") && !preamble.contains("exec_shell"),
                "zh preamble must never teach a retired tool name: {preamble:?}"
            );
        }

        let ja = locale_reinforcement_preamble("ja").expect("ja preamble");
        assert!(ja.contains("日本語"), "ja preamble must be in Japanese");
        assert!(ja.contains("reasoning_content"));

        let pt = locale_reinforcement_preamble("pt-BR").expect("pt-BR preamble");
        assert!(
            pt.contains("português do Brasil"),
            "pt preamble must call out pt-BR explicitly"
        );
        assert!(pt.contains("reasoning_content"));
    }

    #[test]
    fn system_prompt_prepends_locale_preamble_for_zh_hans() {
        // Build the full system prompt with locale=zh-Hans and assert
        // the native-script preamble shows up *before* the English
        // base-prompt body. Cache stability and attention precedence
        // both depend on this ordering.
        let tmp = tempdir().expect("tempdir");
        let text = system_prompt_flat_text(
            &system_prompt_for_mode_with_context_skills_session_and_approval(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "zh-Hans",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ),
        );
        let preamble_marker = "## 语言要求";
        let base_marker = "You are Codewhale";
        let preamble_pos = text
            .find(preamble_marker)
            .expect("zh-Hans preamble should be present");
        let base_pos = text
            .find(base_marker)
            .expect("base prompt should be present");
        assert!(
            preamble_pos < base_pos,
            "locale preamble must precede the English base prompt (preamble={preamble_pos}, base={base_pos})",
        );
    }

    #[test]
    fn locale_reinforcement_closer_returns_native_script_for_supported_locales() {
        // English (and unknown locales) get None.
        assert!(locale_reinforcement_closer("en").is_none());
        assert!(locale_reinforcement_closer("fr-FR").is_none());
        assert!(locale_reinforcement_closer("").is_none());

        // Each supported locale gets a closer in its own script that
        // explicitly tells the model "don't drift to English even as
        // English context accumulates" — that's the load-bearing claim
        // behind the bookend pattern.
        let zh = locale_reinforcement_closer("zh-Hans").expect("zh closer");
        assert!(
            zh.contains("简体中文"),
            "zh closer must be in Simplified Chinese"
        );
        assert!(
            zh.contains("reasoning_content"),
            "zh closer must steer reasoning_content"
        );
        let ja = locale_reinforcement_closer("ja").expect("ja closer");
        assert!(ja.contains("日本語"), "ja closer must be in Japanese");
        assert!(ja.contains("reasoning_content"));
        let pt = locale_reinforcement_closer("pt-BR").expect("pt-BR closer");
        assert!(pt.contains("português do Brasil"));
        assert!(pt.contains("reasoning_content"));
    }

    #[test]
    fn v092_locales_add_no_prompt_bookends_so_prompt_bytes_stay_stable() {
        // Cache-stability contract: adding the v0.9.2 UI locales
        // (ca, de, fr, id, hi, ru, uk) — and the already-shipped UI packs
        // that never had bookends (ko, es-419, zh-Hant) — must not change
        // the model-visible system prompt for an identical route/session
        // when translation is not explicitly enabled. The bookend list
        // stays intentionally short (zh-Hans, ja, pt-BR, vi); every other
        // shipped locale resolves to None and therefore renders the exact
        // same prompt bytes as English.
        for tag in [
            "zh-Hant", "ko", "es-419", "ca", "de", "fr", "id", "hi", "ru", "uk",
        ] {
            assert!(
                locale_reinforcement_preamble(tag).is_none(),
                "{tag} must not gain a locale preamble"
            );
            assert!(
                locale_reinforcement_closer(tag).is_none(),
                "{tag} must not gain a locale closer"
            );
        }
        // The bookend set is exactly the original four locales — growing it
        // is a deliberate, reviewable prompt change, not a side effect of
        // adding a UI pack.
        for tag in ["zh-Hans", "ja", "pt-BR", "vi"] {
            assert!(
                locale_reinforcement_preamble(tag).is_some(),
                "{tag} lost its locale preamble"
            );
            assert!(
                locale_reinforcement_closer(tag).is_some(),
                "{tag} lost its locale closer"
            );
        }
    }

    #[test]
    fn translation_seam_names_every_shipped_locale_canonically() {
        // The translation output instruction is the declared model-facing
        // seam: it only enters the prompt when `translation_enabled` is
        // true. When it does, every shipped locale must be named
        // canonically (English name + endonym) — never silently "English".
        for locale in codewhale_localization::Locale::shipped() {
            assert_eq!(
                translation_target_language_for_tag(locale.tag()),
                locale.translation_target_name(),
                "{} translation seam drifted from the canonical locale name",
                locale.tag()
            );
        }
    }

    #[test]
    fn system_prompt_bookends_zh_hans_with_preamble_and_closer() {
        // The full system prompt for zh-Hans must contain BOTH the
        // opening preamble (`## 语言要求`) and the closing reinforcement
        // (`## 语言再次提醒`), with the closer appearing AFTER the
        // preamble — i.e. the prompt is "bookended" in native script,
        // matching the empirical finding from the WeChat thread that
        // motivated the closer.
        let tmp = tempdir().expect("tempdir");
        let text = system_prompt_flat_text(
            &system_prompt_for_mode_with_context_skills_session_and_approval(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "zh-Hans",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ),
        );
        let preamble_pos = text
            .find("## 语言要求")
            .expect("zh-Hans preamble must be in prompt");
        let closer_pos = text
            .find("## 语言再次提醒")
            .expect("zh-Hans closer must be in prompt");
        assert!(
            preamble_pos < closer_pos,
            "closer must come after preamble (preamble={preamble_pos}, closer={closer_pos})",
        );
        // The closer must be the very last block — anything else after
        // it defeats the recency-bias purpose. Skip the closer's own
        // `## ` header before scanning.
        let closer_header_end = closer_pos + "## 语言再次提醒".len();
        let after_closer_body = &text[closer_header_end..];
        assert!(
            !after_closer_body.contains("\n## "),
            "no other top-level section should follow the closer; got: {after_closer_body:?}",
        );
    }

    #[test]
    fn system_prompt_skips_locale_preamble_for_english() {
        // English locale → no preamble injected. Asserts the
        // "preamble is opt-in for non-English" invariant.
        let tmp = tempdir().expect("tempdir");
        let text = system_prompt_flat_text(
            &system_prompt_for_mode_with_context_skills_session_and_approval(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ),
        );
        assert!(
            !text.contains("语言要求"),
            "English locale must not get a zh preamble: {text:?}"
        );
        assert!(
            !text.contains("言語要件"),
            "English locale must not get a ja preamble: {text:?}"
        );
        assert!(
            !text.contains("Requisito de Idioma"),
            "English locale must not get a pt-BR preamble: {text:?}"
        );
        // Closer too — same bookend rule.
        assert!(
            !text.contains("语言再次提醒"),
            "English locale must not get a zh closer: {text:?}"
        );
        assert!(
            !text.contains("言語再確認"),
            "English locale must not get a ja closer: {text:?}"
        );
        assert!(
            !text.contains("Reforço de Idioma"),
            "English locale must not get a pt-BR closer: {text:?}"
        );
        assert!(
            !contains_cjk(BASE_PROMPT),
            "base prompt must not contain static CJK priming tokens"
        );
        // Do not assert on arbitrary CJK in the full system prompt: project
        // context may legitimately contain localized file names, README text,
        // or user-authored instructions. The locale bookend markers above are
        // the priming tokens this test is meant to guard.
    }

    #[test]
    fn locale_bookends_carry_reasoning_content_directives_for_1118() {
        // #1118 ("Language has been configured to Chinese, but thinking
        // outputs are still in English"): after the 0.9.0 constitution
        // reduction, locale-native bookends carry the runtime language
        // reinforcement instead of the base constitution.
        let lang = LOCALE_PREAMBLE_ZH_HANS;
        assert!(
            lang.contains("reasoning_content"),
            "locale preamble must explicitly call out reasoning_content"
        );
        assert!(
            lang.contains("最终回复"),
            "locale preamble must explicitly cover the final reply"
        );
        assert!(
            lang.contains("代码") && lang.contains("工具名称"),
            "code and tool names must be named as non-language signals"
        );
        assert!(
            LOCALE_CLOSER_ZH_HANS.contains("reasoning_content")
                && LOCALE_CLOSER_ZH_HANS.contains("继续用简体中文思考和回答"),
            "closing bookend must preserve recency-positioned language reinforcement"
        );
        // Explicit-user-override clause keeps the prompt useful for the
        // opposite preference (#1118 commenters who want English
        // thinking for token-cost reasons).
        let phrase = "think in English";
        assert!(
            lang.contains(phrase) && LOCALE_CLOSER_ZH_HANS.contains(phrase),
            "expected the user-override example `{phrase}`"
        );
    }

    #[test]
    fn environment_block_is_inserted_into_system_prompt() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "ja",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));
        assert!(prompt.contains("## Environment"));
        assert!(prompt.contains("- lang: ja"));
        assert!(!prompt.contains("- codewhale_version:"));
        assert!(prompt.contains("- platform:"));
        assert!(prompt.contains("- shell:"));
    }

    #[test]
    fn user_global_constitution_is_captured_outside_the_stable_prefix() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let codewhale_home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&codewhale_home).expect("codewhale home");
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());

        let constitution = codewhale_config::UserConstitution {
            about: Some("Maintains Codewhale release lanes.".to_string()),
            working_style: vec!["Prefer live verification before claims.".to_string()],
            priorities: vec!["Keep release gates green.".to_string()],
            autonomy_preference: codewhale_config::AutonomyPreference::Balanced,
            ..codewhale_config::UserConstitution::default()
        };
        constitution
            .save_to(
                &codewhale_home
                    .join(codewhale_config::user_constitution::USER_CONSTITUTION_FILE_NAME),
            )
            .expect("save user constitution");

        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                &workspace,
                None,
                None,
                None,
                PromptSessionContext {
                    project_context_pack_enabled: false,
                    ..PromptSessionContext::default()
                },
            ));

        assert!(!prompt.contains("<codewhale_user_constitution"));
        let block = load_user_constitution_block().expect("admission snapshot");
        assert!(block.contains("Maintains Codewhale release lanes."));
        assert!(block.contains("Prefer live verification before claims."));
        assert!(!block.contains(&codewhale_home.display().to_string()));
    }

    #[test]
    fn bundled_choice_disables_user_global_constitution_block() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let codewhale_home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&codewhale_home).expect("codewhale home");
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());

        let constitution = codewhale_config::UserConstitution {
            about: Some("This file should stay inactive.".to_string()),
            ..codewhale_config::UserConstitution::default()
        };
        constitution
            .save_to(
                &codewhale_home
                    .join(codewhale_config::user_constitution::USER_CONSTITUTION_FILE_NAME),
            )
            .expect("save user constitution");

        let mut state = codewhale_config::SetupState::default();
        state.complete_constitution_checkpoint(
            crate::prompts::CONSTITUTION_CHECKPOINT_VERSION,
            codewhale_config::ConstitutionChoice::Bundled,
        );
        state
            .save_to(&codewhale_home.join(codewhale_config::setup_state::SETUP_STATE_FILE_NAME))
            .expect("save setup state");

        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                &workspace,
                None,
                None,
                None,
                PromptSessionContext {
                    project_context_pack_enabled: false,
                    ..PromptSessionContext::default()
                },
            ));

        assert!(!prompt.contains("<codewhale_user_constitution"));
        assert!(!prompt.contains("This file should stay inactive."));
    }

    #[test]
    fn invalid_user_global_constitution_is_skipped() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let codewhale_home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&codewhale_home).expect("codewhale home");
        std::fs::write(
            codewhale_home.join(codewhale_config::user_constitution::USER_CONSTITUTION_FILE_NAME),
            "{ not valid json",
        )
        .expect("write invalid user constitution");
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());

        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                &workspace,
                None,
                None,
                None,
                PromptSessionContext {
                    project_context_pack_enabled: false,
                    ..PromptSessionContext::default()
                },
            ));

        assert!(!prompt.contains("<codewhale_user_constitution"));
    }

    #[test]
    fn memory_guidance_carries_paired_examples() {
        // The fragment is the contract — verify the verbatim ✓ / ✗
        // pair is present so V4 has both shapes to imitate.
        assert!(MEMORY_GUIDANCE.contains("declarative facts"));
        assert!(MEMORY_GUIDANCE.contains(" ✓"));
        assert!(MEMORY_GUIDANCE.contains(" ✗"));
        assert!(MEMORY_GUIDANCE.contains("Imperative"));
    }

    #[test]
    fn memory_guidance_does_not_reference_scrapped_moraine() {
        // Moraine was scrapped for v0.9.4 (no in-repo server ever existed);
        // the native Markdown + SQLite FTS5 memory is the surviving system.
        assert!(!MEMORY_GUIDANCE.contains("Moraine"));
        assert!(!MEMORY_GUIDANCE.contains("moraine"));
    }

    #[test]
    fn memory_guidance_absent_when_no_memory_block() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));
        assert!(
            !prompt.contains("Memory Hygiene"),
            "memory guidance must not leak into sessions without a memory block"
        );
    }

    #[test]
    fn memory_guidance_appended_after_memory_block() {
        let tmp = tempdir().expect("tempdir");
        let block = "## User Memory\n\n- prefers Rust\n";
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: Some(block),
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));
        let mem_at = prompt.find("User Memory").expect("user memory present");
        let guide_at = prompt.find("Memory Hygiene").expect("guidance present");
        assert!(
            mem_at < guide_at,
            "guidance must come after the user memory block"
        );
    }

    #[test]
    fn continual_harness_is_injected_as_untrusted_world_state() {
        let tmp = tempdir().expect("tempdir");
        codewhale_runtime::continual_harness::refine(
            tmp.path(),
            codewhale_runtime::continual_harness::HarnessRefinement {
                kind: codewhale_runtime::continual_harness::HarnessEntryKind::PromptNote,
                title: "Verify release claims from direct evidence".to_string(),
                content: "Retain exact current command output for each release gate.".to_string(),
                evidence:
                    "A prior release report mixed stale hosted CI with newer local test output."
                        .to_string(),
            },
        )
        .expect("persist harness state");

        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));
        assert!(prompt.contains("<continual_harness trust=\"untrusted\">"));
        assert!(prompt.contains("supplemental working guidance"));
        assert!(prompt.contains("Verify release claims from direct evidence"));
    }

    #[test]
    fn headless_prompt_omits_continual_harness_guidance() {
        let tmp = tempdir().expect("tempdir");
        codewhale_runtime::continual_harness::refine(
            tmp.path(),
            codewhale_runtime::continual_harness::HarnessRefinement {
                kind: codewhale_runtime::continual_harness::HarnessEntryKind::PromptNote,
                title: "Use a project-specific orchestration routine".to_string(),
                content: "This guidance is available through the harness tool.".to_string(),
                evidence: "Prior interactive session.".to_string(),
            },
        )
        .expect("persist harness state");

        let prompt = system_prompt_flat_text(
            &system_prompt_for_mode_with_context_skills_session_and_approval_for_host(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext::default(),
                PromptHost::Headless,
            ),
        );
        assert!(!prompt.contains("<continual_harness"));
        assert!(!prompt.contains("project-specific orchestration routine"));
    }

    #[test]
    fn memory_guidance_does_not_state_precedence() {
        // #4777: only BASE_PROMPT § Whose word wins states ranks. Memory
        // hygiene keeps the imperative→preference rule and drops the
        // inverted Tier list that used to put Constitution above the user.
        let guidance = MEMORY_GUIDANCE.to_ascii_lowercase();
        for forbidden in [
            "tier 1",
            "tier 2",
            "tier 7",
            "statute",
            "regulation",
            "local law",
            "constitutional hierarchy",
        ] {
            assert!(
                !guidance.contains(forbidden),
                "MEMORY_GUIDANCE must not restate ranks (found {forbidden:?})"
            );
        }
        assert!(
            MEMORY_GUIDANCE.contains("treated as a preference")
                && MEMORY_GUIDANCE.contains("not a command"),
            "keep the imperative-as-preference rule"
        );
    }

    #[test]
    fn only_the_constitution_states_precedence() {
        // Composed overlays must describe behavior, never their own rank.
        let overlays = [
            ("COMPACT_TEMPLATE", COMPACT_TEMPLATE),
            ("MEMORY_GUIDANCE", MEMORY_GUIDANCE),
            ("LANGUAGE_PROMPT", LANGUAGE_PROMPT),
            ("OUTPUT_PROMPT", OUTPUT_PROMPT),
            ("AUTHORITY_RECAP", AUTHORITY_RECAP),
        ];
        let rank_markers = [
            "Tier 1",
            "Tier 2",
            "Tier 3",
            "Tier 4",
            "Tier 5",
            "Tier 6",
            "Tier 7",
            "Tier 8",
            "Tier 9",
            "Statute",
            "Article IV",
            "Article V",
            "Article VII",
            "Local Law",
            "Regulation (Tier",
        ];
        for (name, text) in overlays {
            for marker in rank_markers {
                assert!(
                    !text.contains(marker),
                    "{name} must not carry rank vocabulary {marker:?}"
                );
            }
        }
        assert!(
            BASE_PROMPT.contains("### Whose word wins"),
            "canonical precedence section must remain in BASE_PROMPT"
        );
        assert!(
            BASE_PROMPT.contains("This ordering is stated here and nowhere else"),
            "BASE_PROMPT must assert single-source precedence"
        );
    }

    #[test]
    fn project_context_pack_can_be_disabled() {
        let tmp = tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("README.md"), "# Pack test").expect("write readme");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));
        assert!(!prompt.contains("<project_context_pack>"));
    }

    #[test]
    fn project_context_pack_is_before_dynamic_tail() {
        let tmp = tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("README.md"), "# Pack test").expect("write readme");
        std::fs::create_dir_all(tmp.path().join(".deepseek")).expect("mkdir");
        std::fs::write(tmp.path().join(".deepseek").join("handoff.md"), "handoff")
            .expect("handoff");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    // Explicit opt-in — pack is off by default (#4781).
                    project_context_pack_enabled: true,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));
        assert!(prompt.contains("<project_context_pack>"));
        assert!(
            prompt.find("<project_context_pack>").expect("pack")
                < prompt.find("## Previous Session Relay").expect("relay")
        );
    }

    #[test]
    fn handoff_artifact_is_prepended_to_system_prompt_when_present() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let handoff_dir = workspace.join(".deepseek");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("handoff.md"),
            "# Session relay — prior\n\n## Active task\nFinish #32.\n\n## Open blockers\n- [ ] write the basic version\n",
        )
        .unwrap();

        let prompt = system_prompt_flat_text(&system_prompt_for_mode_with_context(workspace, None));

        assert!(prompt.contains(HANDOFF_BLOCK_MARKER));
        assert!(prompt.contains("Finish #32."));
        assert!(prompt.contains("write the basic version"));
        // Only the legacy file exists, so the block must name it rather
        // than send the model to a primary file that is not there.
        assert!(prompt.contains("left a relay artifact at `.deepseek/handoff.md`"));
        assert!(!prompt.contains("left a relay artifact at `.codewhale/handoff.md`"));
    }

    #[test]
    fn primary_handoff_wins_over_legacy_and_is_named() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        for (dir, body) in [
            (".codewhale", "# Session relay\n\nprimary relay body\n"),
            (".deepseek", "# Session relay\n\nlegacy relay body\n"),
        ] {
            std::fs::create_dir_all(workspace.join(dir)).unwrap();
            std::fs::write(workspace.join(dir).join("handoff.md"), body).unwrap();
        }

        let prompt = system_prompt_flat_text(&system_prompt_for_mode_with_context(workspace, None));

        assert!(prompt.contains("left a relay artifact at `.codewhale/handoff.md`"));
        assert!(prompt.contains("primary relay body"));
        assert!(!prompt.contains("legacy relay body"));
        assert!(!prompt.contains("left a relay artifact at `.deepseek/handoff.md`"));
    }

    #[test]
    fn missing_handoff_does_not_inject_block() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context(tmp.path(), None));
        assert!(!prompt.contains(HANDOFF_BLOCK_MARKER));
    }

    #[test]
    fn empty_handoff_file_does_not_inject_block() {
        let tmp = tempdir().expect("tempdir");
        let dir = tmp.path().join(".deepseek");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("handoff.md"), "   \n\n  ").unwrap();
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context(tmp.path(), None));
        assert!(!prompt.contains(HANDOFF_BLOCK_MARKER));
    }

    #[test]
    fn compose_prompt_includes_all_layers() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        // Base layer — balanced Constitution; procedural recipes stay out.
        assert!(prompt.contains("## Codewhale"));
        assert!(prompt.contains("### Whose word wins"));
        assert!(!prompt.contains("## STATUTES (Tier 2)"));
        assert!(!prompt.contains("## EVIDENCE (Tier 6)"));
        // Mode and approval are not inlined — they travel as
        // request-time runtime metadata.
        assert!(!prompt.contains("Mode: Agent"));
        assert!(!prompt.contains("Approval Policy:"));
    }

    /// `constitution.md` is the single hand-maintained source of the balanced
    /// constitutional core. This replaces the old 600-line policy tail: a
    /// hand-edit that drops a core section or reorders the skeleton fails the
    /// build instead of silently shipping a malformed prompt.
    #[test]
    fn constitution_md_carries_required_structure() {
        let md = BASE_PROMPT;
        assert!(md.contains("## Codewhale"), "missing title");
        let mut cursor = 0usize;
        for needle in [
            "## Codewhale",
            "### Ground truth",
            "### User intent and scope",
            "### Truthful completion",
            "### Put guarantees in mechanism",
            "### Whose word wins",
        ] {
            let pos = md
                .find(needle)
                .unwrap_or_else(|| panic!("ordering check: {needle:?} not found"));
            assert!(
                pos >= cursor,
                "cache-stable ordering broken: {needle:?} at {pos} precedes a previous section at {cursor}"
            );
            cursor = pos + needle.len();
        }
    }

    /// Gate against shipping a release with a missing CHANGELOG entry — which
    /// is exactly what happened with v0.8.21 / v0.8.22 (entries had to be
    /// backfilled in v0.8.23). Asserts the top-of-file CHANGELOG contains a
    /// `## [X.Y.Z]` heading matching the current `CARGO_PKG_VERSION`. No
    /// hardcoded version string — the test self-updates with the workspace
    /// version bump and only fires when the CHANGELOG is the missing piece.
    ///
    /// Walks up from `CARGO_MANIFEST_DIR` to find `CHANGELOG.md` instead of
    /// assuming a fixed `../../CHANGELOG.md` layout. The workspace root is
    /// the common case, but the walk also tolerates deeper crate layouts and
    /// the packaged-crate case (where the workspace root has been stripped
    /// out): if no `CHANGELOG.md` is reachable, the gate quietly skips
    /// rather than panicking, so consumers running the suite outside the
    /// workspace checkout don't see a spurious failure.
    #[test]
    fn changelog_entry_exists_for_current_package_version() {
        let version = env!("CARGO_PKG_VERSION");
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(changelog_path) = manifest_dir
            .ancestors()
            .map(|dir| dir.join("CHANGELOG.md"))
            .find(|candidate| candidate.is_file())
        else {
            eprintln!(
                "changelog_entry_exists_for_current_package_version: no \
                 CHANGELOG.md found above {} — skipping (this gate only \
                 fires inside a workspace checkout).",
                manifest_dir.display()
            );
            return;
        };

        let contents = std::fs::read_to_string(&changelog_path).unwrap_or_else(|err| {
            panic!(
                "failed to read CHANGELOG.md at {}: {err}",
                changelog_path.display()
            )
        });
        let header = format!("## [{version}]");
        assert!(
            contents.contains(&header),
            "CHANGELOG.md is missing a `{header}` entry for the current package \
             version. Add a release section at the top before tagging — see \
             docs/RELEASE_CHECKLIST.md."
        );
    }

    #[test]
    fn compose_prompt_deterministic_order() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        let base_pos = prompt.find("## Codewhale").unwrap();
        let article_pos = prompt.find("### Ground truth").unwrap();

        assert!(base_pos < article_pos);
    }

    #[test]
    fn base_prompt_is_mode_agnostic() {
        // Mode and approval text are no longer inlined into compose_prompt —
        // they travel as request-time runtime metadata.
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(!prompt.contains("Mode: Agent"));
        assert!(!prompt.contains("Mode: YOLO"));
        assert!(!prompt.contains("Mode: Plan"));
        assert!(!prompt.contains("Approval Policy:"));
        // Base prompt carries the 0.9.0 compact Constitution.
        assert!(prompt.contains("You are Codewhale"));
        assert!(prompt.contains("Take the work seriously. Don't take"));
    }

    #[test]
    fn approval_policy_no_longer_inlined_in_base_prompt() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(!prompt.contains("Mode: Agent"));
        assert!(!prompt.contains("Approval Policy:"));
        // The compact Constitutional preamble is still present.
        assert!(prompt.contains("You are Codewhale"));
    }

    #[test]
    fn execution_contract_states_proposal_is_not_execution() {
        // #5146: the live execution layer must make the propose-vs-execute
        // contract explicit after the legacy approval overlay was removed.
        assert!(
            CORE_EXECUTION_PROFILE_PROMPT.contains("is the proposal, not the execution"),
            "Execution profile must state the propose-vs-execute contract"
        );
        assert!(
            CORE_EXECUTION_PROFILE_PROMPT.contains("present the change in your plan"),
            "Execution profile must name the correct behavior on rejection"
        );
    }

    #[test]
    fn compact_template_is_lazy_in_fresh_prompt() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context(tmp.path(), None));
        assert!(!prompt.contains("# Session relay"));
        assert!(!prompt.contains("## Verification"));
    }

    #[test]
    fn session_goal_stays_volatile_while_compact_template_is_lazy() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                Some("## Repo Working Set\nsrc/lib.rs"),
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: Some("Fix transcript corruption"),
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));

        let goal_pos = prompt.find("<session_goal>").expect("goal block");
        assert!(prompt.contains("Fix transcript corruption"));
        // Session goal remains volatile content below the stable static
        // layers. The relay template is injected only when relay/compaction
        // actually needs it.
        assert!(goal_pos > 0);
        assert!(!prompt.contains("# Session relay"));
        assert!(!prompt.contains("src/lib.rs"));
    }

    #[test]
    fn empty_session_goal_is_not_injected() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: Some("   "),
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));

        assert!(!prompt.contains("<session_goal>"));
        assert!(!prompt.contains("## Current Goal"));
    }

    #[test]
    fn recovery_hint_renders_only_when_present() {
        // Prompt assembly reads env-dependent paths (skills, memory, session
        // state); the byte-equality check must serialize against env-guard
        // tests in the same binary.
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let build = |recovery_hint: Option<&str>| {
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: None,
                    recovery_hint,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    mode: AppMode::Agent,
                },
            ))
        };

        let hinted = build(Some(
            "A previous session (\"fix\", id abc12345) has a recovery checkpoint",
        ));
        assert!(hinted.contains("## Prior Session"));
        assert!(hinted.contains("<session_recovery>"));
        assert!(hinted.contains("recovery checkpoint"));

        // Clean sessions share identical prefix bytes: no block, no heading.
        let clean = build(None);
        assert!(!clean.contains("## Prior Session"));
        assert!(!clean.contains("session_recovery"));
        let blank = build(Some("   "));
        for (i, (a, b)) in clean.lines().zip(blank.lines()).enumerate() {
            assert_eq!(a, b, "line {i} differs");
        }
        assert_eq!(
            clean.lines().count(),
            blank.lines().count(),
            "line counts differ"
        );
    }

    #[test]
    fn universal_prompt_leaves_tool_selection_to_the_catalog() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(!prompt.contains("Tool Selection Guide"));
        for forbidden in [
            "`File`",
            "`Git`",
            "`Run`",
            "`Bash`",
            "read_file",
            "git_status",
            "run_tests",
            "exec_shell",
            "When NOT to use certain tools",
            "Don't reach for",
        ] {
            assert!(!BASE_PROMPT.contains(forbidden));
        }
    }

    /// #588: after the 0.9.0 constitution reduction, language-mirroring
    /// reinforcement lives in its own static segment plus locale bookends.
    #[test]
    fn language_segment_present_outside_reduced_constitution() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(
            !BASE_PROMPT.contains("## Language"),
            "0.9.0 constitution.md should stay reduced; language belongs in its own segment"
        );
        assert!(
            LANGUAGE_PROMPT.contains("## Language") && prompt.contains("## Language"),
            "default static prompt must still include the language segment"
        );
        assert!(
            LANGUAGE_PROMPT.contains("latest user message")
                && LANGUAGE_PROMPT.contains("fallback, not an override")
                && LANGUAGE_PROMPT.contains("localized READMEs")
                && LANGUAGE_PROMPT.contains("Use the `lang` field only when")
                && LANGUAGE_PROMPT.contains("constitution and other system law stay English"),
            "language segment must keep the mirror contract while staying short (#4784)"
        );
        assert!(
            LANGUAGE_PROMPT.contains("reasoning_content")
                && prompt.contains("reasoning_content")
                && LOCALE_PREAMBLE_ZH_HANS.contains("reasoning_content")
                && LOCALE_CLOSER_ZH_HANS.contains("reasoning_content"),
            "language segment and locale bookends must keep the reasoning_content anchor"
        );
    }

    #[test]
    fn output_formatting_segment_present_outside_reduced_constitution() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(
            !BASE_PROMPT.contains("## Output Formatting"),
            "0.9.0 constitution.md should stay reduced; output formatting belongs in its own segment"
        );
        assert!(OUTPUT_PROMPT.contains("## Output Formatting"));
        assert!(prompt.contains("## Output Formatting"));
        assert!(prompt.contains("terminal, not a browser"));
        assert!(prompt.contains("Markdown tables almost never render correctly"));
    }

    #[test]
    fn runtime_prompt_assembly_preserves_split_static_layers() {
        let tmp = tempdir().expect("tempdir");
        let prompt =
            system_prompt_flat_text(&system_prompt_for_mode_with_context_skills_and_session(
                tmp.path(),
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "glm-5.2",
                    context_window_override: Some(1_000_000),
                    verbosity: None,
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ));

        assert!(prompt.contains("## Codewhale"));
        assert!(prompt.contains("## Language"));
        assert!(prompt.contains("## Output Formatting"));
        assert!(prompt.contains("Use the `lang` field only when"));
    }

    #[test]
    fn locale_bookends_resist_english_context_drift() {
        assert!(
            LOCALE_PREAMBLE_ZH_HANS.contains("reasoning_content")
                && LOCALE_CLOSER_ZH_HANS.contains("reasoning_content"),
            "locale bookends must keep the reasoning_content anchor"
        );
        assert!(
            LOCALE_CLOSER_ZH_HANS.contains("英文代码")
                && LOCALE_CLOSER_ZH_HANS.contains("用户的语言决定"),
            "closing locale bookend must explicitly resist English-context drift"
        );
        assert!(
            LOCALE_PREAMBLE_ZH_HANS.contains("代码、文件路径、工具名称"),
            "opening locale bookend must keep code/tool tokens untranslated"
        );
    }

    #[test]
    fn english_base_prompt_avoids_native_script_language_priming() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(
            !contains_cjk(&prompt),
            "English base prompt should keep native-script reinforcement in locale bookends only"
        );
        assert!(
            !prompt.contains("multilingual coding agent"),
            "identity should not prime language switching; language belongs in runtime bookends"
        );
    }

    #[test]
    fn legacy_rlm_compatibility_descriptions_remain_available() {
        let descriptions = [
            RlmTool::alias("rlm_open", "open").description().to_string(),
            RlmTool::alias("rlm_eval", "eval").description().to_string(),
            RlmTool::alias("rlm_configure", "configure")
                .description()
                .to_string(),
            RlmTool::alias("rlm_close", "close")
                .description()
                .to_string(),
            HandleReadTool.description().to_string(),
        ]
        .join("\n");
        let rlm_count = descriptions.to_lowercase().matches("rlm").count();
        assert!(
            rlm_count >= 5,
            "RLM tool descriptions present: expected >= 5 mentions of 'rlm', got {rlm_count}"
        );
        assert!(!BASE_PROMPT.contains("`rlm`"));
    }

    /// Project instructions rank above memory, with the nearest scope winning
    /// over the broader. The embedder-injected-instructions case is covered
    /// by project law/instructions sitting above memory/handoffs.
    #[test]
    fn project_instructions_outrank_memory_in_whose_word_wins() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        let project_at = prompt
            .find("3. Project law and instructions")
            .expect("Whose word wins must rank project instructions");
        let memory_at = prompt
            .find("5. Memory and previous-session handoffs.")
            .expect("Whose word wins must rank memory below project instructions");
        assert!(
            project_at < memory_at,
            "project instructions must outrank memory so embedder-injected \
             instructions are not treated as mere memory preferences"
        );
    }

    #[test]
    fn workspace_orientation_guidance_present() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(prompt.contains("Project law and instructions"));
        assert!(
            prompt.contains("the nearest in\nscope winning over the broader")
                || prompt.contains("the nearest in scope winning over the broader"),
            "Whose word wins must keep the nearest-scope-wins rule for project instructions"
        );
    }

    #[test]
    fn prompt_documents_fork_context_prefix_cache_contract() {
        let source = include_str!("tools/subagent/mod.rs");
        assert!(source.contains("fork_context"));
        assert!(!BASE_PROMPT.contains("fork_context"));
    }

    #[test]
    fn prompt_documents_explicit_subagent_model_strength() {
        let source = include_str!("tools/subagent/mod.rs");
        assert!(source.contains("model_strength"));
        assert!(!BASE_PROMPT.contains("model_strength"));
    }

    #[test]
    fn prompt_documents_structured_subagent_briefs() {
        assert!(!BASE_PROMPT.contains("Subagent Brief"));
        for heading in [
            "### SUMMARY",
            "### EVIDENCE",
            "### CHANGES",
            "### RISKS",
            "### BLOCKERS",
        ] {
            assert!(text::SUBAGENT_OUTPUT_FORMAT.contains(heading));
        }
    }

    #[test]
    fn universal_prompt_does_not_invent_orchestration_limits() {
        assert!(!BASE_PROMPT.contains("3-5 tool calls"));
        assert!(!BASE_PROMPT.contains("No fan-out without a fan-in owner"));
    }

    #[test]
    fn universal_prompt_does_not_teach_optional_workflow_recipes() {
        for recipe in [
            "Workflow",
            "responseSchema",
            "request_user_input",
            ".workflow.js",
        ] {
            assert!(!BASE_PROMPT.contains(recipe));
        }
    }

    #[test]
    fn universal_prompt_does_not_expose_control_plane_ceremony() {
        for internal in [
            "sub-agent",
            "completion sentinels",
            "<codewhale:subagent.done>",
            "dispatch, join",
            "busy-waiting",
        ] {
            assert!(!BASE_PROMPT.contains(internal));
        }
    }

    #[test]
    fn preamble_carries_tone_and_ownership_guidance() {
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");
        assert!(prompt.contains("The A is already yours"));
        assert!(prompt.contains("Your competence is a settled fact"));
        assert!(prompt.contains("Take the work seriously. Don't take"));
        assert!(prompt.contains("Let the work speak"));
    }

    // ── Cache-prefix stability harness (#263 step 2) ───────────────────────
    //
    // These tests pin the byte-stability invariant required for DeepSeek's
    // KV prefix cache to hit: any prompt-construction surface that ends up
    // in the cached prefix must produce identical bytes given identical
    // inputs across calls.

    use crate::test_support::{EnvVarGuard, assert_byte_identical};

    #[test]
    fn compose_prompt_is_byte_stable_across_calls() {
        // Suspect #4 from #263: stable prompt churn within a single session.
        // Two calls with identical inputs must produce
        // identical bytes — anything else is a cache buster.
        let a = compose_prompt_with_approval_model_and_shell("codewhale");
        let b = compose_prompt_with_approval_model_and_shell("codewhale");
        assert_byte_identical("compose_prompt", &a, &b);
    }

    #[test]
    fn system_prompt_for_mode_with_context_is_byte_stable_for_unchanged_workspace() {
        // Same workspace, no working_set / skills churn between calls →
        // identical bytes. This pins the most representative production
        // surface (engine.rs builds the system prompt via this fn or
        // its sibling _and_skills variant on every turn).
        let _env_guard = crate::test_support::lock_test_env();
        let workspace_tmp = tempdir().expect("workspace tempdir");
        let home_tmp = tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", home_tmp.path().as_os_str());
        let _userprofile = EnvVarGuard::set("USERPROFILE", home_tmp.path().as_os_str());
        let _skills_dir = EnvVarGuard::remove("DEEPSEEK_SKILLS_DIR");
        let workspace = workspace_tmp.path();

        let a = system_prompt_flat_text(&system_prompt_for_mode_with_context(workspace, None));
        let b = system_prompt_flat_text(&system_prompt_for_mode_with_context(workspace, None));
        assert_byte_identical(
            "system_prompt_for_mode_with_context() on empty workspace",
            &a,
            &b,
        );
    }

    #[test]
    fn system_prompt_ignores_working_set_summary_argument() {
        // Working-set metadata is now injected into the latest user message
        // per turn. The legacy argument remains for call-site compatibility
        // but must not reintroduce volatile bytes into the system prompt.
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home_tmp = tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", home_tmp.path().as_os_str());
        let _userprofile = EnvVarGuard::set("USERPROFILE", home_tmp.path().as_os_str());
        let _skills_dir = EnvVarGuard::remove("DEEPSEEK_SKILLS_DIR");
        let workspace = tmp.path();
        let summary = "## Repo Working Set\nWorkspace: /tmp/x\n";

        let a = system_prompt_flat_text(&system_prompt_for_mode_with_context(
            workspace,
            Some(summary),
        ));
        let b = system_prompt_flat_text(&system_prompt_for_mode_with_context(
            workspace,
            Some(summary),
        ));
        assert_byte_identical(
            "system_prompt_for_mode_with_context with constant working_set summary",
            &a,
            &b,
        );
        assert!(
            !a.contains(summary),
            "summary must not be embedded in system prompt"
        );
    }

    #[test]
    fn oversized_relay_artifact_is_capped_like_an_instructions_file() {
        let tmp = tempdir().expect("tempdir");
        let dir = tmp.path().join(".codewhale");
        std::fs::create_dir_all(&dir).unwrap();
        // 300 KiB relay: well past the per-file prompt cap.
        std::fs::write(dir.join("handoff.md"), "R".repeat(300 * 1024)).unwrap();

        let block = super::load_handoff_block(tmp.path()).expect("relay block");
        assert!(block.contains("[…truncated:"), "truncation marker missing");
        assert!(
            block.len() < 110 * 1024,
            "relay must be capped near 100 KiB, got {} bytes",
            block.len()
        );
    }

    #[test]
    fn system_prompt_with_handoff_file_is_byte_stable_when_file_is_unchanged() {
        // If `.deepseek/handoff.md` hasn't moved between two builds, the
        // rendered prompt must produce identical bytes. The relay block
        // lands below the static boundary in
        // `system_prompt_for_mode_with_context_and_skills`.
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home_tmp = tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", home_tmp.path().as_os_str());
        let _userprofile = EnvVarGuard::set("USERPROFILE", home_tmp.path().as_os_str());
        let _skills_dir = EnvVarGuard::remove("DEEPSEEK_SKILLS_DIR");
        let workspace = tmp.path();
        let handoff_dir = workspace.join(".deepseek");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("handoff.md"),
            "# Session relay\n\n## Active task\nFinish #280.\n\n## Open blockers\n- [ ] none\n",
        )
        .unwrap();

        let a = system_prompt_flat_text(&system_prompt_for_mode_with_context(workspace, None));
        let b = system_prompt_flat_text(&system_prompt_for_mode_with_context(workspace, None));
        assert_byte_identical(
            "system_prompt_for_mode_with_context with constant handoff file",
            &a,
            &b,
        );
        assert!(a.contains(HANDOFF_BLOCK_MARKER), "relay must be embedded");
        assert!(a.contains("Finish #280."), "relay body must be present");
    }

    #[test]
    fn handoff_appears_after_static_blocks_without_working_set() {
        // Cache-prefix invariant: the relay artifact must come after static
        // `## Core Execution`. The relay template itself is now action-local,
        // not part of every system prompt. Working-set metadata is per-turn
        // user metadata, not a system-prompt tail block.
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let handoff_dir = workspace.join(".deepseek");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(handoff_dir.join("handoff.md"), "# handoff body\n").unwrap();

        let summary = "## Repo Working Set\nWorkspace: /tmp/x\n";
        let prompt = system_prompt_flat_text(&system_prompt_for_mode_with_context(
            workspace,
            Some(summary),
        ));

        let execution_pos = prompt
            .find("## Core Execution")
            .expect("Core Execution section present in Agent mode");
        let handoff_pos = prompt
            .find(HANDOFF_BLOCK_MARKER)
            .expect("relay block present when fixture file exists");
        assert!(
            !prompt.contains("## Repo Working Set"),
            "working-set summary must stay out of the system prompt"
        );

        assert!(
            execution_pos < handoff_pos,
            "## Core Execution must precede the relay block"
        );
        assert!(!prompt.contains("# Session relay"));
    }

    /// #4632 — The system prompt prefix (the byte-stable part cached by
    /// inference servers) must never contain private content: absolute
    /// filesystem paths, API keys, or home-directory references.
    ///
    /// Pin `HOME`/`USERPROFILE` to a scratch dir (and hold the env barrier)
    /// so global `~/.codewhale/instructions.md` and home-resolved skills
    /// cannot leak their real absolute paths into the prompt under test.
    /// Without this a machine that has `~/.codewhale/instructions.md` fails
    /// the absolute-path assertion, and the test only passes in parallel
    /// runs when a sibling test happens to hold a temporary `HOME` guard at
    /// the same moment — process-global env, so the result must never
    /// depend on scheduling or the developer's machine.
    #[test]
    fn system_prompt_prefix_never_leaks_private_content() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home_tmp = tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", home_tmp.path().as_os_str());
        let _userprofile = EnvVarGuard::set("USERPROFILE", home_tmp.path().as_os_str());
        let _skills_dir = EnvVarGuard::remove("DEEPSEEK_SKILLS_DIR");
        let workspace = tmp.path();
        let prompt = match system_prompt_for_mode_with_context(workspace, None) {
            SystemPrompt::Text(text) => text,
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };

        // No absolute paths (Unix or Windows).
        let offending: Vec<&str> = prompt
            .lines()
            .filter(|line| {
                line.contains("/Users/") || line.contains("/home/") || line.contains("C:\\")
            })
            .collect();
        assert!(
            offending.is_empty(),
            "system prompt must not contain absolute user paths, found: {offending:?}"
        );
        // No API key patterns.
        assert!(
            !prompt.contains("sk-") && !prompt.contains("api_key") && !prompt.contains("API_KEY"),
            "system prompt must not contain API key material"
        );
        // The workspace path itself must not appear.
        assert!(
            !prompt.contains(workspace.to_str().unwrap_or("/nonexistent")),
            "system prompt must not embed the workspace path"
        );
    }

    fn test_tool(name: &str, description: &str) -> Tool {
        Tool {
            tool_type: Some("function".to_string()),
            name: name.to_string(),
            description: description.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    #[test]
    fn static_prefix_guard_measures_tool_schemas() {
        let prompt = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "constitution".to_string(),
            cache_control: None,
        }]);
        let without_tools = super::measure_static_prefix(Some(&prompt), &[]);
        let with_tools = super::measure_static_prefix(
            Some(&prompt),
            &[test_tool("read_file", "Read a file from the workspace")],
        );
        assert!(
            with_tools > without_tools,
            "tool schemas are part of the stable prefix ({with_tools} vs {without_tools})"
        );
    }

    #[test]
    fn prefix_guard_includes_memory_growth() {
        let stable = SystemBlock {
            block_type: "text".to_string(),
            text: "stable prefix".to_string(),
            cache_control: None,
        };
        let one_block = SystemPrompt::Blocks(vec![stable.clone()]);
        let with_volatile = SystemPrompt::Blocks(vec![
            stable,
            SystemBlock {
                block_type: "text".to_string(),
                text: "MEMORY_BLOCK ".repeat(2_000),
                cache_control: None,
            },
        ]);
        assert!(
            super::measure_static_prefix(Some(&with_volatile), &[])
                > super::measure_static_prefix(Some(&one_block), &[])
        );
    }

    #[test]
    fn relay_artifact_cap_unchanged() {
        // The relay keeps its own 100 KiB cap; the instruction ceiling comes
        // from `[instruction_budget] file_max_bytes` and must not change how a
        // relay artifact is read.
        assert_eq!(super::RELAY_ARTIFACT_MAX_BYTES, 100 * 1024);
    }

    #[test]
    fn configured_instructions_render_inside_the_instruction_block() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let present = workspace.join("present.md");
        std::fs::write(&present, "CONFIGURED_PRESENT").unwrap();
        let empty = workspace.join("empty.md");
        std::fs::write(&empty, "   \n\n").unwrap();
        let missing = workspace.join("missing.md");
        let configured = vec![
            super::InstructionSource::File(missing),
            super::InstructionSource::File(empty),
            super::InstructionSource::File(present.clone()),
            super::InstructionSource::Inline {
                name: "embedded:test".to_string(),
                content: "INLINE_PRESENT".to_string(),
            },
        ];
        let mut ctx = crate::project_context::load_project_context_with_parents(workspace);
        let budgets = crate::project_context::InstructionBudgets::defaults_for(Some(128_000));
        let assembly =
            crate::project_context::assemble_instruction_block(&mut ctx, &configured, &budgets);
        let block = assembly.block.expect("a block");
        assert!(block.contains("CONFIGURED_PRESENT"), "{block}");
        assert!(block.contains("INLINE_PRESENT"), "{block}");
        assert!(block.contains("source=\"embedded:test\""), "{block}");
        let present_at = block.find("CONFIGURED_PRESENT").unwrap();
        let inline_at = block.find("INLINE_PRESENT").unwrap();
        assert!(present_at < inline_at, "declared order is preserved");
        assert!(
            assembly
                .notices
                .iter()
                .any(|notice| notice.action == crate::project_context::InstructionAction::Skipped),
            "missing and empty entries are reported: {:?}",
            assembly.notices
        );
    }

    #[test]
    fn instructions_block_appears_in_system_prompt_when_configured() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let extra = workspace.join("extra-instructions.md");
        std::fs::write(&extra, "EXTRA_INSTRUCTIONS_MARKER_BODY").unwrap();

        let extra_source: super::InstructionSource = extra.clone().into();
        let prompt =
            system_prompt_flat_text(&super::system_prompt_for_mode_with_context_and_skills(
                workspace,
                None,
                None,
                Some(std::slice::from_ref(&extra_source)),
                None,
            ));

        assert!(
            prompt.contains("EXTRA_INSTRUCTIONS_MARKER_BODY"),
            "configured instructions file body must appear in the prompt"
        );
        assert!(
            prompt.contains("source=\"extra-instructions.md\"")
                && !prompt.contains(&extra.display().to_string()),
            "instructions block must name its source without the host root"
        );
    }

    #[test]
    fn verbosity_concise_appends_discipline_block() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path();
        let prompt = system_prompt_flat_text(
            &super::system_prompt_for_mode_with_context_skills_session_and_approval(
                workspace,
                None,
                None,
                None,
                PromptSessionContext {
                    user_memory_block: None,
                    goal_objective: None,
                    project_context_pack_enabled: false,
                    locale_tag: "en",
                    translation_enabled: false,
                    model_id: "codewhale",
                    context_window_override: None,
                    verbosity: Some(" Concise "),
                    skills_config: Default::default(),
                    plugin_registry: None,
                    recovery_hint: None,
                    mode: AppMode::Agent,
                },
            ),
        );

        assert!(
            prompt.contains("## Concise Output Discipline"),
            "Concise Output Discipline should be appended"
        );
    }

    #[test]
    fn live_prompt_path_returns_world_state_blocks_with_markers() {
        let tmp = tempdir().expect("tempdir");
        let prompt = system_prompt_for_mode_with_context_skills_session_and_approval(
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                user_memory_block: Some("## Memory\n- remember the cutover"),
                goal_objective: Some("ship WorldState Blocks"),
                project_context_pack_enabled: false,
                locale_tag: "en",
                translation_enabled: false,
                model_id: "deepseek-v4-pro",
                context_window_override: None,
                verbosity: Some("concise"),
                skills_config: Default::default(),
                plugin_registry: None,
                recovery_hint: None,
                mode: AppMode::Agent,
            },
        );

        let SystemPrompt::Blocks(blocks) = prompt else {
            panic!("live prompt assembly must return SystemPrompt::Blocks");
        };
        assert!(
            blocks.len() >= 3,
            "constitution + at least one WorldState fragment + authority trailer"
        );
        assert!(
            !blocks[0].text.contains("<!-- cw:ctx:"),
            "constitution block must stay marker-free for prefix cache stability"
        );
        assert!(
            blocks[0].text.contains("## Core Execution"),
            "constitution retains static core execution guidance"
        );

        let flat = system_prompt_flat_text(&SystemPrompt::Blocks(blocks.clone()));
        assert!(flat.contains(codewhale_runtime::model_context::FragmentId::Workspace.marker()));
        assert!(flat.contains(codewhale_runtime::model_context::FragmentId::Route.marker()));
        assert!(flat.contains("## Environment"));
        assert!(
            flat.contains("verbosity: concise") && flat.contains("translation: off"),
            "route fragment keeps verbosity/translation but drops the model id"
        );
        assert!(!flat.contains("model: deepseek-v4-pro"));
        assert!(flat.contains("<session_goal>"));
        assert!(flat.contains("ship WorldState Blocks"));
        assert!(flat.contains("remember the cutover"));
        assert!(flat.contains("## Authority Recap"));
        assert!(
            !flat.contains(codewhale_runtime::model_context::FragmentId::SkillsTools.marker()),
            "skills remain in constitution, not a volatile SkillsTools fragment"
        );
    }

    #[test]
    fn live_prompt_world_state_diff_retains_unchanged_fragments() {
        let tmp = tempdir().expect("tempdir");
        let session = PromptSessionContext {
            user_memory_block: None,
            goal_objective: None,
            project_context_pack_enabled: false,
            locale_tag: "en",
            translation_enabled: false,
            model_id: "codewhale",
            context_window_override: None,
            verbosity: None,
            skills_config: Default::default(),
            plugin_registry: None,
            recovery_hint: None,
            mode: AppMode::Agent,
        };
        let first = system_prompt_for_mode_with_context_skills_session_and_approval(
            tmp.path(),
            None,
            None,
            None,
            session.clone(),
        );
        let second = system_prompt_for_mode_with_context_skills_session_and_approval(
            tmp.path(),
            None,
            None,
            None,
            PromptSessionContext {
                goal_objective: Some("only goal changed"),
                ..session
            },
        );

        let extract_world =
            |prompt: &SystemPrompt| -> codewhale_runtime::model_context::WorldState {
                let SystemPrompt::Blocks(blocks) = prompt else {
                    panic!("expected Blocks");
                };
                let mut state = codewhale_runtime::model_context::WorldState::new();
                for block in blocks.iter().skip(1) {
                    for id in codewhale_runtime::model_context::FragmentId::all() {
                        let marker = id.marker();
                        if let Some(rest) = block.text.strip_prefix(marker) {
                            let body = rest.trim_start_matches('\n');
                            state.upsert(
                                codewhale_runtime::model_context::ModelContextFragment::new(
                                    *id,
                                    id.role(),
                                    body,
                                ),
                            );
                        }
                    }
                }
                state
            };

        let previous = extract_world(&first);
        let next = extract_world(&second);
        let diff = next.render_diff(Some(&previous));
        assert!(
            diff.retained.iter().any(
                |marker| marker == codewhale_runtime::model_context::FragmentId::Route.marker()
            ),
            "unchanged route fragment must be retained: {diff:?}"
        );
        assert!(
            diff.updated
                .iter()
                .any(|fragment| fragment.id
                    == codewhale_runtime::model_context::FragmentId::Workspace),
            "goal change must update workspace fragment: {diff:?}"
        );
    }

    #[test]
    fn default_prompt_stays_under_2953_static_baseline() {
        const ISSUE_2953_BASELINE_CHARS: usize = 30_461;
        let prompt = compose_prompt_with_approval_model_and_shell("codewhale");

        assert!(
            prompt.chars().count() < ISSUE_2953_BASELINE_CHARS,
            "default static prompt should stay below the #2953 baseline"
        );
    }
}
#[test]
fn core_execution_profile_is_runtime_only() {
    for required in [
        "repository instructions",
        "inspect the narrow owner",
        "verify it",
        "Report changed files",
    ] {
        assert!(CORE_EXECUTION_PROFILE_PROMPT.contains(required));
    }
    for forbidden in [
        "footer",
        "color",
        "hotbar",
        "panel",
        "Fleet",
        "Workflow",
        "OpenHands",
    ] {
        assert!(!CORE_EXECUTION_PROFILE_PROMPT.contains(forbidden));
    }
}
