//! Settings system - Persistent user preferences
//!
//! Settings are stored at ~/.codewhale/settings.toml, with legacy fallbacks.
//!
//! There is one persisted settings store. The historical `tui.toml` second
//! store is folded into it on load and moved aside with a receipt — see
//! [`TuiPrefsMigration`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{expand_path, normalize_model_name};
use codewhale_config::resolve::Layer;
use codewhale_localization::normalize_configured_locale;
use codewhale_palette::{normalize_hex_rgb_color, normalize_theme_setting};

const SETTINGS_FILE_NAME: &str = "settings.toml";

/// Fresh terminal installs and explicit theme resets share one default.
use codewhale_config::settings_schema::DEFAULT_TUI_THEME;

/// Smallest Top work surface that can show its divider plus the compact
/// goal / to-do / Agent projection without turning the rail into invisible
/// keyboard state. Older releases accepted two rows, which left only one
/// content row and could hide every actionable item behind the goal title.
pub const WORK_SURFACE_TOP_HEIGHT_MIN: u16 = 5;
pub const WORK_SURFACE_TOP_HEIGHT_MAX: u16 = 16;
const TUI_PREFS_FILE_NAME: &str = "tui.toml";

/// How successful structured file mutations are represented in the live
/// transcript. Exact evidence is retained for inspection in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InlineDiffMode {
    /// Show a bounded red/green unified diff plus semantic change statistics.
    #[default]
    Full,
    /// Show only bounded semantic change statistics.
    Summary,
    /// Keep the calm File outcome row without any inline diff detail.
    Off,
}

impl InlineDiffMode {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "summary" => Self::Summary,
            "off" => Self::Off,
            _ => Self::Full,
        }
    }

    #[must_use]
    pub const fn as_setting(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Summary => "summary",
            Self::Off => "off",
        }
    }
}

// ============================================================================
// tui.toml — folded into settings.toml (0.9.12: one settings store)
// ============================================================================

/// What the one-time `tui.toml` fold did, so a session can say it out loud.
///
/// `tui.toml` used to be a second persisted store for `theme`, `font_size`,
/// and keybind overrides. Startup never read it, so a theme saved there could
/// disagree with `settings.toml` forever and nothing told the user which store
/// won. The file is now folded into `settings.toml` at load: a value
/// `settings.toml` does not already own explicitly is adopted, a value it does
/// own is reported as kept, and a key with no `Settings` field is quarantined
/// by name. The original bytes are moved to a dated backup — never deleted,
/// never silently dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuiPrefsMigration {
    /// The `tui.toml` that was folded.
    pub source: PathBuf,
    /// Where its original bytes now live.
    pub backup: Option<PathBuf>,
    /// `(settings key, adopted value)` folded into `settings.toml`.
    pub folded: Vec<(String, String)>,
    /// `(settings key, tui.toml value, settings.toml value)` — settings.toml
    /// already owned the key explicitly, so it won.
    pub kept: Vec<(String, String, String)>,
    /// `tui.toml` keys with no home in [`Settings`]. Listed, never dropped.
    pub quarantined: Vec<String>,
}

impl TuiPrefsMigration {
    /// Whether anything at all is worth telling the user about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.folded.is_empty() && self.kept.is_empty() && self.quarantined.is_empty()
    }

    /// One localized line per outcome, in the order a reader needs them:
    /// what moved, what did not, and what was parked.
    #[must_use]
    pub fn lines(&self, locale: codewhale_localization::Locale) -> Vec<String> {
        use codewhale_localization::{MessageId, tr};

        let backup = self
            .backup
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| self.source.display().to_string());
        let mut lines = Vec::new();
        for (key, value) in &self.folded {
            lines.push(
                tr(locale, MessageId::SettingsTuiPrefsFolded)
                    .replace("{key}", key)
                    .replace("{value}", value),
            );
        }
        for (key, from_prefs, from_settings) in &self.kept {
            lines.push(
                tr(locale, MessageId::SettingsTuiPrefsKept)
                    .replace("{key}", key)
                    .replace("{prefs}", from_prefs)
                    .replace("{settings}", from_settings),
            );
        }
        if !self.quarantined.is_empty() {
            lines.push(
                tr(locale, MessageId::SettingsTuiPrefsQuarantined)
                    .replace("{keys}", &self.quarantined.join(", "))
                    .replace("{path}", &backup),
            );
        }
        lines
    }
}

/// The `tui.toml` next to each settings candidate, first existing wins.
fn tui_prefs_path_from_settings_candidates(
    primary: Option<&Path>,
    legacy_home: Option<&Path>,
) -> Option<PathBuf> {
    [primary, legacy_home]
        .into_iter()
        .flatten()
        .map(|path| path.with_file_name(TUI_PREFS_FILE_NAME))
        .find(|path| path.exists())
}

/// Move `path` aside to `tui.toml.migrated-<YYYYMMDD>`, never clobbering an
/// existing backup. The bytes are preserved; only the name changes, so the
/// dead store cannot reappear as a second source of truth on the next launch.
fn back_up_tui_prefs(path: &Path) -> Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d").to_string();
    let base = format!("{TUI_PREFS_FILE_NAME}.migrated-{stamp}");
    let mut candidate = path.with_file_name(&base);
    let mut attempt = 1u32;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{base}-{attempt}"));
        attempt += 1;
    }
    std::fs::rename(path, &candidate)
        .with_context(|| format!("Failed to move {} aside", path.display()))?;
    Ok(candidate)
}

/// Fold a legacy `tui.toml` into `settings`, returning the receipt.
///
/// `explicit` reports whether `settings.toml` named a key itself; an explicit
/// value always wins, and the disagreement is recorded rather than resolved
/// behind the user's back. When `apply` is false (read-only diagnostics) the
/// values are still folded in memory but no file is moved or written.
fn fold_tui_prefs(
    settings: &mut Settings,
    explicit: &std::collections::BTreeSet<String>,
    prefs_path: &Path,
    apply: bool,
) -> Option<TuiPrefsMigration> {
    let raw = std::fs::read_to_string(prefs_path).ok()?;
    let mut receipt = TuiPrefsMigration {
        source: prefs_path.to_path_buf(),
        ..TuiPrefsMigration::default()
    };
    match toml::from_str::<toml::Value>(&raw) {
        Ok(toml::Value::Table(table)) => {
            for (key, value) in table {
                // `theme` is the only tui.toml key with a `Settings` field.
                // `font_size` and `[keybinds]` never had one, so they are
                // quarantined by name instead of being thrown away.
                if key != "theme" {
                    receipt.quarantined.push(key);
                    continue;
                }
                let Some(theme) = value.as_str().map(str::to_string) else {
                    receipt.quarantined.push(key);
                    continue;
                };
                let normalized = normalize_settings_theme(&theme);
                if explicit.contains("theme") {
                    if normalized != settings.theme {
                        receipt.kept.push((key, normalized, settings.theme.clone()));
                    }
                } else {
                    settings.theme = normalized.clone();
                    receipt.folded.push((key, normalized));
                }
            }
        }
        _ => {
            // Unreadable bytes are still the user's: park the whole file
            // under its own name rather than guessing at its contents.
            receipt.quarantined.push(TUI_PREFS_FILE_NAME.to_string());
        }
    }
    receipt.quarantined.sort();

    if apply {
        match back_up_tui_prefs(prefs_path) {
            Ok(backup) => receipt.backup = Some(backup),
            Err(error) => {
                tracing::warn!("failed to move {} aside: {error:#}", prefs_path.display());
            }
        }
    }
    Some(receipt)
}

/// User settings with defaults
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedModel {
    /// Exact configured provider identity; labels never replace this value.
    pub provider: String,
    /// Exact provider-owned model id.
    pub model: String,
    /// Optional presentation-only label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Auto-compact conversations when they approach the model limit.
    pub auto_compact: bool,
    /// Context-window percentage that triggers pre-send auto-compaction when
    /// `auto_compact` is enabled. The hard token floor still applies.
    pub auto_compact_threshold_percent: f64,
    /// Whether the persisted settings file expressed an auto-compaction
    /// preference. Runtime defaults must not be written back as user intent
    /// when an unrelated setting is saved.
    #[serde(skip)]
    pub auto_compact_explicit: bool,
    /// Reduce status noise and collapse details more aggressively
    pub calm_mode: bool,
    /// Use the pet as the main view on the next launch.
    pub pet_mode: bool,
    /// Dense tool-run collapse mode: compact, expanded, or calm.
    pub tool_collapse_mode: String,
    /// Reduce decorative motion. This must never synthesize model text speed;
    /// streaming follows upstream deltas in both modes.
    pub low_motion: bool,
    /// Set when the persisted file existed but could not be parsed and the
    /// values above are defaults. Never serialized; surfaces must not present
    /// these defaults as saved.
    #[serde(skip)]
    pub load_error: Option<String>,
    /// Enable expressive live-state motion. This affects chrome and state
    /// affordances only; model text always follows upstream stream deltas.
    pub fancy_animations: bool,
    /// Focus-context texture prototype for modal views (#4823): `off`
    /// (default), `scrim` dims the area outside the focused modal, `grain`
    /// sprinkles deterministic dots over blank cells there. Static texture,
    /// never obscures text; unknown values fall back to `off` at render time.
    pub focus_texture: String,
    /// Ocean Tasks / To-do / Workers rail placement: top, left, or right.
    /// The lower edge remains owned by the composer and phase footer.
    pub work_surface_placement: String,
    /// Remembered total height (content plus divider) for top Work placement.
    pub work_surface_top_height: u16,
    /// Remembered total width (content plus divider) for side Work placement.
    pub work_surface_side_width: u16,
    /// Which panel the rail shows: tasks, agents, context, or pinned.
    /// Orthogonal to `work_surface_placement` (rail unification, 0.9.4).
    pub rail_panel: String,
    /// Runtime-only: whether the loaded settings document explicitly named
    /// `rail_panel`. The sidebar→rail migration must not override an
    /// explicit choice that happens to equal the default ("tasks").
    #[serde(skip)]
    pub(crate) rail_panel_explicit: bool,
    /// Runtime-only: whether the loaded settings document explicitly named
    /// `work_surface_placement`. A legacy hidden sidebar must become `off`,
    /// unless the user had already chosen a first-class rail placement.
    #[serde(skip)]
    pub(crate) work_surface_placement_explicit: bool,
    /// Runtime-only 30 FPS cap for terminals that flicker at high redraw
    /// rates. Separate from accessibility motion and text delivery.
    #[serde(skip)]
    pub constrained_frame_rate: bool,
    /// Enable terminal bracketed-paste mode. Default true. Disable if your
    /// terminal mishandles the `\e[?2004h` escape (rare; some legacy
    /// terminals over SSH+screen multiplex without the cap).
    pub bracketed_paste: bool,
    /// Enable rapid-key paste-burst detection for terminals that do not emit
    /// bracketed-paste events. Independent from `bracketed_paste`.
    pub paste_burst_detection: bool,
    /// Maximum number of file-mention popup candidates retained before the
    /// composer renders its visible window. The widget paginates by terminal
    /// height, so this is a data-side cap rather than a visible-row budget.
    pub mention_menu_limit: usize,
    /// Maximum workspace depth for `@`-mention completion walks. `0` means
    /// unlimited depth; use with care in very large repositories.
    pub mention_walk_depth: usize,
    /// `@`-mention completion behavior: fuzzy workspace search or deterministic
    /// directory browser.
    pub mention_menu_behavior: String,
    /// Show thinking blocks from the model
    pub show_thinking: bool,
    /// When true, thinking blocks render expanded by default instead of
    /// collapsed. Space still toggles collapse/expand. Useful for SSH/tmux
    /// users where the Space key may be captured by the terminal layer.
    #[serde(default)]
    pub thinking_default_expanded: bool,
    /// Collapsed completed-thought preview rows. Default 2 (compact).
    /// Set `10` for the older dump, or `0` for header-only. Full expand is
    /// still `thinking_default_expanded` / Space.
    #[serde(default = "default_thinking_preview_lines")]
    pub thinking_preview_lines: usize,
    /// Keep thinking visible while disabling its filled background treatment.
    pub thinking_highlight: bool,
    /// When true, Help/shortcuts groups start expanded. Default false folds
    /// the long tail. Type-to-filter still unfolds matches.
    #[serde(default)]
    pub help_expand_groups: bool,
    /// Show quiet, action-triggered command discovery tips.
    #[serde(default = "default_true")]
    pub contextual_tips: bool,
    /// Pin the last user prompt at the top of the transcript when it has
    /// scrolled off. Default on.
    #[serde(default = "default_true")]
    pub pin_last_prompt: bool,
    /// Show detailed tool output
    pub show_tool_details: bool,
    /// Successful structured File mutation evidence: full, summary, or off.
    /// This affects inline presentation only; exact evidence remains available
    /// through the tool-details route in every mode.
    pub inline_diffs: String,
    /// UI locale: auto, en, ja, zh-Hans, zh-Hant, pt-BR, es-419, vi, ko,
    /// ca, de, fr, id, hi, ru, uk.
    /// Every shipped pack holds full `en.json` parity; nothing falls back.
    pub locale: String,
    /// Named UI theme. `"underwater"` is the fresh-install default: a dark
    /// navy water column. `"shoreline"` is the warm charcoal alternative.
    /// `"terminal"` fully inherits the
    /// host terminal's foreground/background. `"system"`, `"dark"`,
    /// `"light"`, `"grayscale"`, and the community presets:
    /// `"catppuccin-mocha"`, `"tokyo-night"`, `"dracula"`,
    /// `"gruvbox-dark"`. The `background_color` setting still overrides the
    /// surface color on top of the resolved theme.
    pub theme: String,
    /// Optional main TUI background color as a 6-digit hex RGB value.
    pub background_color: Option<String>,
    /// Composer layout density: compact, comfortable, spacious
    pub composer_density: String,
    /// Show a border around the composer input area
    pub composer_border: bool,
    /// Keep bare Enter available for multiline drafting. When enabled,
    /// Alt+Enter and supported Shift+Enter submit; Ctrl+J remains a newline shortcut.
    #[serde(default)]
    pub composer_multiline_mode: bool,
    /// Composer editing mode: "normal" (default) or "vim" for modal editing.
    /// When set to "vim" the composer starts in Normal mode; press i/a/o to
    /// enter Insert mode and Esc to return to Normal.
    pub composer_vim_mode: String,
    /// Transcript spacing rhythm: compact, comfortable, spacious
    pub transcript_spacing: String,
    /// Default mode: "agent" (Work), "plan", or "operate". Legacy permission
    /// shorthands are accepted for migration but never advertised as modes.
    pub default_mode: String,
    /// Legacy sidebar width as percentage of terminal width. Load-only
    /// migration shim (0.9.4 rail unification): read by
    /// `migrate_sidebar_settings_to_rail`, never written back.
    #[serde(skip_serializing)]
    pub sidebar_width_percent: u16,
    /// Legacy sidebar focus mode: pinned, auto, tasks, agents, context,
    /// hidden. Load-only migration shim, never written back.
    #[serde(skip_serializing)]
    pub sidebar_focus: String,
    /// Enable the session-context panel (#504). Shows working set, tokens,
    /// cost, MCP/LSP status, cycle count, and memory info.
    pub context_panel: bool,
    /// Show the persistent Sessions rail in the sidebar (#2934).
    ///
    /// Off by default: the rail spends sidebar rows that Work, Activity, and
    /// Agents already compete for, so it is opt-in rather than something a
    /// user discovers by having their layout change under them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sessions_rail: bool,
    /// Reattach to this workspace's most recent session on startup (#2934).
    ///
    /// Off by default. `--resume`/`--continue` remain the explicit paths and
    /// always take precedence; when this is on, startup still refuses to
    /// resume an archived, unreadable, or foreign-workspace session and falls
    /// back to a fresh transcript with a receipt. See
    /// `crate::session_resume` for the decision table.
    #[serde(default, skip_serializing_if = "is_false")]
    pub session_auto_resume: bool,
    /// Cost display currency: usd or cny.
    pub cost_currency: String,
    /// Maximum number of input history entries to save
    pub max_input_history: usize,
    /// Archived startup provider used only to migrate older settings into config.
    pub default_provider: Option<String>,
    /// Archived DeepSeek fallback used only by the config selection migration.
    pub default_model: Option<String>,
    /// Default reasoning effort selected from the TUI model picker.
    /// `None` falls back to `config.toml` and then the runtime default.
    pub reasoning_effort: Option<String>,
    /// TUI-only Shift+Tab posture: ask, auto-review, or full-access.
    /// An explicit/managed `config.toml` approval policy always takes
    /// precedence, so this preference cannot loosen project requirements.
    /// This is **tool-approval posture**, not filesystem scope — see
    /// [`Self::sandbox_mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<String>,
    /// Filesystem sandbox scope, independent of approval posture:
    /// `read-only | workspace-write | danger-full-access | external-sandbox`.
    /// Surfaced in Settings and the shell so "Full Access" (approval) is
    /// never confused with unrestricted filesystem writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<String>,
    /// Archived provider model choices used only by the config selection
    /// migration. Preserve them on unrelated settings saves until migrated.
    pub provider_models: Option<std::collections::HashMap<String, String>>,
    /// Legacy additive picker list written by older builds on every model
    /// switch. Nothing reads it any more (#6533): the picker ranks by recent
    /// use instead. Parsed and preserved only so old files load and survive
    /// unrelated saves until a cleanup removes the table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_models: Option<std::collections::HashMap<String, Vec<String>>>,
    /// Exact provider/model tuples pinned to the top of model choosers, in
    /// user-defined order. Stale entries remain persisted and visible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_models: Vec<PinnedModel>,
    /// Header status indicator next to the effort chip. Cycles through a
    /// per-turn animation keyed off `App::turn_started_at`:
    /// - `"cw"` (default): static typographic Codewhale mark.
    /// - `"whale"`: historical `🐳 → 🐋` 12-frame sequence
    ///   originally shipped in v0.3.5, removed in v0.8.x's "smoother TUI
    ///   streaming" pass, restored in v0.8.30. Idle frame is a steady `🐳`.
    /// - `"dots"`: the 6-frame geometric sequence (`◍ ◉ ◌ ◌ ◉ ◍`) that
    ///   replaced the whale during the dots era.
    /// - `"off"`: hide the indicator entirely.
    pub status_indicator: String,
    /// Whether to wrap each draw in DEC mode 2026 synchronized output
    /// (`\x1b[?2026h` … `\x1b[?2026l`). Synchronized output asks the
    /// terminal to defer rendering until the whole frame is staged so
    /// GPU-accelerated terminals (Ghostty, VS Code, Kitty, WezTerm)
    /// don't flash a blank intermediate frame.
    ///
    /// - `"auto"` (default): emit DEC 2026 unless an environment signal
    ///   says the active terminal mishandles it (currently Ptyxis 50.x
    ///   on VTE 0.84.x — see [`Settings::apply_env_overrides`]).
    /// - `"on"`: always emit DEC 2026 (override the auto opt-out).
    /// - `"off"`: never emit DEC 2026. Use this if your terminal flashes
    ///   the whole screen on every redraw — most often Ptyxis on
    ///   Ubuntu 26.04 today; historically also some legacy ssh+screen
    ///   stacks. The cost of `off` is brief tearing on terminals that
    ///   *do* support DEC 2026; it is purely a rendering-quality knob,
    ///   not a correctness one.
    pub synchronized_output: String,
    /// Follow symbolic links during workspace file discovery walks (`@`-mention
    /// completion, fuzzy resolve, and the file-index builder). When `false`
    /// (default) symlinked directories are skipped, which keeps walks fast and
    /// avoids accidentally traversing into system paths. Set to `true` to
    /// support symlink-based multi-project workspaces where several project
    /// directories are symlinked into a single hub directory.
    ///
    /// **Note**: The walker has built-in cycle detection that skips already-
    /// visited real paths, so symlink loops (A→B→A) will not cause infinite
    /// recursion. However, enabling this on workspaces with symlinks that
    /// point to large directory trees (e.g. `/usr`, home directories) can
    /// significantly increase first-turn latency and memory usage.
    pub workspace_follow_symlinks: bool,
    /// One-time Fleet + Hotbar introduction has been shown. Drives a single
    /// launch nudge (see `App::maybe_show_feature_intro`) so returning users
    /// see it exactly once and never on subsequent launches.
    pub feature_intro_shown: bool,
    /// One-time YOLO deprecation toast has been shown. Suppresses the repeat
    /// toast after the first sighting per install (persisted across sessions).
    pub yolo_deprecation_shown: bool,
    /// Round 3 (2026-09-01) moved the work bar under the composer. Every
    /// settings.toml saved before that carries `work_surface_placement =
    /// "top"` — the old default, persisted verbatim by ordinary saves, not a
    /// choice anyone made. This flag records that the one-time `top` →
    /// `bottom` migration ran, so a user who picks `top` afterwards keeps it.
    #[serde(default)]
    pub work_surface_bottom_migrated: bool,
    /// Persisted impression counts for action-triggered, ephemeral product
    /// guidance. Keys are stable tip identifiers; values are bounded by the
    /// behavioral-tip engine and omitted entirely before the first sighting.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub behavioral_tip_impressions: std::collections::BTreeMap<String, u8>,
    /// Plugin names explicitly dismissed from proactive suggestions. Manual
    /// plugin commands remain available. Names are stored in lowercase.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub dismissed_plugin_suggestions: std::collections::BTreeSet<String>,
    /// Persisted use counts for the Tideline footer key hints. Keys are the
    /// stable hint identifiers in `crate::tui::footer_hints`; a hint retires
    /// to its bare state once its binding has been used enough times.
    /// Omitted entirely before the first recorded use.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub footer_hint_uses: std::collections::BTreeMap<String, u8>,
    /// True only for the current load when `default_mode = "yolo"` was read
    /// from an older settings file. App startup uses this provenance to migrate
    /// the old bundled Full Access choice without weakening project or managed
    /// approval policy. It is never written back to disk.
    #[serde(skip)]
    pub legacy_yolo_default: bool,
    /// Receipt for the one-time `tui.toml` fold performed by this load.
    /// Never serialized: it describes what happened to a file, not a setting.
    #[serde(skip)]
    pub tui_prefs_migration: Option<TuiPrefsMigration>,
    /// Which layer supplied the in-force value of each schema key: user
    /// config for keys `settings.toml` named at load, the default for the
    /// rest, session for keys `set()` touched since. Runtime only — the
    /// resolver reads it, disk never sees it. CLI flags (2D) and managed
    /// policy / project producers mark their own layers when they land.
    #[serde(skip)]
    pub provenance: std::collections::BTreeMap<String, Layer>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Keep the persisted fallback `false`; startup code enables
            // auto-compaction by known model window when the user has not saved
            // an explicit preference. This preserves an explicit opt-out while
            // making long-session continuity the default runtime behavior.
            auto_compact: false,
            auto_compact_threshold_percent: 80.0,
            auto_compact_explicit: false,
            // #4095: default presentation is compact/calm; verbose detail is opt-in.
            calm_mode: true,
            pet_mode: false,
            tool_collapse_mode: "compact".to_string(),
            low_motion: false,
            load_error: None,
            fancy_animations: true,
            focus_texture: "off".to_string(),

            // Round 3 (2026-09-01): the bar's information lives under the
            // composer. Side rails are opt-in and fall back to the top strip
            // on narrow terminals.
            work_surface_placement: "bottom".to_string(),
            // Cap, not fixed height: the top strip auto-fits its rows and
            // only grows to this many lines (user request, 2026-07-23).
            work_surface_top_height: 8,
            work_surface_side_width: 30,
            rail_panel: "tasks".to_string(),
            rail_panel_explicit: false,
            work_surface_placement_explicit: false,
            constrained_frame_rate: false,
            bracketed_paste: true,
            paste_burst_detection: true,
            mention_menu_limit: 128,
            mention_walk_depth: 10,
            mention_menu_behavior: "fuzzy".to_string(),
            // Reasoning is useful when explicitly requested, but it should
            // never displace the actual conversation in the default TUI.
            show_thinking: false,
            thinking_default_expanded: false,
            thinking_preview_lines: default_thinking_preview_lines(),
            thinking_highlight: true,
            help_expand_groups: false,
            contextual_tips: true,
            pin_last_prompt: true,
            show_tool_details: false,
            inline_diffs: "full".to_string(),
            locale: "auto".to_string(),
            theme: DEFAULT_TUI_THEME.to_string(),
            background_color: None,
            composer_density: "comfortable".to_string(),
            composer_border: true,
            composer_multiline_mode: false,
            composer_vim_mode: "normal".to_string(),
            transcript_spacing: "spacious".to_string(),
            default_mode: "agent".to_string(),
            sidebar_width_percent: 28,
            sidebar_focus: "auto".to_string(),
            context_panel: false,
            sessions_rail: false,
            session_auto_resume: false,
            cost_currency: "usd".to_string(),
            max_input_history: 100,
            default_provider: None,
            default_model: None,
            reasoning_effort: None,
            permission_posture: None,
            sandbox_mode: None,
            provider_models: None,
            enabled_models: None,
            pinned_models: Vec::new(),
            // The whale lives in the terminal window title (OSC 0). The in-app
            // header defaults to the static typographic `cw` mark so the two
            // surfaces do not compete with a second spinner.
            status_indicator: "cw".to_string(),
            synchronized_output: "auto".to_string(),
            workspace_follow_symlinks: false,
            feature_intro_shown: false,
            yolo_deprecation_shown: false,
            work_surface_bottom_migrated: false,
            behavioral_tip_impressions: std::collections::BTreeMap::new(),
            dismissed_plugin_suggestions: std::collections::BTreeSet::new(),
            footer_hint_uses: std::collections::BTreeMap::new(),
            legacy_yolo_default: false,
            tui_prefs_migration: None,
            provenance: std::collections::BTreeMap::new(),
        }
    }
}

/// The `calm` transcript preset (#3478): a coherent "beautiful/calm" bundle that
/// favors a quiet, readable transcript over debug-dense output. Presentation
/// only, and evidence-preserving — `show_thinking` is deliberately left untouched
/// (thinking stays visible) and tool runs only have their inline detail
/// collapsed, never hidden. Keyed by [`Settings::set`] names so the preset and a
/// single-key `/config` set share one validation path.
pub const CALM_PRESET_FIELDS: &[(&str, &str)] = &[
    ("calm_mode", "true"),
    ("tool_collapse", "calm"),
    ("transcript_spacing", "compact"),
    ("low_motion", "true"),
    ("fancy_animations", "false"),
    ("show_tool_details", "false"),
];

fn normalize_work_surface_placement(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "top" => "top",
        "bottom" => "bottom",
        "left" => "left",
        "right" => "right",
        "off" => "off",
        // Round 3 (2026-09-01): unknown values fall back to the product
        // default — the bar lives under the composer.
        _ => "bottom",
    }
}

fn normalize_rail_panel(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "agents" => "agents",
        "background" => "background",
        "terminal" => "terminal",
        "files" => "files",
        "notepad" => "notepad",
        "context" => "context",
        "git" => "git",
        "price" => "price",
        "watch" => "watch",
        // `pinned` folded into the tasks view (2026-09-02 dock views).
        _ => "tasks",
    }
}

/// Rail unification (0.9.4): carry the classic sidebar's settings forward
/// instead of stranding them. `sidebar_focus` picks the rail panel —
/// pinned/tasks/agents/context map onto the same-named panels, auto folds
/// into the auto-fitting Tasks panel (it is the shipped default for
/// `sidebar_focus`, and "show work when there is work" is what Tasks does;
/// folding it into the always-on Pinned strip inverted that intent for every
/// upgrading user), and hidden turns the rail off.
/// `sidebar_width_percent` maps onto the absolute side width at a
/// 120-column reference. Auto-collapse itself is deliberately dropped: the
/// rail hides via placement off. Explicit new keys win over migrated ones.
fn migrate_sidebar_settings_to_rail(s: &mut Settings) {
    match s.sidebar_focus.trim().to_ascii_lowercase().as_str() {
        "hidden" | "hide" | "closed" | "off" | "none" => {
            // A legacy hidden sidebar is an explicit intent. Preserve it even
            // now that fresh sessions prefer the responsive left rail, but do
            // not override a newer placement the user explicitly saved.
            if !s.work_surface_placement_explicit {
                s.work_surface_placement = "off".to_string();
            }
        }
        // #5141 let users pin a dedicated sessions panel in the classic
        // sidebar; on the unified rail the equivalent surface is the
        // first-class sessions rail, so carry the intent forward by
        // enabling it.
        "sessions" | "sessions_rail" | "session_history" => {
            s.sessions_rail = true;
        }
        panel @ ("pinned" | "work" | "plan" | "todos" | "tasks" | "activity" | "live"
        | "running" | "agents" | "subagents" | "sub-agents" | "context" | "session"
        // `rail_panel == "tasks"` is the default, so only treat it as unset
        // when the document did not name the key explicitly. Failing the
        // guard falls through to the no-op arm below, which is exactly what
        // the old nested `if` did.
        | "auto")
            if s.rail_panel == "tasks" && !s.rail_panel_explicit =>
        {
            s.rail_panel = match panel {
                // `auto` is the shipped *default* for `sidebar_focus`, so
                // this arm runs for anyone who has a settings.toml at all
                // — even one that only sets `theme`. Auto-collapse meant
                // "show work when there is work", which is exactly the
                // Tasks panel (it auto-fits, and an empty projection
                // reserves no rows). Folding it into the always-on Pinned
                // strip inverted the intent and made a 4-row band the
                // effective default for every upgrading user.
                "tasks" | "activity" | "live" | "running" | "auto" => "tasks",
                "agents" | "subagents" | "sub-agents" => "agents",
                "context" | "session" => "context",
                _ => "pinned",
            }
            .to_string();
        }
        _ => {}
    }
    if s.sidebar_width_percent != 28 {
        let cols = (u32::from(s.sidebar_width_percent) * 120 / 100) as u16;
        s.work_surface_side_width = cols.clamp(26, 80);
    }
}

fn normalize_inline_diffs(value: &str) -> &'static str {
    InlineDiffMode::parse(value).as_setting()
}

/// The `(key, value)` fields a named preset applies, or `None` for an unknown
/// name. Single source of truth shared by [`Settings::apply_preset`] and the
/// `/config preset` command so the bundle is never defined twice.
#[must_use]
pub fn preset_fields(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match name.trim().to_ascii_lowercase().as_str() {
        "calm" => Some(CALM_PRESET_FIELDS),
        _ => None,
    }
}

impl Settings {
    /// Get the canonical settings file path.
    ///
    /// New writes should target `~/.codewhale/settings.toml`. Legacy
    /// DeepSeek-branded paths remain readable as fallbacks during load, but we
    /// no longer surface them as the primary path in `/config`.
    pub fn path() -> Result<PathBuf> {
        let (primary, _legacy_home, legacy_config_dir) = settings_path_candidates();
        primary.or(legacy_config_dir).ok_or_else(|| {
            anyhow::anyhow!("Failed to resolve settings path: no config directory found.")
        })
    }

    /// Load settings from disk, or return defaults if not found
    pub fn load() -> Result<Self> {
        let mut settings = Self::load_persisted()?;
        settings.apply_env_overrides();
        Ok(settings)
    }

    /// Load settings for a diagnostic without migrating a legacy file.
    ///
    /// This preserves the same candidate precedence, parser normalization, and
    /// environment overlays as [`Settings::load`]. Unlike an interactive
    /// startup, diagnostics must not create `~/.codewhale/settings.toml` just
    /// because they inspected a legacy `~/.deepseek/settings.toml` file.
    pub fn load_read_only() -> Result<Self> {
        let mut settings = Self::load_persisted_read_only()?;
        settings.apply_env_overrides();
        Ok(settings)
    }

    /// Read archived route preferences from the user-global settings store.
    ///
    /// Canonical config migration must not inherit a project config's sibling
    /// settings or runtime environment overlays, and must not migrate files.
    pub fn load_legacy_route_preferences_read_only() -> Result<Self> {
        let (primary, legacy_home, legacy_config_dir) = settings_path_candidates_for_scope(false);
        let settings = Self::load_persisted_from_candidates_with_migration(
            primary,
            legacy_home,
            legacy_config_dir,
            false,
        )?;
        // Interactive readers may recover with defaults, but migration must
        // not commit those defaults as if the archived selection were read.
        anyhow::ensure!(settings.load_error.is_none(), "settings.toml: invalid TOML");
        Ok(settings)
    }

    /// Load the normalized values stored on disk without terminal/runtime
    /// overlays. Configuration editors use this path so a value labelled
    /// "saved" never silently reports a tmux, SSH, or accessibility override.
    pub fn load_persisted() -> Result<Self> {
        with_settings_transaction(SettingsTransaction::load)
    }

    /// Load persisted values while the caller already holds the settings
    /// process mutex and adjacent file lock.
    fn load_persisted_locked() -> Result<Self> {
        let (primary, legacy_home, legacy_config_dir) = settings_path_candidates();
        Self::load_persisted_from_candidates(primary, legacy_home, legacy_config_dir)
    }

    /// Load normalized disk values for a diagnostic without creating a
    /// primary settings file from a legacy fallback.
    fn load_persisted_read_only() -> Result<Self> {
        let (primary, legacy_home, legacy_config_dir) = settings_path_candidates();
        Self::load_persisted_from_candidates_with_migration(
            primary,
            legacy_home,
            legacy_config_dir,
            false,
        )
    }

    fn load_persisted_from_candidates(
        primary: Option<PathBuf>,
        legacy_home: Option<PathBuf>,
        legacy_config_dir: Option<PathBuf>,
    ) -> Result<Self> {
        Self::load_persisted_from_candidates_with_migration(
            primary,
            legacy_home,
            legacy_config_dir,
            true,
        )
    }

    fn load_persisted_from_candidates_with_migration(
        primary: Option<PathBuf>,
        legacy_home: Option<PathBuf>,
        legacy_config_dir: Option<PathBuf>,
        migrate_legacy_file: bool,
    ) -> Result<Self> {
        #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
        {
            crate::test_support::with_test_state_io_lock(|| {
                Self::load_persisted_from_candidates_with_migration_unlocked(
                    primary,
                    legacy_home,
                    legacy_config_dir,
                    migrate_legacy_file,
                )
            })
        }
        #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
        Self::load_persisted_from_candidates_with_migration_unlocked(
            primary,
            legacy_home,
            legacy_config_dir,
            migrate_legacy_file,
        )
    }

    fn load_persisted_from_candidates_with_migration_unlocked(
        primary: Option<PathBuf>,
        legacy_home: Option<PathBuf>,
        legacy_config_dir: Option<PathBuf>,
        migrate_legacy_file: bool,
    ) -> Result<Self> {
        let write_path = primary
            .as_ref()
            .cloned()
            .or_else(|| legacy_config_dir.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("Failed to resolve settings path: no config directory found.")
            })?;
        let tui_prefs_path =
            tui_prefs_path_from_settings_candidates(primary.as_deref(), legacy_home.as_deref());
        let read_path =
            resolve_settings_path_from_candidates(primary, legacy_home, legacy_config_dir)
                .unwrap_or_else(|_| write_path.clone());

        let mut explicit_keys = std::collections::BTreeSet::new();
        let mut settings = if !read_path.exists() {
            Self::default()
        } else {
            let content = std::fs::read_to_string(&read_path)
                .with_context(|| format!("Failed to read settings from {}", read_path.display()))?;
            let parsed_document = toml::from_str::<toml::Value>(&content).ok();
            let mut s: Settings = match toml::from_str(&content) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse {} (using defaults): {e:#}",
                        read_path.display()
                    );
                    // Keep the app running on defaults, but carry the failure
                    // so a settings surface never labels them as saved.
                    Self {
                        load_error: Some(format!("{}: {e}", read_path.display())),
                        ..Self::default()
                    }
                }
            };
            // Which keys the document named itself. An explicit value is user
            // intent and always wins over a default or a migrated one.
            explicit_keys.extend(
                parsed_document
                    .as_ref()
                    .and_then(toml::Value::as_table)
                    .into_iter()
                    .flat_map(|table| table.keys().cloned()),
            );
            // A persisted threshold is itself an explicit request for
            // auto-compaction. Older versions accepted this setting while
            // leaving the default `auto_compact = false`, silently turning the
            // requested trigger into a no-op. Preserve an explicit boolean
            // opt-out, but make threshold-only files effective on load.
            s.auto_compact_explicit = parsed_document
                .as_ref()
                .is_some_and(auto_compact_explicitly_configured_in_document);
            s.rail_panel_explicit = explicit_keys.contains("rail_panel");
            s.work_surface_placement_explicit = explicit_keys.contains("work_surface_placement");
            if parsed_document.as_ref().is_some_and(|document| {
                document.as_table().is_some_and(|table| {
                    !table.contains_key("auto_compact")
                        && (table.contains_key("auto_compact_threshold")
                            || table.contains_key("auto_compact_threshold_percent"))
                })
            }) {
                s.auto_compact = true;
            }

            // Compat boundary (2026-09-02): `ocean_treatment` was a modifier
            // on `theme`; the painted field is now the `underwater` theme
            // itself. Fold any persisted deepsea treatment into
            // `theme = "underwater"`, then drop the retired key on the next
            // ordinary save (the struct simply has no such field).
            if let Some(_treatment) = parsed_document
                .as_ref()
                .and_then(toml::Value::as_table)
                .and_then(|table| table.get("ocean_treatment"))
                .and_then(toml::Value::as_str)
                .filter(|treatment| {
                    matches!(
                        treatment.trim().to_ascii_lowercase().as_str(),
                        "deepsea" | "underwater" | "ombre" | "gradient" | "classic"
                    )
                })
            {
                s.theme = "underwater".to_string();
            }

            // "yolo" used to bundle two independent choices: Agent mode and
            // unrestricted approvals.  Keep that behavior on upgrade, but
            // store/show the two choices explicitly so Settings does not claim
            // the app starts in a fictional mode.
            let legacy_yolo_default = s.default_mode.trim().eq_ignore_ascii_case("yolo");
            s.legacy_yolo_default = legacy_yolo_default;
            s.default_mode = if legacy_yolo_default {
                "agent".to_string()
            } else {
                normalize_mode(&s.default_mode).to_string()
            };
            s.composer_density = normalize_composer_density(&s.composer_density).to_string();
            s.transcript_spacing = normalize_transcript_spacing(&s.transcript_spacing).to_string();
            s.tool_collapse_mode = normalize_tool_collapse_mode(&s.tool_collapse_mode).to_string();
            s.sidebar_focus = normalize_sidebar_focus(&s.sidebar_focus).to_string();
            // Rail unification (0.9.4) migration: the classic sidebar is
            // gone, so its settings carry forward instead of stranding.
            migrate_sidebar_settings_to_rail(&mut s);
            s.status_indicator = normalize_status_indicator(&s.status_indicator).to_string();
            s.work_surface_placement =
                normalize_work_surface_placement(&s.work_surface_placement).to_string();
            // Round 3 placement migration: a persisted `top` from before the
            // default moved is the old default, not a preference. Move it
            // once and remember; the next ordinary save persists both.
            if !s.work_surface_bottom_migrated {
                if s.work_surface_placement == "top" {
                    s.work_surface_placement = "bottom".to_string();
                }
                s.work_surface_bottom_migrated = true;
            }
            s.rail_panel = normalize_rail_panel(&s.rail_panel).to_string();
            // Migrate the unreadable 2..=4 legacy range in memory. The next
            // ordinary settings transaction persists the normalized value;
            // loading settings remains a read-only operation.
            s.work_surface_top_height = s
                .work_surface_top_height
                .clamp(WORK_SURFACE_TOP_HEIGHT_MIN, WORK_SURFACE_TOP_HEIGHT_MAX);
            s.work_surface_side_width = s.work_surface_side_width.clamp(26, 80);
            s.inline_diffs = normalize_inline_diffs(&s.inline_diffs).to_string();
            s.synchronized_output =
                normalize_synchronized_output(&s.synchronized_output).to_string();
            s.locale = normalize_configured_locale(&s.locale)
                .unwrap_or("en")
                .to_string();
            s.background_color = normalize_optional_background_color(s.background_color.as_deref());
            s.theme = normalize_settings_theme(&s.theme);
            s.default_model = s.default_model.as_deref().and_then(normalize_default_model);
            s.reasoning_effort = s
                .reasoning_effort
                .as_deref()
                .and_then(|value| normalize_reasoning_effort_setting(value).ok().flatten());
            s.permission_posture = s
                .permission_posture
                .as_deref()
                .and_then(normalize_permission_posture);
            if legacy_yolo_default && s.permission_posture.is_none() {
                s.permission_posture = Some("full-access".to_string());
            }
            s.sandbox_mode = s.sandbox_mode.as_deref().and_then(normalize_sandbox_mode);
            s
        };
        if migrate_legacy_file {
            migrate_settings_file_to_primary_if_needed(&write_path, &read_path);
        }
        // One store: fold the dead `tui.toml` in and say what happened.
        if let Some(prefs_path) = tui_prefs_path.filter(|path| path.exists())
            && let Some(receipt) = fold_tui_prefs(
                &mut settings,
                &explicit_keys,
                &prefs_path,
                migrate_legacy_file,
            )
        {
            if migrate_legacy_file && !receipt.folded.is_empty() {
                // The fold is only real once settings.toml owns the value;
                // otherwise the next launch would read the moved-aside file's
                // theme back out of nothing and quietly lose it.
                if let Err(error) = settings.save_to_path(&write_path) {
                    tracing::warn!(
                        "failed to persist folded tui.toml values to {}: {error:#}",
                        write_path.display()
                    );
                }
            }
            if !receipt.is_empty() {
                settings.tui_prefs_migration = Some(receipt);
            }
        }
        // The provenance ledger: keys the document named itself came from
        // user config; everything else is the schema default until `set()`,
        // a CLI flag, or a higher layer says otherwise.
        for key in &explicit_keys {
            settings.provenance.insert(key.clone(), Layer::UserConfig);
        }
        Ok(settings)
    }

    /// Whether this load normalized a legacy `default_mode = "yolo"` value.
    ///
    /// This is migration provenance, not a user-facing mode. New writes accept
    /// only Agent or Plan and serialize the independent permission posture.
    pub fn legacy_yolo_default_detected(&self) -> bool {
        self.legacy_yolo_default
    }

    /// Receipt for the one-time `tui.toml` fold, when this load performed one.
    pub fn tui_prefs_migration(&self) -> Option<&TuiPrefsMigration> {
        self.tui_prefs_migration.as_ref()
    }

    /// Which layer supplied the in-force value of `key`: the load ledger,
    /// defaulting to [`Layer::Default`] for keys the document never named.
    /// Aliases resolve to their canonical key first, so `/set collapse`
    /// reports the same layer as the `tool_collapse` row.
    pub fn provenance(&self, key: &str) -> Layer {
        let canonical = Self::canonical_key(key).unwrap_or(key);
        self.provenance
            .get(canonical)
            .copied()
            .unwrap_or(Layer::Default)
    }

    /// The persisted field name behind a canonical schema key. Two schema
    /// keys predate their persisted names and cannot be renamed without a
    /// settings.toml migration.
    fn persisted_field_name(canonical: &str) -> &str {
        match canonical {
            "tool_collapse" => "tool_collapse_mode",
            "max_history" => "max_input_history",
            other => other,
        }
    }

    /// The value `key` currently holds in this store, in its written-to-disk
    /// string form — `None` when the key is not a field of this store.
    ///
    /// `Settings` serializes field-for-field to settings.toml, so a document
    /// lookup on the serialized form shares `set`'s key vocabulary instead
    /// of growing a second hand-keyed reader beside it.
    pub fn value(&self, key: &str) -> Option<String> {
        let canonical = Self::canonical_key(key).unwrap_or(key);
        let field = Self::persisted_field_name(canonical);
        let document = toml::Value::try_from(self).ok()?;
        let value = document.as_table()?.get(field)?;
        Some(match value {
            toml::Value::String(text) => text.clone(),
            other => other.to_string(),
        })
    }

    /// Whether the loaded settings document explicitly named `key` — a
    /// persisted user choice rather than an inherited default.
    pub fn is_set(&self, key: &str) -> bool {
        let canonical = Self::canonical_key(key).unwrap_or(key);
        self.provenance
            .get(Self::persisted_field_name(canonical))
            .is_some_and(|layer| *layer == Layer::UserConfig)
    }

    /// Whether the user explicitly persisted an auto-compaction preference.
    /// A threshold is intent to enable compaction unless an explicit boolean
    /// says otherwise. When all three keys are absent, callers may choose a
    /// model-aware default.
    pub fn auto_compact_explicitly_configured() -> bool {
        let candidates = settings_path_candidates();
        #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
        {
            crate::test_support::with_test_state_io_lock(|| {
                auto_compact_explicitly_configured_from_candidates(candidates)
            })
        }
        #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
        auto_compact_explicitly_configured_from_candidates(candidates)
    }
}

fn auto_compact_explicitly_configured_from_candidates(
    (primary, legacy_home, legacy_config_dir): (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>),
) -> bool {
    let Ok(path) = resolve_settings_path_from_candidates(primary, legacy_home, legacy_config_dir)
    else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&content) else {
        return false;
    };
    auto_compact_explicitly_configured_in_document(&value)
}

fn auto_compact_explicitly_configured_in_document(value: &toml::Value) -> bool {
    value.as_table().is_some_and(|table| {
        table.contains_key("auto_compact")
            || table.contains_key("auto_compact_threshold")
            || table.contains_key("auto_compact_threshold_percent")
    })
}

/// The runtime overlay that forces `low_motion` on, when one wins over the
/// persisted value. Mirrors the precedence of
/// [`Settings::apply_env_overrides`] so a settings surface can name the real
/// owner instead of calling a forced value "saved".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionOverride {
    NoAnimationsEnv,
    VsCodeTerminal,
    TermiusTerminal,
    SshSession,
    TabbyTerminal,
    LegacyWindowsConsole,
}

impl MotionOverride {
    /// The literal token a person can look for in their environment.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NoAnimationsEnv => "NO_ANIMATIONS",
            Self::VsCodeTerminal => "TERM_PROGRAM=vscode",
            Self::TermiusTerminal => "TERM_PROGRAM=Termius",
            Self::SshSession => "SSH_CLIENT/SSH_TTY",
            Self::TabbyTerminal => "TERM_PROGRAM=tabby",
            Self::LegacyWindowsConsole => "legacy Windows console",
        }
    }

    /// Whether the override comes from the environment (a variable or an
    /// SSH session) rather than from the terminal program itself.
    #[must_use]
    pub fn is_environment(self) -> bool {
        matches!(self, Self::NoAnimationsEnv | Self::SshSession)
    }
}

/// Detect which runtime overlay forces `low_motion`, in the order
/// [`Settings::apply_env_overrides`] applies them.
#[must_use]
pub fn detect_low_motion_override() -> Option<MotionOverride> {
    let env_nonempty = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if env_truthy("NO_ANIMATIONS") {
        return Some(MotionOverride::NoAnimationsEnv);
    }
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if term_program.eq_ignore_ascii_case("vscode") {
        return Some(MotionOverride::VsCodeTerminal);
    }
    if term_program == "Termius" {
        return Some(MotionOverride::TermiusTerminal);
    }
    if env_nonempty("SSH_CLIENT") || env_nonempty("SSH_TTY") {
        return Some(MotionOverride::SshSession);
    }
    if term_program.to_ascii_lowercase().contains("tabby") {
        return Some(MotionOverride::TabbyTerminal);
    }
    if detected_legacy_windows_console_host() {
        return Some(MotionOverride::LegacyWindowsConsole);
    }
    None
}

impl Settings {
    /// Apply environment-driven overlays after disk load. Used for
    /// platform a11y signals that should ignore the user's saved
    /// preference (#450). The env values are consulted at startup;
    /// changing them mid-session has no effect because settings are
    /// only re-read on `Settings::load()`.
    pub fn apply_env_overrides(&mut self) {
        if env_truthy("NO_ANIMATIONS") {
            self.low_motion = true;
            self.fancy_animations = false;
        }
        // VS Code (TERM_PROGRAM=vscode, #1356) and a few VTE terminals
        // (#1470) produce visible flicker at 120 FPS. Cap their redraw rate.
        // VS Code's xterm.js renderer also needs decorative
        // motion disabled: the underwater chrome added substantially more
        // independently moving cells than the original #1356 fix covered.
        // Ghostty is deliberately absent from this 30 FPS compatibility lane.
        // Its synchronized GPU renderer gets a dedicated 60 FPS atmosphere
        // cap in display_refresh; putting it here made the restored truecolor
        // ocean visibly step even though the terminal could keep up.
        // Like NO_ANIMATIONS above, this unconditionally overrides any
        // disk-loaded value — consistent precedence: env signals always win.
        let term_program = std::env::var("TERM_PROGRAM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Tabby renders through Electron/xterm.js. Its Windows IME bridge
        // can observe cursor-positioning sequences while a frame is still
        // being applied, so use the calmer rendering path there.
        let term_is_tabby = term_program.contains("tabby");
        let term_constrains_frame_rate = term_program == "vscode";
        let vte_env_constrains_frame_rate = std::env::var_os("TILIX_ID")
            .is_some_and(|v| !v.is_empty())
            || std::env::var_os("TERMINATOR_UUID").is_some_and(|v| !v.is_empty());
        if term_constrains_frame_rate || vte_env_constrains_frame_rate {
            self.constrained_frame_rate = true;
        }
        if term_program == "vscode" {
            self.low_motion = true;
            self.fancy_animations = false;
        }

        // Termius (TERM_PROGRAM=Termius) and SSH sessions exhibit the
        // same 120-FPS flicker class as VS Code — the SSH round-trip
        // races ahead of what the remote renderer can flush, so rapid
        // cursor-positioning sequences cycle through input boxes.
        // Drop both to the 30 FPS low-motion cap. Harvested from
        // PR #1479 by @CrepuscularIRIS / autoghclaw (closes #1433).
        //
        // SSH_CLIENT is exported by sshd for every TCP SSH session;
        // SSH_TTY is exported only for interactive PTY logins, so we
        // check both so non-PTY-allocating tools (rsync wrappers, etc.)
        // still pick this up if they end up running the TUI.
        let term_is_termius = std::env::var("TERM_PROGRAM").as_deref() == Ok("Termius");
        let in_ssh_session = std::env::var_os("SSH_CLIENT").is_some_and(|v| !v.is_empty())
            || std::env::var_os("SSH_TTY").is_some_and(|v| !v.is_empty());
        if term_is_termius || in_ssh_session {
            self.low_motion = true;
            self.fancy_animations = false;
        }
        if term_is_tabby {
            self.low_motion = true;
            self.fancy_animations = false;
            self.constrained_frame_rate = true;
            if self.synchronized_output.eq_ignore_ascii_case("auto") {
                self.synchronized_output = "off".to_string();
            }
        }

        // Multiplexers need a bounded redraw rate, not a different product.
        // Preserve authored motion and let the frame limiter protect tmux /
        // screen; NO_ANIMATIONS remains the explicit hard-off contract.
        let in_terminal_multiplexer = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
            || std::env::var_os("STY").is_some_and(|v| !v.is_empty());
        if in_terminal_multiplexer {
            self.constrained_frame_rate = true;
        }

        // Plain Windows PowerShell / cmd.exe under legacy ConHost exposes none
        // of the modern terminal markers below. Keep rendering calmer there:
        // lower the motion rate, disable animated chrome, and avoid DEC 2026
        // synchronized-output wrapping unless the user explicitly forced it on.
        if detected_legacy_windows_console_host() {
            self.low_motion = true;
            self.fancy_animations = false;
            if self.synchronized_output.eq_ignore_ascii_case("auto") {
                self.synchronized_output = "off".to_string();
            }
        }

        // Ptyxis 50.x (the new default terminal on Ubuntu 26.04) ships with
        // VTE 0.84.x which mishandles DEC mode 2026 synchronized output: the
        // begin/end pair is parsed but each wrapped frame still triggers a
        // full-viewport flash on the GPU compositor side, so any TUI that
        // uses DEC 2026 to avoid tearing instead gets visible flicker on
        // every redraw. gnome-terminal 3.58 on the same VTE renders cleanly,
        // so we can't broaden the opt-out to all VTE-based terminals —
        // only the Ptyxis-specific signals trigger it. Confirmed
        // user-visible regression starting with Ubuntu 26.04's default
        // terminal swap; cargo-installed binaries are not exempt because
        // the bug is in the terminal, not the binary.
        //
        // Only flip `auto` to `off`; respect an explicit `"on"` so users
        // who upgrade Ptyxis or want to confirm the fix landed upstream
        // can override the heuristic from the persisted settings.toml or
        // `/set synchronized_output on`.
        if self.synchronized_output.eq_ignore_ascii_case("auto") && detected_ptyxis_terminal() {
            self.synchronized_output = "off".to_string();
        }
    }

    /// Run one atomic load → mutate → save cycle against `settings.toml`.
    ///
    /// **Every writer that reads the whole file, changes some fields, and writes
    /// the whole file back must go through here** (or through
    /// [`SettingsTransaction`] for the multi-step shape). `save` serializes the
    /// complete struct, so two unsynchronized writers that each did their own
    /// `load_persisted` will each write back the *other's* pre-image: whichever
    /// saves last silently reverts the other's field. Locking `save` alone does
    /// not help, because the stale read already happened before the lock.
    ///
    /// Two locks are taken (see [`with_settings_transaction`]): a process-wide
    /// mutex keyed by the resolved settings path, which covers writers that
    /// never share an object — a background startup-default drain and a
    /// synchronous Shift+Tab permission write, the concrete pair that lost
    /// `default_mode` / `permission_posture` against each other — and a
    /// cross-process file lock, which covers a second Codewhale process on the
    /// same home directory.
    ///
    /// The closure must not call `transact`, [`with_settings_transaction`],
    /// `save`, or `load_persisted` itself — the lock is not re-entrant. Use
    /// [`with_settings_transaction`] when you need more than one save in one
    /// critical section.
    pub fn transact<T>(mutate: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        with_settings_transaction(|transaction| {
            let mut settings = transaction.load()?;
            let value = mutate(&mut settings)?;
            transaction.save(&settings)?;
            Ok(value)
        })
    }

    /// [`Self::transact`] for a mutation that may decide there is nothing to
    /// write. Returning `None` abandons the transaction without touching disk,
    /// so a "flag already set" early return does not rewrite the file.
    pub fn transact_opt<T>(
        mutate: impl FnOnce(&mut Self) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        with_settings_transaction(|transaction| {
            let mut settings = transaction.load()?;
            let Some(value) = mutate(&mut settings)? else {
                return Ok(None);
            };
            transaction.save(&settings)?;
            Ok(Some(value))
        })
    }

    /// Save settings to disk as a standalone, fully locked write.
    ///
    /// Prefer [`Self::transact`]: calling this on a `Settings` that was loaded
    /// outside a transaction writes back a snapshot that may already be stale
    /// for every field the caller did *not* mean to change. This entry point
    /// still takes both locks, so the bytes it writes are never interleaved with
    /// another writer's — it just cannot fix a stale read that already happened.
    ///
    /// Not callable from inside a transaction: the cross-process lock is not
    /// re-entrant, so a nested acquisition would deadlock against itself. Inside
    /// a critical section use [`SettingsTransaction::save`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn save(&self) -> Result<()> {
        with_settings_transaction(|transaction| transaction.save(self))
    }

    /// The write half of a settings transaction: serialize, merge comments, and
    /// replace the file atomically. The caller already holds both the
    /// process-wide mutex and the cross-process file lock.
    fn save_locked(&self, path: &Path) -> Result<()> {
        #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
        {
            crate::test_support::with_test_state_io_lock(|| self.save_to_path(path))
        }
        #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
        self.save_to_path(path)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        // Parse-error fallback values keep the UI usable, but cannot replace
        // the unreadable document. Do not echo its potentially private text.
        anyhow::ensure!(self.load_error.is_none(), "settings.toml: invalid TOML");
        // Create config directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }

        let mut serialized =
            toml::to_string_pretty(self).context("Failed to serialize settings")?;
        if !self.auto_compact_explicit {
            let mut document = serialized
                .parse::<toml_edit::DocumentMut>()
                .context("Failed to prepare settings for persistence")?;
            document.remove("auto_compact");
            document.remove("auto_compact_threshold_percent");
            serialized = document.to_string();
        }
        let body = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read settings at {}", path.display()))?;
            codewhale_config::merge_and_preserve_comments(&serialized, &raw).unwrap_or_else(|e| {
                tracing::warn!("failed to merge settings comments, saving without them: {e:#}");
                serialized
            })
        } else {
            serialized
        };
        atomically_replace_settings_file(path, body.as_bytes())
    }

    /// Set a single setting by key
    /// Canonical schema key for a `set()` spelling: the first pattern of each
    /// match arm below. `None` means `set()` rejects the spelling, so the
    /// ledger never learns it. Keep in sync with the arms — the
    /// `set_marks_session_provenance` test enforces it per spelling.
    pub fn canonical_key(key: &str) -> Option<&'static str> {
        Some(match key {
            "auto_compact" | "compact" => "auto_compact",
            "auto_compact_threshold" | "auto_compact_threshold_percent" => {
                "auto_compact_threshold_percent"
            }
            "calm_mode" | "calm" => "calm_mode",
            "pet_mode" => "pet_mode",
            "tool_collapse" | "tool_collapse_mode" | "collapse" => "tool_collapse",
            "low_motion" | "motion" => "low_motion",
            "fancy_animations" | "fancy" | "animations" => "fancy_animations",
            "focus_texture" | "texture" => "focus_texture",
            "work_surface_placement" | "work_surface" | "work_rail" => "work_surface_placement",
            "rail_panel" | "rail" => "rail_panel",
            "work_surface_top_height" | "work_top_height" => "work_surface_top_height",
            "work_surface_side_width" | "work_side_width" => "work_surface_side_width",
            "bracketed_paste" | "paste" => "bracketed_paste",
            "paste_burst_detection" | "paste_burst" => "paste_burst_detection",
            "mention_menu_limit" | "mention_limit" => "mention_menu_limit",
            "mention_walk_depth" | "mention_depth" | "completions_walk_depth" => {
                "mention_walk_depth"
            }
            "mention_menu_behavior" | "mention_behavior" | "mention_menu" => {
                "mention_menu_behavior"
            }
            "show_thinking" | "thinking" => "show_thinking",
            "thinking_default_expanded" | "thinking_expanded" => "thinking_default_expanded",
            "thinking_preview_lines" | "thinking_preview" => "thinking_preview_lines",
            "thinking_highlight" | "reasoning_highlight" => "thinking_highlight",
            "help_expand_groups" | "help_expanded" => "help_expand_groups",
            "contextual_tips" => "contextual_tips",
            "pin_last_prompt" | "pin_prompt" => "pin_last_prompt",
            "show_tool_details" | "tool_details" => "show_tool_details",
            "inline_diffs" | "inline_diff" | "diffs" => "inline_diffs",
            "locale" | "language" => "locale",
            "theme" | "ui_theme" => "theme",
            "background_color" | "background" | "bg" => "background_color",
            "composer_density" | "composer" => "composer_density",
            "composer_border" | "border" => "composer_border",
            "composer_multiline_mode" | "multiline_mode" | "multiline" => "composer_multiline_mode",
            "composer_vim_mode" | "vim_mode" | "vim" => "composer_vim_mode",
            "transcript_spacing" | "spacing" => "transcript_spacing",
            "status_indicator" | "indicator" => "status_indicator",
            "synchronized_output" | "sync_output" | "sync" => "synchronized_output",
            "workspace_follow_symlinks" | "follow_symlinks" => "workspace_follow_symlinks",
            "default_mode" | "mode" => "default_mode",
            "context_panel" | "context" | "session_panel" => "context_panel",
            "sessions_rail" | "sessions_panel" | "session_rail" => "sessions_rail",
            "session_auto_resume" | "auto_resume" => "session_auto_resume",
            "cost_currency" | "currency" => "cost_currency",
            "max_history" | "history" => "max_history",
            "default_model" | "model" => "default_model",
            "reasoning_effort" | "effort" => "reasoning_effort",
            "permission_posture" | "permissions" => "permission_posture",
            "sandbox_mode" | "sandbox" | "filesystem_sandbox" => "sandbox_mode",
            _ => return None,
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        // The ledger learns the canonical key only when the write below
        // succeeds: a rejected value leaves the previous layer in force.
        let canonical = Self::canonical_key(key);
        match key {
            "auto_compact" | "compact" => {
                self.auto_compact = parse_bool(value)?;
                self.auto_compact_explicit = true;
            }
            "auto_compact_threshold" | "auto_compact_threshold_percent" => {
                self.auto_compact_threshold_percent =
                    parse_percent_setting("auto_compact_threshold_percent", value)?;
                self.auto_compact = true;
                self.auto_compact_explicit = true;
            }
            "calm_mode" | "calm" => {
                self.calm_mode = parse_bool(value)?;
            }
            "pet_mode" => {
                self.pet_mode = parse_bool(value)?;
            }
            "tool_collapse" | "tool_collapse_mode" | "collapse" => {
                let normalized = normalize_tool_collapse_mode(value);
                if !matches!(normalized, "compact" | "expanded" | "calm") {
                    return Err(anyhow::anyhow!(
                        "Failed to update setting: invalid tool collapse mode '{value}'. Expected: compact, expanded, or calm."
                    ));
                }
                self.tool_collapse_mode = normalized.to_string();
            }
            "low_motion" | "motion" => {
                self.low_motion = parse_bool(value)?;
            }
            "fancy_animations" | "fancy" | "animations" => {
                self.fancy_animations = parse_bool(value)?;
            }
            "focus_texture" | "texture" => {
                let normalized = value.trim().to_ascii_lowercase();
                if !matches!(normalized.as_str(), "off" | "scrim" | "grain") {
                    anyhow::bail!(
                        "Failed to update setting: invalid focus texture '{value}'. Expected: off, scrim, or grain."
                    );
                }
                self.focus_texture = normalized;
            }
            "work_surface_placement" | "work_surface" | "work_rail" => {
                let normalized = value.trim().to_ascii_lowercase();
                if !matches!(
                    normalized.as_str(),
                    "top" | "bottom" | "left" | "right" | "off"
                ) {
                    anyhow::bail!(
                        "Failed to update setting: invalid work surface placement '{value}'. Expected: top, bottom, left, right, or off."
                    );
                }
                self.work_surface_placement = normalized;
            }
            "rail_panel" | "rail" => {
                let normalized = value.trim().to_ascii_lowercase();
                // `pinned` stays accepted as a setting word; it folds into
                // the tasks view exactly like the load-time migration.
                if !matches!(
                    normalized.as_str(),
                    "tasks"
                        | "agents"
                        | "background"
                        | "terminal"
                        | "files"
                        | "notepad"
                        | "context"
                        | "git"
                        | "price"
                        | "watch"
                        | "pinned"
                ) {
                    anyhow::bail!(
                        "Failed to update setting: invalid workbar panel '{value}'. Expected: tasks, agents, background, terminal, files, notepad, context, git, or price."
                    );
                }
                self.rail_panel = normalize_rail_panel(&normalized).to_string();
                self.rail_panel_explicit = true;
            }
            "work_surface_top_height" | "work_top_height" => {
                self.work_surface_top_height = parse_u16_range(
                    "work_surface_top_height",
                    value,
                    WORK_SURFACE_TOP_HEIGHT_MIN,
                    WORK_SURFACE_TOP_HEIGHT_MAX,
                )?;
            }
            "work_surface_side_width" | "work_side_width" => {
                self.work_surface_side_width =
                    parse_u16_range("work_surface_side_width", value, 26, 80)?;
            }
            "bracketed_paste" | "paste" => {
                self.bracketed_paste = parse_bool(value)?;
            }
            "paste_burst_detection" | "paste_burst" => {
                self.paste_burst_detection = parse_bool(value)?;
            }
            "mention_menu_limit" | "mention_limit" => {
                self.mention_menu_limit = parse_usize_setting("mention_menu_limit", value)?;
            }
            "mention_walk_depth" | "mention_depth" | "completions_walk_depth" => {
                self.mention_walk_depth = parse_usize_setting("mention_walk_depth", value)?;
            }
            "mention_menu_behavior" | "mention_behavior" | "mention_menu" => {
                self.mention_menu_behavior = normalize_mention_menu_behavior(value)?;
            }
            "show_thinking" | "thinking" => {
                self.show_thinking = parse_bool(value)?;
            }
            "thinking_default_expanded" | "thinking_expanded" => {
                self.thinking_default_expanded = parse_bool(value)?;
            }
            "thinking_preview_lines" | "thinking_preview" => {
                self.thinking_preview_lines =
                    parse_usize_setting("thinking_preview_lines", value)?.min(40);
            }
            "thinking_highlight" | "reasoning_highlight" => {
                self.thinking_highlight = parse_bool(value)?;
            }
            "help_expand_groups" | "help_expanded" => {
                self.help_expand_groups = parse_bool(value)?;
            }
            "contextual_tips" => {
                self.contextual_tips = parse_bool(value)?;
            }
            "pin_last_prompt" | "pin_prompt" => {
                self.pin_last_prompt = parse_bool(value)?;
            }
            "show_tool_details" | "tool_details" => {
                self.show_tool_details = parse_bool(value)?;
            }
            "inline_diffs" | "inline_diff" | "diffs" => {
                let normalized = value.trim().to_ascii_lowercase();
                if !matches!(normalized.as_str(), "full" | "summary" | "off") {
                    anyhow::bail!(
                        "Failed to update setting: invalid inline diff mode '{value}'. Expected: full, summary, or off."
                    );
                }
                self.inline_diffs = normalized;
            }
            "locale" | "language" => {
                let Some(locale) = normalize_configured_locale(value) else {
                    anyhow::bail!(
                        "Failed to update setting: invalid locale '{value}'. Expected: {}.",
                        codewhale_localization::configured_locale_values(", ")
                    );
                };
                self.locale = locale.to_string();
            }
            "theme" | "ui_theme" => {
                self.theme = normalize_theme_setting(value).map_err(anyhow::Error::msg)?;
            }
            "background_color" | "background" | "bg" => {
                self.background_color = normalize_background_color_setting(value)?;
            }
            "composer_density" | "composer" => {
                let normalized = normalize_composer_density(value);
                if !["compact", "comfortable", "spacious"].contains(&normalized) {
                    anyhow::bail!(
                        "Failed to update setting: invalid composer density '{value}'. Expected: compact, comfortable, spacious."
                    );
                }
                self.composer_density = normalized.to_string();
            }
            "composer_border" | "border" => {
                self.composer_border = parse_bool(value)?;
            }
            "composer_multiline_mode" | "multiline_mode" | "multiline" => {
                self.composer_multiline_mode = parse_bool(value)?;
            }
            "composer_vim_mode" | "vim_mode" | "vim" => {
                let normalized = value.trim().to_ascii_lowercase();
                if !["vim", "normal"].contains(&normalized.as_str()) {
                    anyhow::bail!(
                        "Failed to update setting: invalid composer vim mode '{value}'. Expected: normal, vim."
                    );
                }
                self.composer_vim_mode = normalized;
            }
            "transcript_spacing" | "spacing" => {
                let normalized = normalize_transcript_spacing(value);
                if !["compact", "comfortable", "spacious"].contains(&normalized) {
                    anyhow::bail!(
                        "Failed to update setting: invalid transcript spacing '{value}'. Expected: compact, comfortable, spacious."
                    );
                }
                self.transcript_spacing = normalized.to_string();
            }
            "status_indicator" | "indicator" => {
                let normalized = normalize_status_indicator(value);
                if !["cw", "whale", "dots", "off"].contains(&normalized) {
                    anyhow::bail!(
                        "Failed to update setting: invalid status indicator '{value}'. Expected: cw, whale, dots, off."
                    );
                }
                self.status_indicator = normalized.to_string();
            }
            "synchronized_output" | "sync_output" | "sync" => {
                let normalized = normalize_synchronized_output(value);
                if !["auto", "on", "off"].contains(&normalized) {
                    anyhow::bail!(
                        "Failed to update setting: invalid synchronized_output '{value}'. Expected: auto, on, off."
                    );
                }
                self.synchronized_output = normalized.to_string();
            }
            "workspace_follow_symlinks" | "follow_symlinks" => {
                self.workspace_follow_symlinks = parse_bool(value)?;
            }
            "default_mode" | "mode" => {
                // Work (wire: agent), Plan, and Operate are valid startup modes.
                // yolo remains a permission-migration alias, not a mode write.
                self.default_mode = match value.trim().to_ascii_lowercase().as_str() {
                    "agent" | "normal" | "act" | "work" | "edit" => "agent".to_string(),
                    "plan" => "plan".to_string(),
                    "operate" | "operation" | "ops" => "operate".to_string(),
                    _ => anyhow::bail!(
                        "Failed to update setting: invalid mode '{value}'. Expected: work (agent), plan, or operate."
                    ),
                };
            }
            "context_panel" | "context" | "session_panel" => {
                self.context_panel = parse_bool(value)?;
            }
            "sessions_rail" | "sessions_panel" | "session_rail" => {
                self.sessions_rail = parse_bool(value)?;
            }
            "session_auto_resume" | "auto_resume" => {
                self.session_auto_resume = parse_bool(value)?;
            }
            "cost_currency" | "currency" => {
                let Some(currency) = codewhale_config::pricing::CostCurrency::from_setting(value)
                else {
                    anyhow::bail!(
                        "Failed to update setting: invalid cost currency '{value}'. Expected: usd, cny, rmb, yuan."
                    );
                };
                self.cost_currency = match currency {
                    codewhale_config::pricing::CostCurrency::Usd => "usd",
                    codewhale_config::pricing::CostCurrency::Cny => "cny",
                }
                .to_string();
            }
            "max_history" | "history" => {
                let max: usize = value.parse().map_err(|_| {
                    anyhow::anyhow!(
                        "Failed to update setting: invalid max history '{value}'. Expected a positive number."
                    )
                })?;
                self.max_input_history = max;
            }
            "default_model" | "model" => {
                anyhow::bail!(
                    "Model defaults belong to config.toml. Use /model and choose Remember as my default, or /config model <id> --save."
                );
            }
            "reasoning_effort" | "effort" => {
                self.reasoning_effort = normalize_reasoning_effort_setting(value)?;
            }
            "permission_posture" | "permissions" => {
                self.permission_posture = normalize_permission_posture(value);
                if self.permission_posture.is_none() {
                    anyhow::bail!(
                        "Failed to update setting: invalid permission posture '{value}'. Expected: ask, auto-review, or full-access."
                    );
                }
            }
            "sandbox_mode" | "sandbox" | "filesystem_sandbox" => {
                self.sandbox_mode = normalize_sandbox_mode(value);
                if self.sandbox_mode.is_none() {
                    anyhow::bail!(
                        "Failed to update setting: invalid sandbox_mode '{value}'. Expected: read-only, workspace-write, danger-full-access, or external-sandbox."
                    );
                }
            }
            _ => {
                anyhow::bail!("Failed to update setting: unknown setting '{key}'.");
            }
        }
        if let Some(canonical) = canonical {
            self.provenance
                .insert(canonical.to_string(), Layer::SessionOverride);
        }
        Ok(())
    }

    /// Apply a named settings preset (#3478).
    ///
    /// Presets are the first bundled-settings mechanism: a single name applies a
    /// coherent group of presentation knobs. `calm` is the "beautiful/calm
    /// transcript" preset — it quiets motion and verbose tool output while
    /// **keeping evidence reachable**: thinking stays visible and tool runs stay
    /// expandable (only their inline detail is collapsed), so maintainer/release
    /// work is never blind to failures. Presentation only — no model, provider,
    /// routing, or safety setting is touched. Reuses [`Settings::set`] so each
    /// field goes through the same validation as a single-key set.
    ///
    /// Returns the keys changed, or an error for an unknown preset.
    pub fn apply_preset(&mut self, name: &str) -> Result<Vec<&'static str>> {
        let Some(bundle) = preset_fields(name) else {
            anyhow::bail!("Unknown preset '{}'. Available presets: calm", name.trim());
        };
        let mut changed = Vec::with_capacity(bundle.len());
        for (key, value) in bundle {
            self.set(key, value)?;
            changed.push(*key);
        }
        Ok(changed)
    }

    /// Get all settings as a displayable string
    /// Test-only whole-report renderer; the command renders from [`Self::command_report`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn display(&self, locale: codewhale_localization::Locale) -> String {
        use codewhale_localization::{MessageId, tr};
        let report = self.command_report();
        let mut lines = Vec::new();
        lines.push(tr(locale, MessageId::SettingsTitle).to_string());
        lines.push("─────────────────────────────".to_string());
        lines.extend(report.rows.iter().map(|row| {
            format!(
                "  {:<width$}{}",
                row.label,
                row.value,
                width = row.label_width
            )
        }));
        lines.push(String::new());
        lines.push(format!(
            "{} {}",
            tr(locale, MessageId::SettingsConfigFile),
            report.config_file
        ));
        if !report.from_settings.is_empty() || !report.session_overrides.is_empty() {
            lines.push(String::new());
            if !report.from_settings.is_empty() {
                lines.push(format!(
                    "  from settings.toml: {}",
                    report.from_settings.join(", ")
                ));
            }
            if !report.session_overrides.is_empty() {
                lines.push(format!(
                    "  session override: {}",
                    report.session_overrides.join(", ")
                ));
            }
        }
        lines.join("\n")
    }

    /// Typed `/settings text` report: the command owns the frame and joining.
    #[must_use]
    pub fn command_report(&self) -> codewhale_command_contract::config::CommandSettingsReport {
        let mut rows = Vec::new();
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "auto_compact:".to_string(),
            value: self.auto_compact.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "auto_compact_pct:".to_string(),
            value: format!("{:.0}", self.auto_compact_threshold_percent),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "calm_mode:".to_string(),
            value: self.calm_mode.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "pet_mode:".to_string(),
            value: self.pet_mode.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "tool_collapse:".to_string(),
            value: self.tool_collapse_mode.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "low_motion:".to_string(),
            value: self.low_motion.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "fancy_animations:".to_string(),
            value: self.fancy_animations.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "focus_texture:".to_string(),
            value: self.focus_texture.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "work_surface:".to_string(),
            value: self.work_surface_placement.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "work_top_height:".to_string(),
            value: self.work_surface_top_height.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "work_side_width:".to_string(),
            value: self.work_surface_side_width.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "rail_panel:".to_string(),
            value: self.rail_panel.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "bracketed_paste:".to_string(),
            value: self.bracketed_paste.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "paste_burst_detect:".to_string(),
            value: self.paste_burst_detection.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "mention_menu_limit:".to_string(),
            value: self.mention_menu_limit.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "mention_walk_depth:".to_string(),
            value: self.mention_walk_depth.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "mention_behavior:".to_string(),
            value: self.mention_menu_behavior.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "show_thinking:".to_string(),
            value: self.show_thinking.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "thinking_expanded:".to_string(),
            value: self.thinking_default_expanded.to_string(),
            label_width: 21,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "thinking_preview:".to_string(),
            value: self.thinking_preview_lines.to_string(),
            label_width: 21,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "thinking_highlight:".to_string(),
            value: self.thinking_highlight.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "help_expand_groups:".to_string(),
            value: self.help_expand_groups.to_string(),
            label_width: 21,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "pin_last_prompt:".to_string(),
            value: self.pin_last_prompt.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "contextual_tips:".to_string(),
            value: self.contextual_tips.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "show_tool_details:".to_string(),
            value: self.show_tool_details.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "inline_diffs:".to_string(),
            value: self.inline_diffs.to_string(),
            label_width: 19,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "locale:".to_string(),
            value: self.locale.to_string(),
            label_width: 19,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "theme:".to_string(),
            value: self.theme.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "background_color:".to_string(),
            value: self
                .background_color
                .as_deref()
                .unwrap_or("(default)")
                .to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "composer_density:".to_string(),
            value: self.composer_density.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "composer_border:".to_string(),
            value: self.composer_border.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "composer_multiline_mode:".to_string(),
            value: self.composer_multiline_mode.to_string(),
            label_width: 25,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "composer_vim_mode:".to_string(),
            value: self.composer_vim_mode.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "transcript_spacing:".to_string(),
            value: self.transcript_spacing.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "status_indicator:".to_string(),
            value: self.status_indicator.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "synchronized_output:".to_string(),
            value: self.synchronized_output.to_string(),
            label_width: 21,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "workspace_follow_symlinks:".to_string(),
            value: self.workspace_follow_symlinks.to_string(),
            label_width: 27,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "default_mode:".to_string(),
            value: self.default_mode.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "context_panel:".to_string(),
            value: self.context_panel.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "cost_currency:".to_string(),
            value: self.cost_currency.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "max_history:".to_string(),
            value: self.max_input_history.to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "model defaults:".to_string(),
            value: "config.toml (use /config)".to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "reasoning_effort:".to_string(),
            value: self
                .reasoning_effort
                .as_deref()
                .unwrap_or("(config/default)")
                .to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "permission_posture:".to_string(),
            value: self
                .permission_posture
                .as_deref()
                .unwrap_or("(config/default)")
                .to_string(),
            label_width: 20,
        });
        rows.push(codewhale_command_contract::config::CommandSettingsRow {
            label: "sandbox_mode:".to_string(),
            value: format!(
                "{}  # filesystem scope (not approval)",
                self.sandbox_mode.as_deref().unwrap_or("(config/default)")
            ),
            label_width: 20,
        });
        // Provenance footer: which keys this load actually owns versus the
        // schema defaults, so `/settings` says what the user set. Session
        // marks only appear when the instance outlives a `set()` (the CLI
        // and editor transactions reload from disk).
        let mut user: Vec<&str> = Vec::new();
        let mut session: Vec<&str> = Vec::new();
        let mut keys: Vec<&str> = self.provenance.keys().map(String::as_str).collect();
        keys.sort_unstable();
        for key in keys {
            match self.provenance(key) {
                Layer::UserConfig => user.push(key),
                Layer::SessionOverride => session.push(key),
                Layer::ManagedPolicy | Layer::CliFlag | Layer::ProjectConfig | Layer::Default => {}
            }
        }
        codewhale_command_contract::config::CommandSettingsReport {
            rows,
            config_file: Self::path()
                .map_or_else(|_| "(unknown)".to_string(), |p| p.display().to_string()),
            from_settings: user.into_iter().map(str::to_string).collect(),
            session_overrides: session.into_iter().map(str::to_string).collect(),
        }
    }

    /// Toggle one exact provider/model pin without touching credentials or
    /// the provider's default route.
    pub fn toggle_pinned_model(&mut self, provider: &str, model: &str) -> bool {
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() || model.eq_ignore_ascii_case("auto") {
            return false;
        }
        if let Some(index) = self.pinned_models.iter().position(|pin| {
            pin.provider.eq_ignore_ascii_case(provider) && pin.model.eq_ignore_ascii_case(model)
        }) {
            self.pinned_models.remove(index);
            return false;
        }
        self.pinned_models.push(PinnedModel {
            provider: provider.to_string(),
            model: model.to_string(),
            label: None,
        });
        true
    }

    pub fn move_pinned_model(&mut self, provider: &str, model: &str, delta: isize) -> bool {
        let Some(index) = self.pinned_models.iter().position(|pin| {
            pin.provider.eq_ignore_ascii_case(provider) && pin.model.eq_ignore_ascii_case(model)
        }) else {
            return false;
        };
        let target = if delta.is_negative() {
            index.saturating_sub(delta.unsigned_abs())
        } else {
            index.saturating_add(delta as usize)
        };
        let target = target.min(self.pinned_models.len().saturating_sub(1));
        if target == index {
            return false;
        }
        let pin = self.pinned_models.remove(index);
        self.pinned_models.insert(target, pin);
        true
    }

    /// Resolved boolean for whether the renderer should wrap each frame in
    /// DEC mode 2026 synchronized output. `auto` and `on` enable; `off`
    /// disables. The `auto` → `off` flip for known-bad terminals happens
    /// earlier in [`Self::apply_env_overrides`]; this method only inspects
    /// the final state.
    #[must_use]
    pub fn synchronized_output_enabled(&self) -> bool {
        !self.synchronized_output.eq_ignore_ascii_case("off")
    }

    /// Runtime bracketed-paste mode after terminal-host quirks are applied.
    ///
    /// This deliberately does not mutate [`Settings::bracketed_paste`]:
    /// `apply_env_overrides()` can run before saving settings, and a legacy
    /// conhost runtime fallback must not permanently disable bracketed paste
    /// when the same config is later used in Windows Terminal or another
    /// modern terminal.
    #[must_use]
    pub fn effective_bracketed_paste(&self) -> bool {
        self.bracketed_paste && !detected_legacy_windows_console_host()
    }
}

fn resolve_settings_path_from_candidates(
    primary: Option<PathBuf>,
    legacy_home: Option<PathBuf>,
    legacy_config_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = primary.as_ref()
        && path.exists()
    {
        return Ok(path.clone());
    }

    if let Some(path) = legacy_home
        && path.exists()
    {
        return Ok(path);
    }

    if let Some(path) = legacy_config_dir.as_ref()
        && path.exists()
    {
        return Ok(path.clone());
    }

    primary.or(legacy_config_dir).ok_or_else(|| {
        anyhow::anyhow!("Failed to resolve settings path: no config directory found.")
    })
}

/// Proof that the caller is inside the settings critical section.
///
/// Only [`with_settings_transaction`] can hand one out, so a `load`/`save` pair
/// on this type is by construction covered by both the process-wide mutex and
/// the cross-process file lock.
pub struct SettingsTransaction {
    path: PathBuf,
}

impl SettingsTransaction {
    /// Read the on-disk values inside the critical section.
    pub fn load(&self) -> Result<Settings> {
        Settings::load_persisted_locked()
    }

    /// Write the whole file inside the critical section.
    pub fn save(&self, settings: &Settings) -> Result<()> {
        settings.save_locked(&self.path)
    }
}

/// Run `operation` as one whole-file settings critical section.
///
/// Most callers want [`Settings::transact`]. Reach for this directly only when a
/// single logical change needs more than one save under one lock — the
/// Shift+Tab root-policy release is the motivating case: it commits the new
/// posture, unsets the shadowing root config key, and must restore the previous
/// posture if that unset fails. Splitting that into two `transact` calls would
/// let another writer observe (and rewrite over) the uncommitted middle state.
///
/// Two locks are taken, in this order, and both are held across disk I/O:
///
/// 1. A process-wide mutex keyed by the resolved settings path. It covers
///    writers that never share an object — a background startup-default drain
///    and a synchronous Shift+Tab permission write, the concrete pair that lost
///    `default_mode` / `permission_posture` against each other.
/// 2. An **advisory file lock on an adjacent `settings.toml.lock`**, following
///    the `codewhale_config::config_document` pattern. The process mutex says
///    nothing about a second Codewhale process (a second TUI, `codewhale exec`,
///    the runtime HTTP surface in another instance) doing its own
///    load/modify/save. Without a cross-process lock those two interleave and
///    the later save reverts the earlier one's field — last-save-wins across
///    processes, which is exactly the bug the in-process lock was added to
///    prevent in-process.
///
/// The lock file is only ever a lock: no settings content is written to it, so
/// a stale one carries nothing to lose.
///
/// There is exactly one permitted lock order for anything that touches
/// `settings.toml`, and every acquisition in the tree below obeys it:
///
/// ```text
/// StartupDefaultsWriter::write  →  settings process mutex  →  settings file lock  →  test env lock  →  test state-I/O lock
/// ```
///
/// Two consequences worth stating, because breaking either is a deadlock:
///
/// - A thread holding a transaction must never wait on
///   `StartupDefaultsWriter::write`. The queued-drain paths (`flush`,
///   `apply_blocking`) take `write` *first* and only then enter a transaction.
/// - Under `cfg(test)` path resolution enters the process-wide env barrier from
///   inside a transaction, so a background thread inside a transaction must be
///   enrolled in the sealing test's env scope (see `tui::startup_defaults`) or it
///   will park on a lock its own test holds.
///
/// Neither lock is re-entrant. `operation` must not call back into `transact`,
/// `Settings::save`, or this function.
pub fn with_settings_transaction<T>(
    operation: impl FnOnce(&SettingsTransaction) -> Result<T>,
) -> Result<T> {
    let path = Settings::path()?;
    let _process_guard = lock_settings_transaction(settings_transaction_mutex(&path));
    with_settings_file_lock(&path, || {
        operation(&SettingsTransaction { path: path.clone() })
    })
}

/// Hold an exclusive advisory lock on `<settings.toml>.lock` for `operation`.
///
/// The lock file is opened (not followed) with owner-only permissions and is
/// created if absent. Dropping the `fd_lock` guard — including on an unwind —
/// releases it, and the OS releases it if the process dies, so a crash cannot
/// wedge another Codewhale instance out of its settings.
fn with_settings_file_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    use std::fs;

    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        anyhow::bail!(
            "Failed to lock settings: {} has no parent directory",
            path.display()
        );
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create config directory {}", parent.display()))?;

    let mut lock_name = path
        .file_name()
        .context("Failed to lock settings: settings path has no file name")?
        .to_os_string();
    lock_name.push(".lock");
    let lock_path = parent.join(lock_name);
    reject_settings_lock_symlink(&lock_path)?;

    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let lock_file = options
        .open(&lock_path)
        .with_context(|| format!("Failed to open settings lock at {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        lock_file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| {
                format!("Failed to secure settings lock at {}", lock_path.display())
            })?;
    }
    if !lock_file
        .metadata()
        .with_context(|| format!("Failed to inspect settings lock at {}", lock_path.display()))?
        .file_type()
        .is_file()
    {
        anyhow::bail!(
            "Refusing a non-regular settings lock at {}",
            lock_path.display()
        );
    }

    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock
        .write()
        .with_context(|| format!("Failed to acquire settings lock at {}", lock_path.display()))?;
    operation()
}

/// Refuse to lock through a symlink: a planted `settings.toml.lock -> …` would
/// otherwise let an attacker pick which file we create with our permissions.
fn reject_settings_lock_symlink(lock_path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => anyhow::bail!(
            "Refusing a symlinked settings lock at {}",
            lock_path.display()
        ),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Replace `path` with `body` by writing an adjacent temporary file and
/// renaming it into place.
///
/// A direct `fs::write` truncates first, so any concurrent reader — another
/// Codewhale process, an editor, a `cat` — can observe a half-written file and
/// parse it as truncated TOML, silently losing every key past the tear. A
/// same-directory temp file plus the platform's replace primitive makes the
/// swap atomic for readers: they see either the whole previous file or the
/// whole new one.
///
/// The temp file inherits the existing file's permission bits when there is one
/// (so a user who tightened `settings.toml` keeps that), and is created
/// owner-only otherwise. `NamedTempFile` removes itself if anything below fails,
/// so a failed save leaves no debris and never damages the previous file.
fn atomically_replace_settings_file(path: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".settings-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("Failed to stage settings write in {}", dir.display()))?;
    tmp.write_all(body)
        .with_context(|| format!("Failed to write settings to {}", path.display()))?;
    tmp.flush()
        .with_context(|| format!("Failed to flush settings for {}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("Failed to sync settings for {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("Failed to set permissions for {}", path.display()))?;
    }

    #[cfg(windows)]
    if path.exists() {
        // `tempfile::persist` uses MoveFileExW on Windows. Under concurrent
        // reads that can expose a partially replaced destination. ReplaceFileW
        // is the native existing-file replacement operation and also preserves
        // the destination's ACLs and attributes.
        let mut temporary = tmp.into_temp_path();
        match replace_existing_settings_file(path, &temporary) {
            Ok(()) => {
                // ReplaceFileW consumed the temporary pathname. Do not ask
                // TempPath to clean up that now-nonexistent source on drop.
                temporary.disable_cleanup(true);
                return Ok(());
            }
            // A directory that refuses every rename — Codewhale's own home is
            // one of them on this machine — will refuse this call forever, so a
            // save that only ever replaces can never land. Rewrite in place
            // instead: not atomic, but a settings file that saves is worth more
            // than one that reports 1175 on every toggle.
            Err(error) if settings_replace_is_transient(&error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "settings replacement refused; rewriting the file in place"
                );
                temporary.disable_cleanup(true);
                drop(temporary);
                rewrite_settings_in_place(path, body)?;
                return Ok(());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to write settings to {}", path.display()));
            }
        }
    }

    tmp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to write settings to {}", path.display()))?;
    Ok(())
}

/// Last-resort delivery for `settings.toml`: rewrite it without a rename.
///
/// Deliberately not atomic — an interruption between truncate and the last byte
/// can leave a partial document — so it runs only after every replacing call
/// has been refused. `Settings::load_persisted` already tolerates a corrupt
/// document by falling back to defaults with `load_error` set, so the worst case
/// here is recoverable, while the alternative is a setting that never saves.
#[cfg(windows)]
fn rewrite_settings_in_place(path: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("Failed to open {} for an in-place write", path.display()))?;
    file.write_all(body)
        .with_context(|| format!("Failed to rewrite settings at {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to sync settings at {}", path.display()))?;
    Ok(())
}

/// How many times a refused replacement is attempted before the error surfaces.
#[cfg(windows)]
const SETTINGS_REPLACE_ATTEMPTS: u32 = 6;

/// Backoff step; attempt *n* sleeps `n * step`, so the last wait is 250ms and
/// the whole budget is 750ms. A `settings.toml` save is a user action (a mode
/// toggle, a `/config ... --save`), so it can afford to outlast a scanner or a
/// second instance holding the file for a moment; it cannot afford to hang.
#[cfg(windows)]
const SETTINGS_REPLACE_RETRY_STEP_MS: u64 = 50;

/// Whether a refused `ReplaceFileW` is worth retrying.
///
/// The destination being held open — by another Codewhale instance's reader, a
/// virus scanner, or an editor's preview pane — is momentary, and every code
/// below says exactly that: 5 `ERROR_ACCESS_DENIED`, 32
/// `ERROR_SHARING_VIOLATION`, 33 `ERROR_LOCK_VIOLATION` and 1175
/// `ERROR_UNABLE_TO_DELETE_FILE`, the one `ReplaceFileW` raises when it cannot
/// displace the destination it must delete first. A permission bit or a full
/// disk is not momentary and must surface at once.
#[cfg(windows)]
fn settings_replace_is_transient(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33 | 1175))
}

#[cfg(windows)]
fn replace_existing_settings_file(path: &Path, replacement: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TEMPORARY, ReplaceFileW, SetFileAttributesW,
    };

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let path_wide = wide_path(path);
    let replacement_wide = wide_path(replacement);
    let mut attempt: u32 = 0;
    loop {
        // SAFETY: both paths are NUL-terminated and live; reserved params are null.
        let error = unsafe {
            // NamedTempFile marks its source with the temporary caching hint.
            // Clear it before publication, matching tempfile's persistence path.
            // A refusal here is about the *source*, so it is never retried: the
            // retry below exists to outlast a held *destination*.
            if SetFileAttributesW(replacement_wide.as_ptr(), FILE_ATTRIBUTE_NORMAL) == 0 {
                return Err(std::io::Error::last_os_error());
            }

            if ReplaceFileW(
                path_wide.as_ptr(),
                replacement_wide.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                return Ok(());
            }

            let error = std::io::Error::last_os_error();
            // Restore the hint so TempPath retains its normal cleanup behavior
            // when replacement fails and the source still exists.
            let _ = SetFileAttributesW(replacement_wide.as_ptr(), FILE_ATTRIBUTE_TEMPORARY);
            error
        };

        attempt += 1;
        if attempt >= SETTINGS_REPLACE_ATTEMPTS || !settings_replace_is_transient(&error) {
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(
            u64::from(attempt) * SETTINGS_REPLACE_RETRY_STEP_MS,
        ));
    }
}

/// Per-settings-path transaction mutexes.
///
/// Keyed by path rather than global because tests seal `HOME` onto their own
/// temp dirs: two sealed tests write different files and have no reason to
/// serialize against each other. Production has exactly one entry, so the
/// registry never grows; entries are intentionally `'static` (leaked once) so a
/// transaction can hold a plain `MutexGuard` without also pinning the registry
/// lock it came from.
fn settings_transaction_mutex(path: &Path) -> &'static std::sync::Mutex<()> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, &'static Mutex<()>>>> = OnceLock::new();
    let key = path.to_path_buf();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mutex: &'static Mutex<()> = locks
        .entry(key)
        .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))));
    drop(locks);
    mutex
}

/// Acquire a transaction lock.
///
/// The mutex protects ordering, not an invariant, so a panic inside one
/// transaction must not wedge settings persistence for the rest of the session:
/// a poisoned guard is recovered rather than propagated.
#[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
fn lock_settings_transaction(
    mutex: &'static std::sync::Mutex<()>,
) -> std::sync::MutexGuard<'static, ()> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Test build of [`lock_settings_transaction`], with a watchdog.
///
/// Production blocks indefinitely, which is correct — the only thing ahead of it
/// is a bounded settings transaction. In a test binary an indefinite wait is
/// indistinguishable from a lock-order inversion, and a hung test job reports
/// nothing. This is not a synchronization device: every honest acquisition
/// succeeds on the first `try_lock` or shortly after. It exists so a regression
/// fails loudly instead of hanging CI.
///
/// The deadline is generous on purpose. A transaction still reads and writes
/// under `cfg(test)`'s state-I/O barrier, and the cross-process file lock can be
/// held by a deliberately slow child process in the cross-process regressions —
/// so the watchdog only has to be longer than the slowest honest transaction and
/// shorter than a CI job timeout, not tight.
#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
fn lock_settings_transaction(
    mutex: &'static std::sync::Mutex<()>,
) -> std::sync::MutexGuard<'static, ()> {
    use std::sync::TryLockError;

    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "settings transaction lock was not released within {DEADLINE:?}. Some thread is \
             holding it across a load/modify/save that cannot finish — usually because it is \
             blocked on a lock this test already holds, or because a transaction was opened \
             re-entrantly. See Settings::transact."
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn settings_path_candidates() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    settings_path_candidates_for_scope(true)
}

fn settings_path_candidates_for_scope(
    include_config_override: bool,
) -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let from_environment = || {
        if include_config_override {
            settings_path_candidates_from_environment()
        } else {
            home_settings_path_candidates_from_environment()
        }
    };
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        let honor_guarded_environment =
            crate::test_support::guarded_environment_provides_state_paths();
        crate::test_support::with_test_env_lock(|| {
            // A project-path guard cannot authorize a reader that deliberately
            // ignores that path. Likewise, a guarded HOME must not expose an
            // ambient CODEWHALE_HOME that takes precedence over it.
            let home_is_guarded = || {
                let present = |var| std::env::var_os(var).is_some_and(|value| !value.is_empty());
                if present("CODEWHALE_HOME") {
                    crate::test_support::env_var_currently_guarded("CODEWHALE_HOME")
                } else {
                    ["HOME", "USERPROFILE"].iter().any(|var| {
                        crate::test_support::env_var_currently_guarded(var) && present(var)
                    })
                }
            };
            if honor_guarded_environment && (include_config_override || home_is_guarded()) {
                from_environment()
            } else {
                (
                    Some(crate::test_support::unsealed_test_state_root().join(SETTINGS_FILE_NAME)),
                    None,
                    None,
                )
            }
        })
    }

    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    from_environment()
}

fn settings_path_candidates_from_environment() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)
{
    // Allow tests to override the settings directory via the same env vars
    // used for config. CODEWHALE_CONFIG_PATH is canonical; the legacy alias
    // remains a read-only fallback for existing installs.
    if let Some(parent) = config_override_parent() {
        return (Some(parent.join(SETTINGS_FILE_NAME)), None, None);
    }

    home_settings_path_candidates_from_environment()
}

fn home_settings_path_candidates_from_environment()
-> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let primary = codewhale_config::codewhale_home()
        .ok()
        .map(|home| home.join(SETTINGS_FILE_NAME));
    if codewhale_config::codewhale_home_is_explicit() {
        return (primary, None, None);
    }
    let legacy_home = codewhale_config::legacy_deepseek_home()
        .ok()
        .map(|home| home.join(SETTINGS_FILE_NAME));
    let legacy_config_dir =
        dirs::config_dir().map(|dir| dir.join("deepseek").join(SETTINGS_FILE_NAME));

    (primary, legacy_home, legacy_config_dir)
}

fn config_override_parent() -> Option<PathBuf> {
    fn read() -> Option<PathBuf> {
        for var in ["CODEWHALE_CONFIG_PATH", "DEEPSEEK_CONFIG_PATH"] {
            if let Ok(config_path) = std::env::var(var) {
                let config_path = config_path.trim();
                if !config_path.is_empty() {
                    return expand_path(config_path).parent().map(Path::to_path_buf);
                }
            }
        }
        None
    }

    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    {
        read()
    }
}

fn migrate_settings_file_to_primary_if_needed(primary: &Path, active_read_path: &Path) {
    use std::io::Write as _;

    if primary == active_read_path || primary.exists() || !active_read_path.exists() {
        return;
    }

    let Some(parent) = primary.parent() else {
        return;
    };

    if let Err(err) = std::fs::create_dir_all(parent) {
        tracing::warn!(
            "failed to create settings migration directory {}: {err}",
            parent.display()
        );
        return;
    }

    let migration = (|| -> Result<()> {
        let body = std::fs::read(active_read_path).with_context(|| {
            format!(
                "Failed to read legacy settings from {}",
                active_read_path.display()
            )
        })?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".settings-migration-")
            .suffix(".tmp")
            .tempfile_in(parent)
            .with_context(|| {
                format!("Failed to stage settings migration in {}", parent.display())
            })?;
        tmp.write_all(&body).with_context(|| {
            format!(
                "Failed to stage legacy settings from {}",
                active_read_path.display()
            )
        })?;
        tmp.flush()
            .context("Failed to flush staged settings migration")?;
        tmp.as_file()
            .sync_all()
            .context("Failed to sync staged settings migration")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(active_read_path)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0o600);
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(mode))
                .context("Failed to preserve legacy settings permissions")?;
        }

        match tmp.persist_noclobber(primary) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.error).with_context(|| {
                format!(
                    "Failed to install migrated settings at {}",
                    primary.display()
                )
            }),
        }
    })();

    if let Err(err) = migration {
        tracing::warn!(
            "failed to migrate settings from {} to {}: {err}",
            active_read_path.display(),
            primary.display()
        );
    }
}

fn normalize_default_model(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("auto") {
        Some("auto".to_string())
    } else {
        normalize_model_name(trimmed)
    }
}

fn normalize_permission_posture(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ask" | "suggest" | "on-request" | "untrusted" => Some("ask".to_string()),
        "auto" | "auto-review" | "auto_review" => Some("auto-review".to_string()),
        "full" | "full-access" | "full_access" | "bypass" => Some("full-access".to_string()),
        _ => None,
    }
}

/// Normalize filesystem sandbox mode. Distinct from permission posture.
fn normalize_sandbox_mode(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "read-only" | "readonly" | "read_only" | "ro" => Some("read-only".to_string()),
        "workspace-write" | "workspace_write" | "workspace" | "workspace-only" => {
            Some("workspace-write".to_string())
        }
        "danger-full-access" | "danger_full_access" | "full-fs" | "full_filesystem"
        | "filesystem-full" => Some("danger-full-access".to_string()),
        "external-sandbox" | "external_sandbox" | "opensandbox" | "external" => {
            Some("external-sandbox".to_string())
        }
        _ => None,
    }
}

fn normalize_reasoning_effort_setting(value: &str) -> Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "default" | "(default)" | "config" | "configured" | "unset"
        )
    {
        return Ok(None);
    }

    codewhale_config::settings_schema::canonical_reasoning_effort(trimmed)
        .map(|effort| Some(effort.to_string()))
        .map_err(|err| anyhow::anyhow!("Failed to update setting: {err}"))
}

/// Parse a boolean value from various formats
pub(crate) fn parse_bool(value: &str) -> Result<bool> {
    match value.to_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "enabled" => Ok(true),
        "off" | "false" | "no" | "0" | "disabled" => Ok(false),
        _ => {
            anyhow::bail!("Failed to parse boolean '{value}': expected on/off, true/false, yes/no.")
        }
    }
}

fn default_thinking_preview_lines() -> usize {
    2
}

fn default_true() -> bool {
    true
}

fn parse_usize_setting(key: &str, value: &str) -> Result<usize> {
    value.trim().parse::<usize>().map_err(|_| {
        anyhow::anyhow!(
            "Failed to update setting: invalid {key} '{value}'. Expected 0 or a positive integer."
        )
    })
}

fn parse_u16_range(key: &str, value: &str, min: u16, max: u16) -> Result<u16> {
    let parsed = value
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("Invalid {key} '{value}': expected {min}-{max}"))?;
    if !(min..=max).contains(&parsed) {
        anyhow::bail!("Invalid {key} '{value}': expected {min}-{max}");
    }
    Ok(parsed)
}

fn parse_percent_setting(key: &str, value: &str) -> Result<f64> {
    let trimmed = value.trim().trim_end_matches('%').trim();
    let percent = trimmed.parse::<f64>().map_err(|_| {
        anyhow::anyhow!(
            "Failed to update setting: invalid {key} '{value}'. Expected a number from 10 to 100."
        )
    })?;
    if !(10.0..=100.0).contains(&percent) {
        anyhow::bail!(
            "Failed to update setting: invalid {key} '{value}'. Expected a number from 10 to 100."
        );
    }
    Ok(percent)
}

fn normalize_mention_menu_behavior(value: &str) -> Result<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "fuzzy" | "default" => Ok("fuzzy".to_string()),
        "browser" | "browse" | "file-browser" | "file_browser" => Ok("browser".to_string()),
        _ => {
            anyhow::bail!(
                "Failed to update setting: invalid mention_menu_behavior '{value}'. Expected: fuzzy, browser."
            )
        }
    }
}

fn normalize_mode(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "edit" => "agent",
        "normal" => "agent",
        "agent" | "act" | "work" => "agent",
        "plan" => "plan",
        // Operate is a first-class startup mode (Hunter 2026-07-24).
        "operate" | "operation" | "ops" => "operate",
        // yolo was mode+permission; keep mode as Act and migrate posture on load.
        "yolo" => "agent",
        _ => value,
    }
}

fn normalize_composer_density(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" | "tight" => "compact",
        "comfortable" | "default" | "normal" => "comfortable",
        "spacious" | "loose" => "spacious",
        _ => value,
    }
}

fn normalize_transcript_spacing(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" | "tight" => "compact",
        "comfortable" | "default" | "normal" => "spacious",
        "spacious" | "loose" => "spacious",
        _ => value,
    }
}

fn normalize_tool_collapse_mode(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" | "collapsed" | "collapse" | "default" | "on" | "true" => "compact",
        "expanded" | "expand" | "off" | "none" | "false" => "expanded",
        "calm" | "calm_mode" | "calm-mode" | "calm_only" | "calm-only" => "calm",
        _ => value,
    }
}

/// Normalize the `status_indicator` header chip setting. Accepts the
/// canonical names plus common aliases ("none"/"hidden" → "off",
/// "dot" → "dots"). Unknown values fall through unchanged so the parser
/// in `update_setting` can surface a clear error.
fn normalize_status_indicator(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "cw" | "mark" | "text" => "cw",
        // The whale emoji header chip is retired (2026-07-23): persisted
        // opt-ins migrate to the typographic mark on load.
        "whale" | "🐳" | "🐋" => "cw",
        "dots" | "dot" => "dots",
        "off" | "none" | "hidden" | "false" => "off",
        _ => value,
    }
}

/// Normalize the `synchronized_output` setting. Accepts the canonical
/// `"auto"` / `"on"` / `"off"` plus the usual truthy/falsey spellings.
/// Unknown values fall through unchanged so the parser in `set` can
/// surface a clear error.
fn normalize_synchronized_output(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" | "default" => "auto",
        "on" | "true" | "yes" | "1" | "enabled" => "on",
        "off" | "false" | "no" | "0" | "disabled" => "off",
        _ => value,
    }
}

fn normalize_settings_theme(value: &str) -> String {
    // Unknown persisted selectors fall back to the same fresh-install default.
    // Valid saved choices, including Shoreline, remain unchanged.
    normalize_theme_setting(value).unwrap_or_else(|_| DEFAULT_TUI_THEME.to_string())
}

/// Returns `true` when the active terminal is Ptyxis (the new default
/// terminal on Ubuntu 26.04). Used by [`Settings::apply_env_overrides`]
/// to flip `synchronized_output` from `auto` to `off` so DEC mode 2026
/// flicker on Ptyxis 50.x + VTE 0.84.x stops at the source.
///
/// We deliberately keep this narrow:
///
/// - `TERM_PROGRAM` matches `ptyxis` case-insensitively (the value
///   Ptyxis sets when it forwards a process-launch context).
/// - `PTYXIS_VERSION` is set to any non-empty value (the binary's
///   own version probe, present whether or not `TERM_PROGRAM` made it
///   into the child environment).
///
/// Either signal is sufficient. We do *not* trigger on `VTE_VERSION`
/// alone because gnome-terminal 3.58 ships with the same VTE 0.84.x
/// and renders cleanly — broadening the heuristic would regress every
/// gnome-terminal user.
pub fn detected_ptyxis_terminal() -> bool {
    if let Ok(program) = std::env::var("TERM_PROGRAM")
        && program.trim().to_ascii_lowercase().contains("ptyxis")
    {
        return true;
    }
    matches!(std::env::var("PTYXIS_VERSION"), Ok(v) if !v.trim().is_empty())
}

/// Returns `true` for the unmarked Windows console-host path used by plain
/// PowerShell / cmd.exe. Modern Windows terminals set at least one marker that
/// lets us keep the richer rendering path.
pub fn detected_legacy_windows_console_host() -> bool {
    cfg!(windows)
        && legacy_windows_console_host_env([
            std::env::var_os("WT_SESSION").as_deref(),
            std::env::var_os("ConEmuPID").as_deref(),
            std::env::var_os("TERM_PROGRAM").as_deref(),
            std::env::var_os("WEZTERM_EXECUTABLE").as_deref(),
            std::env::var_os("WEZTERM_PANE").as_deref(),
            std::env::var_os("ALACRITTY_WINDOW_ID").as_deref(),
            std::env::var_os("ANSICON").as_deref(),
            std::env::var_os("TERM").as_deref(),
        ])
}

fn legacy_windows_console_host_env(markers: [Option<&std::ffi::OsStr>; 8]) -> bool {
    fn has_value(value: Option<&std::ffi::OsStr>) -> bool {
        value.is_some_and(|v| !v.is_empty())
    }

    markers.into_iter().all(|value| !has_value(value))
}

fn normalize_optional_background_color(value: Option<&str>) -> Option<String> {
    value.and_then(|raw| normalize_background_color_setting(raw).ok().flatten())
}

fn normalize_background_color_setting(value: &str) -> Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "default" | "none" | "reset" | "off"
        )
    {
        return Ok(None);
    }

    normalize_hex_rgb_color(trimmed).map(Some).ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to update setting: invalid background_color '{value}'. Expected #RRGGBB, RRGGBB, or default."
        )
    })
}

fn normalize_sidebar_focus(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "pinned" | "visible" | "show" | "on" | "work" | "plan" | "todos" => "pinned",
        "tasks" | "activity" | "live" | "running" => "tasks",
        "agents" | "subagents" | "sub-agents" => "agents",
        "context" => "context",
        "sessions" | "sessions_rail" | "session_history" => "sessions",
        "hidden" | "hide" | "closed" | "off" | "none" => "hidden",
        _ => "auto",
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Resolve an environment variable as a boolean. Recognises the
/// common truthy spellings (`1`, `true`, `yes`, `on`) case-
/// insensitively. Used by [`Settings::apply_env_overrides`] for
/// platform a11y signals like `NO_ANIMATIONS`.
fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused `ReplaceFileW` is retried only when the destination is being
    /// held, never when the platform is refusing the write itself.
    ///
    /// 1175 is the one this module was reported against: `ReplaceFileW` cannot
    /// displace a destination that another handle has open without
    /// `FILE_SHARE_DELETE`, so `/config pet_mode --save` answered
    /// "无法删除要被替换的文件。(os error 1175)" for a write it could have made a
    /// moment later.
    #[cfg(windows)]
    #[test]
    fn only_a_held_destination_is_retried() {
        for code in [5, 32, 33, 1175] {
            assert!(
                settings_replace_is_transient(&std::io::Error::from_raw_os_error(code)),
                "os error {code} is contention and must be retried"
            );
        }
        for code in [2, 3, 112, 87] {
            assert!(
                !settings_replace_is_transient(&std::io::Error::from_raw_os_error(code)),
                "os error {code} is not contention and must surface at once"
            );
        }
    }

    /// The shared guard, under this module's historical name.
    ///
    /// It was a byte-for-byte copy of `EnvVarGuard` until #5359 gave the shared
    /// one a second job: recording which variables a test actually redirected,
    /// so state-path resolution can tell a sealed environment from a test that
    /// holds the lock for unrelated reasons. A private copy silently opts every
    /// caller here out of that record.
    use crate::test_support::EnvVarGuard as EnvVarRestore;

    /// The override detector names the same winner `apply_env_overrides`
    /// applies: `NO_ANIMATIONS` is first in precedence and is environment,
    /// not terminal, authority.
    #[test]
    fn low_motion_override_detector_agrees_with_env_overlay() {
        let _lock = crate::test_support::lock_test_env();
        let _no_animations = crate::test_support::EnvVarGuard::set("NO_ANIMATIONS", "1");

        let detected = detect_low_motion_override();
        assert_eq!(detected, Some(MotionOverride::NoAnimationsEnv));
        assert!(detected.is_some_and(MotionOverride::is_environment));
        assert_eq!(detected.map(MotionOverride::label), Some("NO_ANIMATIONS"));

        let mut settings = Settings::default();
        assert!(!settings.low_motion);
        settings.apply_env_overrides();
        assert!(settings.low_motion, "the overlay forces low motion on");
        assert!(!settings.fancy_animations);
    }

    // -----------------------------------------------------------------------
    // Cross-process settings integrity
    // -----------------------------------------------------------------------
    //
    // The in-process mutex says nothing about a *second* Codewhale process on
    // the same home directory — a second TUI, `codewhale exec`, the runtime HTTP
    // surface in another instance. Two of those doing load/modify/save at once
    // is the same last-save-wins bug the in-process lock was added to prevent,
    // and no amount of thread-based testing can observe it: threads share the
    // mutex that makes the bug impossible. These regressions therefore drive a
    // real child process.
    //
    // The child is this same test binary, re-invoked with `--ignored --exact`
    // on the helper below. It inherits the sealed `HOME`/`CODEWHALE_HOME`
    // through its environment, so both processes resolve the same
    // `settings.toml`.

    /// Selects which child behavior [`settings_cross_process_child_helper`] runs.
    const CHILD_ROLE_ENV: &str = "CODEWHALE_TEST_SETTINGS_CHILD_ROLE";
    /// Path of the parent↔child handshake file. Its meaning is per-role: the
    /// slow writer *creates* it once its transaction is open; the reader *waits*
    /// for it as a stop signal.
    const CHILD_SIGNAL_ENV: &str = "CODEWHALE_TEST_SETTINGS_CHILD_SIGNAL";
    /// Where the child writes what it observed, for the parent to assert on.
    const CHILD_RESULT_ENV: &str = "CODEWHALE_TEST_SETTINGS_CHILD_RESULT";

    /// The other process in the cross-process regressions.
    ///
    /// Ignored so a normal `cargo test` never runs it directly; the parent tests
    /// invoke it explicitly with `--ignored --exact`. With no role set it is a
    /// no-op, so an accidental `--ignored` sweep stays green.
    #[test]
    #[ignore = "spawned as a child process by the cross-process settings regressions"]
    fn settings_cross_process_child_helper() {
        use std::time::{Duration, Instant};

        let Ok(role) = std::env::var(CHILD_ROLE_ENV) else {
            return;
        };
        // Under `cfg(test)` the settings path only honors the real environment
        // for a thread that holds this lock; without it the child would resolve
        // the isolated per-process test root and never touch the parent's file.
        // The child is a fresh process, so the acquisition is uncontended.
        let _env_lock = crate::test_support::lock_test_env();
        let inherited_home = std::env::var_os("CODEWHALE_HOME")
            .expect("settings child needs an inherited Codewhale home");
        let _state_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", inherited_home);
        let signal = PathBuf::from(
            std::env::var(CHILD_SIGNAL_ENV).expect("child helper needs a signal path"),
        );

        match role.as_str() {
            // Hold the settings critical section open across a visible delay, so
            // the parent's transaction is guaranteed to arrive while this one is
            // mid-flight.
            "slow-writer" => {
                with_settings_transaction(|transaction| {
                    let mut settings = transaction.load()?;
                    settings.default_mode = "operate".to_string();
                    // Announce *after* the read: from here on, any parent write
                    // that is not excluded by the lock will be lost by the save
                    // below.
                    std::fs::write(&signal, b"loaded").expect("write the handshake file");
                    std::thread::sleep(Duration::from_millis(1_500));
                    transaction.save(&settings)
                })
                .expect("the child transaction must commit");
            }
            // Read the raw file as fast as possible while the parent rewrites
            // it, and report how many reads were torn.
            "reader" => {
                let result = PathBuf::from(
                    std::env::var(CHILD_RESULT_ENV).expect("reader needs a result path"),
                );
                let path = Settings::path().expect("resolve the shared settings path");
                let ready = result.with_extension("ready");
                let deadline = Instant::now() + Duration::from_secs(60);
                let (mut reads, mut torn) = (0_u64, 0_u64);

                // A ready marker must mean that the reader has actually run.
                // On Windows the child can otherwise create the marker, lose
                // its time slice, and perform no reads before the parent
                // completes every write and signals it to stop.
                loop {
                    assert!(
                        Instant::now() < deadline,
                        "reader did not observe the seeded settings file"
                    );
                    match std::fs::read_to_string(&path) {
                        Ok(raw)
                            if !raw.is_empty() && toml::from_str::<toml::Value>(&raw).is_ok() =>
                        {
                            reads += 1;
                            break;
                        }
                        Ok(_) | Err(_) => std::thread::yield_now(),
                    }
                }
                std::fs::write(&ready, b"ready").expect("announce that the reader is ready");

                while !path_exists_for_test(&signal) && Instant::now() < deadline {
                    let Ok(raw) = std::fs::read_to_string(&path) else {
                        // The file legitimately does not exist yet.
                        continue;
                    };
                    reads += 1;
                    // Both failure shapes a truncate-then-write produces: the
                    // momentarily empty file, and a prefix that stops mid-value.
                    if raw.is_empty() || toml::from_str::<toml::Value>(&raw).is_err() {
                        torn += 1;
                    }
                }
                std::fs::write(&result, format!("{reads} {torn}")).expect("write the result file");
            }
            other => panic!("unknown child role {other}"),
        }
    }

    fn path_exists_for_test(path: &Path) -> bool {
        std::fs::metadata(path).is_ok()
    }

    /// Spawn this test binary as a child running the helper above in `role`.
    fn spawn_settings_child(
        role: &str,
        home: &Path,
        signal: &Path,
        result: Option<&Path>,
    ) -> std::process::Child {
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("the test binary path is the child program"),
        );
        command
            .arg("settings::tests::settings_cross_process_child_helper")
            .args(["--exact", "--ignored", "--test-threads", "1"])
            .env(CHILD_ROLE_ENV, role)
            .env(CHILD_SIGNAL_ENV, signal)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("CODEWHALE_HOME", home.join(".codewhale"))
            .env_remove("DEEPSEEK_CONFIG_PATH")
            .env_remove("CODEWHALE_CONFIG_PATH")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(result) = result {
            command.env(CHILD_RESULT_ENV, result);
        }
        command.spawn().expect("spawn the settings child process")
    }

    fn seal_settings_home_for_test(home: &Path) -> Vec<crate::test_support::EnvVarGuard> {
        use crate::test_support::EnvVarGuard;
        vec![
            EnvVarGuard::set("HOME", home),
            EnvVarGuard::set("USERPROFILE", home),
            EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale")),
            EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH"),
            EnvVarGuard::remove("CODEWHALE_CONFIG_PATH"),
        ]
    }

    fn wait_for_file(path: &Path, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !path_exists_for_test(path) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what} at {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Two processes mutating **disjoint** fields must both survive.
    ///
    /// The child opens a transaction, reads the pre-image, announces itself, and
    /// only then saves `default_mode`. The parent's `max_history` write arrives
    /// squarely inside that window. Without the cross-process lock the parent
    /// loads the same pre-image, saves, and is then overwritten wholesale by the
    /// child's later save — `max_history` silently reverts. With the lock the
    /// parent waits, re-reads the child's committed value, and both fields land.
    #[test]
    fn two_processes_mutating_disjoint_fields_do_not_last_save_wins() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _env = seal_settings_home_for_test(tmp.path());

        // A real pre-image, so "whichever saves last wins" has something to
        // revert rather than a fresh file.
        Settings::transact(|settings| settings.set("max_history", "100"))
            .expect("seed the settings file");
        let signal = tmp.path().join("child-transaction-open");

        let mut child = spawn_settings_child("slow-writer", tmp.path(), &signal, None);
        wait_for_file(&signal, "the child's open transaction");

        // The child is mid-transaction right now. This must block, not race.
        Settings::transact(|settings| settings.set("max_history", "321"))
            .expect("the parent write must land once the child releases the lock");

        let status = child.wait().expect("await the child process");
        assert!(status.success(), "the child transaction must succeed");

        let settled = Settings::load_persisted().expect("reload the shared settings");
        assert_eq!(
            settled.default_mode, "operate",
            "the child's field must survive the parent's whole-file save"
        );
        assert_eq!(
            settled.max_input_history, 321,
            "the parent's field must survive the child's whole-file save"
        );
    }

    /// A concurrent reader must never observe a half-written `settings.toml`.
    ///
    /// `fs::write` truncates before it writes, so any other process reading at
    /// the wrong moment sees an empty file or a prefix that stops mid-value —
    /// and parses it as a settings file that is simply missing everything past
    /// the tear. Writing to an adjacent temp file and renaming makes the swap
    /// atomic: a reader sees either the whole old file or the whole new one.
    #[test]
    fn concurrent_readers_never_observe_a_truncated_settings_file() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _env = seal_settings_home_for_test(tmp.path());

        // Make the file big enough that a non-atomic write has a real window.
        // A short file can be written in one syscall and hide the bug.
        Settings::transact(|settings| {
            settings.pinned_models = (0..400)
                .map(|index| PinnedModel {
                    provider: "deepseek".to_string(),
                    model: format!("pinned-model-{index:04}"),
                    label: Some(format!("Pinned model {index:04}")),
                })
                .collect();
            Ok(())
        })
        .expect("seed a large settings file");

        let stop = tmp.path().join("reader-stop");
        let result = tmp.path().join("reader-result");
        let ready = result.with_extension("ready");
        let mut child = spawn_settings_child("reader", tmp.path(), &stop, Some(&result));
        wait_for_file(&ready, "the settings reader to become ready");

        for index in 0..150 {
            Settings::transact(|settings| settings.set("max_history", &(100 + index).to_string()))
                .expect("the parent write must land");
        }

        std::fs::write(&stop, b"stop").expect("signal the reader to stop");
        let status = child.wait().expect("await the reader process");
        assert!(status.success(), "the reader must exit cleanly");

        let observed = std::fs::read_to_string(&result).expect("read the reader's report");
        let mut parts = observed.split_whitespace();
        let reads: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let torn: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        assert!(
            reads > 0,
            "the reader observed nothing, so it proves nothing (report: {observed:?})"
        );
        assert_eq!(
            torn, 0,
            "{torn} of {reads} concurrent reads saw a truncated or unparseable settings file"
        );
    }

    #[test]
    fn focus_texture_defaults_off_and_validates() {
        let mut settings = Settings::default();
        assert_eq!(settings.focus_texture, "off");

        settings.set("focus_texture", "scrim").unwrap();
        assert_eq!(settings.focus_texture, "scrim");
        settings.set("texture", "grain").unwrap();
        assert_eq!(settings.focus_texture, "grain");
        settings.set("focus_texture", " OFF ").unwrap();
        assert_eq!(settings.focus_texture, "off");

        let err = settings.set("focus_texture", "static").unwrap_err();
        assert!(err.to_string().contains("off, scrim, or grain"));
    }

    #[test]
    fn retired_ocean_treatment_folds_into_the_underwater_theme() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "theme = \"light\"\nocean_treatment = \"deepsea\"\n")
            .expect("legacy settings");

        let settings = Settings::load_persisted_from_candidates(Some(path.clone()), None, None)
            .expect("legacy setting must remain readable");
        assert_eq!(
            settings.theme, "underwater",
            "the persisted painted field is the user-visible fact; it becomes the theme"
        );

        settings
            .save_to_path(&path)
            .expect("save normalized settings");
        let saved = std::fs::read_to_string(&path).expect("read normalized settings");
        assert!(
            !saved.contains("ocean_treatment"),
            "the retired key must not be written back: {saved}"
        );
        assert!(saved.contains("theme = \"underwater\""), "{saved}");
    }

    #[test]
    fn flat_ocean_treatment_leaves_the_theme_alone_and_is_dropped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "theme = \"light\"\nocean_treatment = \"flat\"\n")
            .expect("legacy settings");

        let settings = Settings::load_persisted_from_candidates(Some(path.clone()), None, None)
            .expect("legacy setting must remain readable");
        assert_eq!(
            settings.theme, "light",
            "flat never opted into a painted field"
        );

        settings
            .save_to_path(&path)
            .expect("save normalized settings");
        let saved = std::fs::read_to_string(&path).expect("read normalized settings");
        assert!(!saved.contains("ocean_treatment"), "{saved}");
        assert!(saved.contains("theme = \"light\""), "{saved}");
    }

    #[test]
    fn work_surface_placement_persists_all_placements_with_bottom_default() {
        let mut settings = Settings::default();
        // Round 3 (2026-09-01): the bar's information lives under the
        // composer, so Bottom is the default.
        assert_eq!(settings.work_surface_placement, "bottom");

        for placement in ["bottom", "top", "left", "right", "off"] {
            settings
                .set("work_surface_placement", placement)
                .expect("valid placement");
            assert_eq!(settings.work_surface_placement, placement);
            let body = toml::to_string(&settings).expect("serialize settings");
            let restored: Settings = toml::from_str(&body).expect("restore settings");
            assert_eq!(restored.work_surface_placement, placement);
        }

        let err = settings
            .set("work_surface_placement", "diagonal")
            .expect_err("nonsense placement");
        assert!(err.to_string().contains("top, bottom, left, right, or off"));
        assert_eq!(settings.work_surface_placement, "off");
    }

    #[test]
    fn rail_panel_persists_every_dock_panel_and_folds_pinned_into_tasks() {
        let mut settings = Settings::default();
        assert_eq!(settings.rail_panel, "tasks");

        // Every panel the dock cycles through must survive `set` and a
        // settings.toml round trip — including an empty Terminal view.
        for panel in [
            "tasks",
            "agents",
            "background",
            "terminal",
            "files",
            "notepad",
            "context",
            "git",
            "price",
        ] {
            settings.set("rail_panel", panel).expect("valid panel");
            assert_eq!(settings.rail_panel, panel);
            let body = toml::to_string(&settings).expect("serialize settings");
            let restored: Settings = toml::from_str(&body).expect("restore settings");
            assert_eq!(restored.rail_panel, panel);
        }

        // `pinned` stays accepted as a setting word but persists as the
        // canonical tasks view, matching the load-time migration.
        settings.set("rail_panel", "agents").expect("reset panel");
        settings.set("rail_panel", "pinned").expect("pinned alias");
        assert_eq!(settings.rail_panel, "tasks");

        let err = settings
            .set("rail_panel", "auto")
            .expect_err("auto-collapse was dropped with the legacy sidebar");
        assert!(err.to_string().contains(
            "tasks, agents, background, terminal, files, notepad, context, git, or price"
        ));
        assert_eq!(settings.rail_panel, "tasks");
    }

    #[test]
    fn work_surface_drag_sizes_round_trip_with_bounded_values() {
        let mut settings = Settings::default();
        settings.set("work_surface_top_height", "9").unwrap();
        settings.set("work_surface_side_width", "54").unwrap();
        let body = toml::to_string(&settings).expect("serialize settings");
        let restored: Settings = toml::from_str(&body).expect("restore settings");
        assert_eq!(restored.work_surface_top_height, 9);
        assert_eq!(restored.work_surface_side_width, 54);
        assert!(settings.set("work_surface_top_height", "17").is_err());
        assert!(settings.set("work_surface_top_height", "4").is_err());
        assert!(settings.set("work_surface_side_width", "25").is_err());
    }

    #[test]
    fn settings_load_keeps_top_placement_chosen_after_the_bottom_migration() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let settings_path = tmp.path().join("settings.toml");
        std::fs::write(
            &settings_path,
            "work_surface_placement = \"top\"\nwork_surface_bottom_migrated = true\n",
        )
        .expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");
        assert_eq!(loaded.work_surface_placement, "top");
    }

    #[test]
    fn settings_load_migrates_unreadable_top_work_surface_height() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let settings_path = tmp.path().join("settings.toml");
        let legacy = "work_surface_placement = \"top\"\nwork_surface_top_height = 2\nrail_panel = \"pinned\"\n";
        std::fs::write(&settings_path, legacy).expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        assert_eq!(loaded.work_surface_top_height, WORK_SURFACE_TOP_HEIGHT_MIN);
        // Round 3: a persisted `top` from before the default moved is the
        // old default, migrated once to `bottom` (0.9.12 defect #9).
        assert_eq!(loaded.work_surface_placement, "bottom");
        assert!(loaded.work_surface_bottom_migrated);
        // `pinned` folded into the tasks view (2026-09-02 dock views).
        assert_eq!(loaded.rail_panel, "tasks");
        assert_eq!(
            std::fs::read_to_string(settings_path).expect("read unchanged settings"),
            legacy,
            "normalizing a legacy height at read time must not rewrite the user's file"
        );
    }

    #[test]
    fn inline_diffs_default_full_and_persist_exactly_one_mode() {
        let mut settings = Settings::default();
        assert_eq!(settings.inline_diffs, "full");
        assert_eq!(
            InlineDiffMode::parse(&settings.inline_diffs),
            InlineDiffMode::Full
        );

        for mode in ["summary", "off", "full"] {
            settings.set("inline_diffs", mode).expect("valid mode");
            assert_eq!(settings.inline_diffs, mode);
            let body = toml::to_string(&settings).expect("serialize settings");
            let restored: Settings = toml::from_str(&body).expect("restore settings");
            assert_eq!(restored.inline_diffs, mode);
        }

        let error = settings
            .set("inline_diffs", "compact")
            .expect_err("unknown mode must not be guessed");
        assert!(error.to_string().contains("full, summary, or off"));
        assert_eq!(settings.inline_diffs, "full");
    }

    #[test]
    fn thinking_highlight_is_independently_configurable_and_persisted() {
        let mut settings = Settings::default();
        assert!(settings.thinking_highlight);

        settings
            .set("thinking_highlight", "false")
            .expect("valid thinking highlight setting");
        assert!(!settings.thinking_highlight);

        let restored: Settings =
            toml::from_str(&toml::to_string(&settings).expect("serialize settings"))
                .expect("restore settings");
        assert!(!restored.thinking_highlight);
    }

    #[test]
    fn thinking_default_expanded_is_opt_in_and_persisted() {
        let mut settings = Settings::default();
        assert!(!settings.thinking_default_expanded);

        settings
            .set("thinking_default_expanded", "true")
            .expect("valid thinking expansion setting");
        assert!(settings.thinking_default_expanded);

        let restored: Settings =
            toml::from_str(&toml::to_string(&settings).expect("serialize settings"))
                .expect("restore settings");
        assert!(restored.thinking_default_expanded);
    }

    #[test]
    fn density_knobs_default_compact_and_persist() {
        let mut settings = Settings::default();
        assert_eq!(settings.thinking_preview_lines, 2);
        assert!(!settings.help_expand_groups);
        assert!(settings.pin_last_prompt);

        settings.set("thinking_preview_lines", "10").unwrap();
        settings.set("help_expand_groups", "true").unwrap();
        settings.set("pin_last_prompt", "false").unwrap();
        assert_eq!(settings.thinking_preview_lines, 10);
        assert!(settings.help_expand_groups);
        assert!(!settings.pin_last_prompt);

        let restored: Settings =
            toml::from_str(&toml::to_string(&settings).expect("serialize settings"))
                .expect("restore settings");
        assert_eq!(restored.thinking_preview_lines, 10);
        assert!(restored.help_expand_groups);
        assert!(!restored.pin_last_prompt);
    }

    /// Explicit animated baseline for env-force tests (#4095 flipped defaults to calm).
    fn animated_settings() -> Settings {
        Settings {
            calm_mode: false,
            low_motion: false,
            load_error: None,
            fancy_animations: true,
            show_tool_details: true,
            transcript_spacing: "spacious".to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn apply_preset_calm_sets_bundle_and_preserves_evidence() {
        let mut settings = Settings::default();
        // Density is calm by default; motion is an independent axis.
        assert!(settings.calm_mode);
        assert!(!settings.show_thinking);

        let changed = settings.apply_preset("CALM").expect("calm preset applies");
        assert_eq!(
            changed,
            CALM_PRESET_FIELDS
                .iter()
                .map(|(k, _)| *k)
                .collect::<Vec<_>>()
        );

        assert!(settings.calm_mode);
        assert_eq!(settings.tool_collapse_mode, "calm");
        assert_eq!(settings.transcript_spacing, "compact");
        assert!(settings.low_motion);
        assert!(!settings.fancy_animations);
        assert!(!settings.show_tool_details);
        // Calm does not override the user's reasoning preference.
        assert!(!settings.show_thinking);
    }

    #[test]
    fn default_settings_use_spacious_transcript_spacing() {
        let settings = Settings::default();
        assert!(settings.calm_mode);
        assert!(!settings.show_tool_details);
        assert!(!settings.low_motion);
        assert!(settings.fancy_animations);
        assert_eq!(settings.transcript_spacing, "spacious");
        assert_eq!(settings.tool_collapse_mode, "compact");
        // Thinking is opt-in so the transcript stays focused on the chat.
        assert!(!settings.show_thinking);
    }

    #[test]
    fn behavioral_tip_impressions_are_backward_compatible_and_persist_when_seen() {
        let default_body = toml::to_string_pretty(&Settings::default()).expect("serialize");
        assert!(!default_body.contains("behavioral_tip_impressions"));

        let mut settings = Settings::default();
        settings
            .behavioral_tip_impressions
            .insert("planning_mode".to_string(), 1);
        let body = toml::to_string_pretty(&settings).expect("serialize");
        let restored: Settings = toml::from_str(&body).expect("restore settings");
        assert_eq!(
            restored
                .behavioral_tip_impressions
                .get("planning_mode")
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn contextual_tips_default_on_and_round_trip_opt_out() {
        let old: Settings = toml::from_str("").unwrap();
        assert!(old.contextual_tips);
        let mut settings = old;
        settings.set("contextual_tips", "off").unwrap();
        let restored: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(!restored.contextual_tips);
    }

    #[test]
    fn settings_save_preserves_malformed_document_instead_of_fallback_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.toml");
        let malformed = "theme = [private_fixture_payload\n";
        std::fs::write(&path, malformed).unwrap();
        let mut settings =
            Settings::load_persisted_from_candidates(Some(path.clone()), None, None).unwrap();
        assert!(settings.load_error.is_some());
        // Impression writers use this same save boundary as the opt-out.
        settings
            .behavioral_tip_impressions
            .insert("planning_mode".into(), 1);
        let error = settings.save_to_path(&path).unwrap_err().to_string();
        assert_eq!(std::fs::read_to_string(path).unwrap(), malformed);
        assert!(!error.contains("private_fixture_payload"));
    }

    #[test]
    fn plugin_dismissals_are_additive_and_omitted_until_used() {
        let old = toml::to_string_pretty(&Settings::default()).unwrap();
        assert!(!old.contains("dismissed_plugin_suggestions"));
        let mut settings: Settings = toml::from_str(&old).unwrap();
        assert!(settings.dismissed_plugin_suggestions.is_empty());
        settings
            .dismissed_plugin_suggestions
            .insert("supabase".into());
        let encoded = toml::to_string_pretty(&settings).unwrap();
        let decoded: Settings = toml::from_str(&encoded).unwrap();
        assert!(decoded.dismissed_plugin_suggestions.contains("supabase"));
    }

    #[test]
    fn footer_hint_uses_are_backward_compatible_and_persist_when_recorded() {
        let default_body = toml::to_string_pretty(&Settings::default()).expect("serialize");
        assert!(!default_body.contains("footer_hint_uses"));

        let mut settings = Settings::default();
        settings
            .footer_hint_uses
            .insert("permission_cycle".to_string(), 2);
        let body = toml::to_string_pretty(&settings).expect("serialize");
        let restored: Settings = toml::from_str(&body).expect("restore settings");
        assert_eq!(
            restored.footer_hint_uses.get("permission_cycle").copied(),
            Some(2)
        );
    }

    #[test]
    fn apply_preset_rejects_unknown_name() {
        let mut settings = Settings::default();
        let err = settings.apply_preset("turbo").expect_err("unknown preset");
        assert!(err.to_string().contains("Unknown preset"));
        assert!(preset_fields("calm").is_some());
        assert!(preset_fields("turbo").is_none());
    }

    #[test]
    fn default_settings_keep_auto_compact_as_unset_fallback() {
        let settings = Settings::default();
        // The persisted fallback remains false so a missing settings file does
        // not look like an explicit user preference. Startup resolves the
        // runtime default from the active model window unless the file contains
        // `auto_compact`.
        assert!(!settings.auto_compact);
        assert_eq!(settings.auto_compact_threshold_percent, 80.0);
        assert!(!settings.auto_compact_explicit);
    }

    #[test]
    fn auto_compact_remains_explicitly_configurable() {
        let mut settings = Settings::default();
        settings.set("auto_compact", "on").expect("enable");
        assert!(settings.auto_compact);
        assert!(settings.auto_compact_explicit);
        settings.set("auto_compact", "off").expect("disable");
        assert!(!settings.auto_compact);
    }

    #[test]
    fn unrelated_save_does_not_materialize_implicit_auto_compact_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        let settings = Settings {
            calm_mode: false,
            ..Settings::default()
        };

        settings.save_to_path(&path).expect("save settings");

        let body = std::fs::read_to_string(&path).expect("read settings");
        let document = toml::from_str::<toml::Value>(&body).expect("parse settings");
        assert!(!auto_compact_explicitly_configured_in_document(&document));
        let reloaded = Settings::load_persisted_from_candidates(Some(path), None, None)
            .expect("reload settings");
        assert!(!reloaded.auto_compact_explicit);
        assert!(!reloaded.auto_compact);
        assert!(!reloaded.calm_mode);
    }

    #[test]
    fn explicit_auto_compact_off_survives_save_and_reload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        let mut settings = Settings::default();
        settings.set("auto_compact", "off").expect("disable");

        settings.save_to_path(&path).expect("save settings");

        assert!(auto_compact_explicitly_configured_from_candidates((
            Some(path.clone()),
            None,
            None,
        )));
        let reloaded = Settings::load_persisted_from_candidates(Some(path), None, None)
            .expect("reload settings");
        assert!(reloaded.auto_compact_explicit);
        assert!(!reloaded.auto_compact);
    }

    #[test]
    fn auto_compact_threshold_is_validated() {
        let mut settings = Settings::default();
        settings
            .set("auto_compact_threshold", "65%")
            .expect("threshold");
        assert!(settings.auto_compact, "a threshold expresses enable intent");
        assert_eq!(settings.auto_compact_threshold_percent, 65.0);
        assert!(settings.auto_compact_explicit);
        assert!(settings.set("auto_compact_threshold", "9").is_err());
        assert!(settings.set("auto_compact_threshold", "101").is_err());
    }

    #[test]
    fn threshold_only_persisted_config_enables_auto_compaction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "auto_compact_threshold_percent = 65\n").expect("settings");

        let loaded = Settings::load_persisted_from_candidates(Some(path.clone()), None, None)
            .expect("load threshold-only settings");

        assert!(loaded.auto_compact);
        assert!(loaded.auto_compact_explicit);
        assert_eq!(loaded.auto_compact_threshold_percent, 65.0);
        assert!(auto_compact_explicitly_configured_from_candidates((
            Some(path),
            None,
            None,
        )));
    }

    #[test]
    fn explicit_auto_compact_off_overrides_a_persisted_threshold() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        std::fs::write(
            &path,
            "auto_compact = false\nauto_compact_threshold_percent = 65\n",
        )
        .expect("settings");

        let loaded = Settings::load_persisted_from_candidates(Some(path.clone()), None, None)
            .expect("load explicit opt-out");

        assert!(!loaded.auto_compact);
        assert!(loaded.auto_compact_explicit);
        assert!(auto_compact_explicitly_configured_from_candidates((
            Some(path),
            None,
            None,
        )));
    }

    #[test]
    fn default_settings_show_footer_water_strip() {
        let settings = Settings::default();
        assert!(
            settings.fancy_animations,
            "underwater presentation is the default"
        );
        assert!(!settings.low_motion);
        assert_eq!(settings.transcript_spacing, "spacious");
    }

    #[test]
    fn retired_launch_screen_setting_is_accepted_and_dropped_on_save() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "launch_screen = false\n").expect("legacy settings");

        let settings = Settings::load_persisted_from_candidates(Some(path.clone()), None, None)
            .expect("legacy setting must remain readable");
        settings
            .save_to_path(&path)
            .expect("save normalized settings");

        let saved = std::fs::read_to_string(&path).expect("read normalized settings");
        assert!(
            !saved.contains("launch_screen"),
            "the retired setting must not be written back: {saved}"
        );
    }

    #[test]
    fn legacy_sidebar_focus_migrates_to_rail_panel_and_placement() {
        let migrate = |focus: &str| {
            let mut settings = Settings {
                sidebar_focus: focus.to_string(),
                ..Settings::default()
            };
            migrate_sidebar_settings_to_rail(&mut settings);
            settings
        };

        assert_eq!(migrate("agents").rail_panel, "agents");
        assert_eq!(migrate("subagents").rail_panel, "agents");
        assert_eq!(migrate("context").rail_panel, "context");
        assert_eq!(migrate("session").rail_panel, "context");
        assert_eq!(migrate("tasks").rail_panel, "tasks");
        assert_eq!(migrate("activity").rail_panel, "tasks");
        assert_eq!(migrate("pinned").rail_panel, "pinned");
        assert_eq!(migrate("work").rail_panel, "pinned");
        // `auto` is the shipped default for `sidebar_focus`, so this arm is
        // the effective default for every upgrading user — it must land on
        // the panel that hides itself when there is nothing to show, not on
        // the always-on pinned strip.
        assert_eq!(migrate("auto").rail_panel, "tasks");
        // A hidden sidebar becomes rail placement off.
        let hidden = migrate("hidden");
        assert_eq!(hidden.work_surface_placement, "off");
        // #5141's pinned sessions panel carries forward as the first-class
        // sessions rail.
        assert!(migrate("sessions").sessions_rail);
        assert!(migrate("sessions_rail").sessions_rail);
        // An explicit `rail_panel = "tasks"` in the document wins over the
        // auto→pinned migration even though "tasks" is the default value.
        let mut explicit = Settings {
            sidebar_focus: "auto".to_string(),
            rail_panel: "tasks".to_string(),
            rail_panel_explicit: true,
            ..Settings::default()
        };
        migrate_sidebar_settings_to_rail(&mut explicit);
        assert_eq!(explicit.rail_panel, "tasks");
        // Placement panels keep their placement when the rail hides.
        let mut bottom = Settings {
            sidebar_focus: "hidden".to_string(),
            work_surface_placement: "bottom".to_string(),
            work_surface_placement_explicit: true,
            ..Settings::default()
        };
        migrate_sidebar_settings_to_rail(&mut bottom);
        // Bottom is a valid explicit placement now, so migration keeps it.
        assert_eq!(bottom.work_surface_placement, "bottom");
    }

    #[test]
    fn legacy_sidebar_width_maps_to_side_columns_and_new_keys_win() {
        let mut settings = Settings {
            sidebar_width_percent: 40,
            ..Settings::default()
        };
        migrate_sidebar_settings_to_rail(&mut settings);
        assert_eq!(settings.work_surface_side_width, 48);

        // The default percent leaves the default side width alone.
        let mut settings = Settings::default();
        migrate_sidebar_settings_to_rail(&mut settings);
        assert_eq!(settings.work_surface_side_width, 30);

        // An explicit rail panel wins over the migrated sidebar focus.
        let mut settings = Settings {
            sidebar_focus: "context".to_string(),
            rail_panel: "agents".to_string(),
            ..Settings::default()
        };
        migrate_sidebar_settings_to_rail(&mut settings);
        assert_eq!(settings.rail_panel, "agents");
    }

    #[test]
    fn reasoning_effort_setting_normalizes_and_clears() {
        let mut settings = Settings::default();
        // `xhigh` and `ultra` are their own rungs since the thinking ladder,
        // so normalizing collapses spellings *within* a tier instead of
        // folding the top three tiers into `max`.
        for (input, stored) in [
            ("xhigh", "xhigh"),
            ("ultracode", "ultra"),
            ("maximum", "max"),
            // Slice 4, D3: `minimal` is a real rung with its own spelling, so it
            // round-trips instead of being folded onto `low`.
            ("minimal", "minimal"),
            ("minimum", "low"),
            ("light", "low"),
        ] {
            settings
                .set("reasoning_effort", input)
                .unwrap_or_else(|error| panic!("normalize {input}: {error}"));
            assert_eq!(settings.reasoning_effort.as_deref(), Some(stored));
        }
        settings
            .set("reasoning_effort", "default")
            .expect("clear effort");
        assert!(settings.reasoning_effort.is_none());
    }

    #[test]
    fn paste_burst_detection_is_configurable_independent_of_bracketed_paste() {
        let mut settings = Settings::default();
        assert!(settings.bracketed_paste);
        assert!(settings.paste_burst_detection);

        settings
            .set("paste_burst_detection", "off")
            .expect("disable paste burst fallback");
        assert!(settings.bracketed_paste);
        assert!(!settings.paste_burst_detection);

        settings
            .set("bracketed_paste", "off")
            .expect("disable bracketed paste");
        assert!(!settings.bracketed_paste);
        assert!(!settings.paste_burst_detection);
    }

    #[test]
    fn mention_completion_caps_are_configurable() {
        let mut settings = Settings::default();
        assert_eq!(settings.mention_menu_limit, 128);
        assert_eq!(settings.mention_walk_depth, 10);
        assert_eq!(settings.mention_menu_behavior, "fuzzy");
        settings
            .set("mention_menu_limit", "256")
            .expect("set mention menu limit");
        settings
            .set("mention_walk_depth", "0")
            .expect("allow unlimited walk depth");
        settings
            .set("mention_menu_behavior", "browser")
            .expect("set mention menu behavior");

        assert_eq!(settings.mention_menu_limit, 256);
        assert_eq!(settings.mention_walk_depth, 0);
        assert_eq!(settings.mention_menu_behavior, "browser");

        let err = settings
            .set("mention_walk_depth", "deep")
            .expect_err("non-numeric depth should fail");
        assert!(err.to_string().contains("invalid mention_walk_depth"));

        let err = settings
            .set("mention_menu_behavior", "random")
            .expect_err("unknown mention behavior should fail");
        assert!(err.to_string().contains("invalid mention_menu_behavior"));
    }

    #[test]
    fn locale_normalizes_supported_values_and_rejects_unknowns() {
        let mut settings = Settings::default();
        for (input, expected) in [
            ("ja_JP.UTF-8", "ja"),
            ("zh-CN", "zh-Hans"),
            ("zh-TW", "zh-Hant"),
            ("zh-Hant", "zh-Hant"),
            ("es-MX", "es-419"),
            ("vi_VN.UTF-8", "vi"),
            ("ko-KR", "ko"),
            ("ca-ES", "ca"),
            ("de_DE.UTF-8", "de"),
            ("fr-FR", "fr"),
            ("id-ID", "id"),
            ("hi_IN.UTF-8", "hi"),
            ("ru-RU", "ru"),
            ("uk_UA.UTF-8", "uk"),
        ] {
            settings
                .set("locale", input)
                .unwrap_or_else(|err| panic!("set locale {input}: {err}"));
            assert_eq!(settings.locale, expected);
        }

        settings.set("language", "pt-PT").expect("set pt fallback");
        assert_eq!(settings.locale, "pt-BR");

        let err = settings
            .set("locale", "ar")
            .expect_err("Arabic is planned, not shipped");
        assert!(err.to_string().contains("invalid locale"));
    }

    #[test]
    fn theme_normalizes_supported_values_and_rejects_unknowns() {
        let mut settings = Settings::default();
        assert_eq!(settings.theme, "underwater");

        settings
            .set("theme", "charcoal")
            .expect("set charcoal alternative");
        assert_eq!(settings.theme, "shoreline");

        settings.set("theme", "grayscale").expect("set grayscale");
        assert_eq!(settings.theme, "grayscale");

        settings.set("ui_theme", "black-white").expect("set alias");
        assert_eq!(settings.theme, "grayscale");

        settings.set("theme", "whale").expect("set dark alias");
        assert_eq!(settings.theme, "dark");

        settings
            .set("theme", "tokyonight")
            .expect("set community theme alias");
        assert_eq!(settings.theme, "tokyo-night");

        settings
            .set("theme", "solarized")
            .expect("set solarized alias");
        assert_eq!(settings.theme, "solarized-light");

        settings
            .set("theme", "custom:Ocean_1")
            .expect("custom selector validation must not depend on the file system");
        assert_eq!(settings.theme, "custom:ocean_1");

        let err = settings
            .set("theme", "nord")
            .expect_err("unknown theme should fail");
        assert!(err.to_string().contains("invalid theme"));
    }

    #[test]
    fn background_color_normalizes_hex_and_accepts_default() {
        let mut settings = Settings::default();
        settings
            .set("background_color", "#1A1b26")
            .expect("set custom background");
        assert_eq!(settings.background_color.as_deref(), Some("#1a1b26"));

        settings
            .set("background", "default")
            .expect("reset custom background");
        assert_eq!(settings.background_color, None);
    }

    #[test]
    fn background_color_rejects_invalid_hex() {
        let mut settings = Settings::default();
        let err = settings
            .set("background_color", "#123")
            .expect_err("short hex should fail");
        assert!(err.to_string().contains("invalid background_color"));
    }

    #[test]
    fn cost_currency_normalizes_yuan_aliases_and_rejects_unknowns() {
        let mut settings = Settings::default();
        assert_eq!(settings.cost_currency, "usd");

        settings.set("cost_currency", "yuan").expect("set yuan");
        assert_eq!(settings.cost_currency, "cny");

        settings.set("currency", "rmb").expect("set rmb");
        assert_eq!(settings.cost_currency, "cny");

        let err = settings
            .set("cost_currency", "eur")
            .expect_err("unsupported currency");
        assert!(err.to_string().contains("invalid cost currency"));
    }

    #[test]
    fn context_panel_is_configurable() {
        let mut settings = Settings::default();
        assert!(!settings.context_panel);

        settings
            .set("context_panel", "on")
            .expect("enable context panel");
        assert!(settings.context_panel);

        settings
            .set("session_panel", "off")
            .expect("disable context panel via alias");
        assert!(!settings.context_panel);
    }

    #[test]
    fn tool_collapse_mode_is_configurable() {
        let mut settings = Settings::default();
        assert_eq!(settings.tool_collapse_mode, "compact");

        settings
            .set("tool_collapse", "expanded")
            .expect("expanded mode");
        assert_eq!(settings.tool_collapse_mode, "expanded");

        settings.set("collapse", "calm-only").expect("calm alias");
        assert_eq!(settings.tool_collapse_mode, "calm");

        settings.set("collapse", "off").expect("off alias");
        assert_eq!(settings.tool_collapse_mode, "expanded");

        // Issue #3256 proposes `collapsed` as the default verbosity name;
        // accept it (and the bare verb) as an alias of the canonical `compact`.
        settings
            .set("tool_collapse", "collapsed")
            .expect("collapsed alias");
        assert_eq!(settings.tool_collapse_mode, "compact");
        settings.set("tool_collapse", "expanded").expect("reset");
        settings
            .set("tool_collapse", "collapse")
            .expect("collapse alias");
        assert_eq!(settings.tool_collapse_mode, "compact");

        let err = settings
            .set("tool_collapse", "mystery")
            .expect_err("invalid collapse mode");
        assert!(err.to_string().contains("invalid tool collapse mode"));
    }

    #[test]
    fn tool_collapse_threshold_is_not_a_settings_key() {
        // #3256: rollup min-run size stays a fixed runtime constant (3), not a
        // user setting — reject any accidental /set surface for it.
        let mut settings = Settings::default();
        let err = settings
            .set("tool_collapse_threshold", "5")
            .expect_err("threshold must not be configurable");
        assert!(
            err.to_string().contains("Unknown setting")
                || err.to_string().contains("unknown setting")
                || err.to_string().contains("Failed to update"),
            "unexpected error: {err}"
        );
        assert_eq!(settings.tool_collapse_mode, "compact");
        assert!(!settings.show_tool_details);
    }

    #[test]
    fn display_localizes_header_and_config_file_label() {
        let settings = Settings::default();
        let en = settings.display(codewhale_localization::Locale::En);
        assert!(en.contains("Settings:"), "english header missing:\n{en}");
        assert!(
            en.contains("Config file:"),
            "english config label missing:\n{en}"
        );

        let zh = settings.display(codewhale_localization::Locale::ZhHans);
        assert!(zh.contains("设置"), "chinese header missing:\n{zh}");
        assert!(
            zh.contains("配置文件"),
            "chinese config label missing:\n{zh}"
        );
    }

    #[test]
    fn display_does_not_present_archived_route_preferences_as_current_defaults() {
        let settings = Settings {
            default_provider: Some("zai".to_string()),
            default_model: Some("deepseek-v4-pro".to_string()),
            provider_models: Some(std::collections::HashMap::from([
                ("zai".to_string(), "GLM-5.2".to_string()),
                ("deepseek".to_string(), "deepseek-v4-flash".to_string()),
            ])),
            ..Settings::default()
        };

        let display = settings.display(codewhale_localization::Locale::En);

        assert!(display.contains("model defaults:     config.toml (use /config)"));
        for archived in [
            "deepseek_fallback:",
            "default_provider:",
            "provider_models:",
            "default_model:",
            "GLM-5.2",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
        ] {
            assert!(
                !display.contains(archived),
                "archived value shown as current: {display}"
            );
        }
    }

    #[test]
    fn archived_model_preferences_survive_serialization_but_reject_new_settings_writes() {
        let mut settings: Settings = toml::from_str(
            "default_provider = 'zai'\ndefault_model = 'deepseek-v4-pro'\n[provider_models]\nzai = 'GLM-5.3'\n",
        ).expect("legacy preferences");
        let before = toml::to_string(&settings).expect("legacy snapshot");

        for key in ["model", "default_model"] {
            let error = settings
                .set(key, "deepseek-v4-flash")
                .expect_err("canonical config owns models");
            assert!(error.to_string().contains("/config model"));
        }

        assert_eq!(
            toml::to_string(&settings).expect("unchanged legacy snapshot"),
            before
        );
        let restored: Settings = toml::from_str(&before).expect("preserved migration inputs");
        assert_eq!(restored.default_provider.as_deref(), Some("zai"));
        assert_eq!(restored.default_model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(
            restored
                .provider_models
                .as_ref()
                .and_then(|models| models.get("zai"))
                .map(String::as_str),
            Some("GLM-5.3")
        );
    }

    #[test]
    fn legacy_enabled_models_table_still_loads_and_round_trips() {
        let settings: Settings = toml::from_str(
            r#"
[enabled_models]
zai = ["GLM-5.2", "GLM-5.3"]
"#,
        )
        .expect("legacy enabled_models loads");
        assert!(settings.provider_models.is_none());
        assert_eq!(
            settings
                .enabled_models
                .as_ref()
                .and_then(|models| models.get("zai")),
            Some(&vec!["GLM-5.2".to_string(), "GLM-5.3".to_string()])
        );
        let encoded = toml::to_string(&settings).expect("serialize enabled models");
        let decoded: Settings = toml::from_str(&encoded).expect("deserialize enabled models");
        assert_eq!(decoded.enabled_models, settings.enabled_models);
    }

    /// Tests that mutate process-global `NO_ANIMATIONS` serialise
    /// through this guard so the cargo parallel runner doesn't
    /// observe interleaved overrides. Uses the process-wide test env
    /// lock so this serializes with the TERM_PROGRAM tests too —
    /// otherwise a `NO_ANIMATIONS=1` leak from this test family can
    /// flip a concurrent `TERM_PROGRAM=iTerm` test's `low_motion`
    /// assertion through the shared `apply_env_overrides` path.
    fn no_animations_test_guard() -> crate::test_support::TestEnvLock {
        crate::test_support::lock_test_env()
    }

    #[test]
    fn no_animations_env_forces_low_motion_on() {
        let _g = no_animations_test_guard();
        // SAFETY: tests in this group serialise through the guard.
        unsafe {
            std::env::set_var("NO_ANIMATIONS", "1");
        }
        let mut settings = animated_settings();
        assert!(!settings.low_motion, "default is animated");
        assert!(settings.fancy_animations, "default shows the water strip");
        settings.apply_env_overrides();
        assert!(settings.low_motion, "NO_ANIMATIONS=1 forces low_motion");
        assert!(
            !settings.fancy_animations,
            "NO_ANIMATIONS=1 keeps fancy off"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("NO_ANIMATIONS");
        }
    }

    #[test]
    fn no_animations_env_overrides_user_opt_in() {
        let _g = no_animations_test_guard();
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("NO_ANIMATIONS", "true");
        }
        // User had explicitly opted into fancy animations on disk.
        let mut settings = Settings {
            fancy_animations: true,
            ..Settings::default()
        };
        settings.apply_env_overrides();
        assert!(
            !settings.fancy_animations,
            "platform NO_ANIMATIONS overrides user-opt-in fancy_animations"
        );
        assert!(settings.low_motion);
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("NO_ANIMATIONS");
        }
    }

    #[test]
    fn no_animations_env_recognises_truthy_spellings_only() {
        let _g = no_animations_test_guard();
        let prev_wt_session = std::env::var_os("WT_SESSION");
        let prev_tmux = std::env::var_os("TMUX");
        let prev_sty = std::env::var_os("STY");
        let prev_term_program = std::env::var_os("TERM_PROGRAM");
        let prev_term = std::env::var_os("TERM");
        let prev_ssh_client = std::env::var_os("SSH_CLIENT");
        let prev_ssh_tty = std::env::var_os("SSH_TTY");
        let prev_tilix_id = std::env::var_os("TILIX_ID");
        let prev_terminator_uuid = std::env::var_os("TERMINATOR_UUID");

        // The test is about NO_ANIMATIONS only. On Windows CI, an unmarked
        // console host now independently enables low_motion, so mark the host
        // as non-legacy while checking falsy spellings.
        // Clear multiplexer markers for the same reason: they also force
        // low_motion independently of NO_ANIMATIONS.
        // Clear TERM_PROGRAM, SSH, and other terminal-specific variables as they
        // also force low_motion independently of NO_ANIMATIONS.
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::remove_var("TMUX");
            std::env::remove_var("STY");
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("TERM");
            std::env::remove_var("SSH_CLIENT");
            std::env::remove_var("SSH_TTY");
            std::env::remove_var("TILIX_ID");
            std::env::remove_var("TERMINATOR_UUID");
        }
        #[cfg(windows)]
        unsafe {
            std::env::set_var("WT_SESSION", "test");
        }
        for truthy in ["1", "true", "True", "YES", "on"] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("NO_ANIMATIONS", truthy);
            }
            let mut s = animated_settings();
            s.apply_env_overrides();
            assert!(s.low_motion, "{truthy:?} should be truthy");
        }
        for falsy in ["0", "false", "no", "off", ""] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("NO_ANIMATIONS", falsy);
            }
            let mut s = animated_settings();
            s.apply_env_overrides();
            assert!(!s.low_motion, "{falsy:?} should be falsy");
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("NO_ANIMATIONS");
            match prev_wt_session {
                Some(v) => std::env::set_var("WT_SESSION", v),
                None => std::env::remove_var("WT_SESSION"),
            }
            match prev_tmux {
                Some(v) => std::env::set_var("TMUX", v),
                None => std::env::remove_var("TMUX"),
            }
            match prev_sty {
                Some(v) => std::env::set_var("STY", v),
                None => std::env::remove_var("STY"),
            }
            match prev_term_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
            match prev_ssh_client {
                Some(v) => std::env::set_var("SSH_CLIENT", v),
                None => std::env::remove_var("SSH_CLIENT"),
            }
            match prev_ssh_tty {
                Some(v) => std::env::set_var("SSH_TTY", v),
                None => std::env::remove_var("SSH_TTY"),
            }
            match prev_tilix_id {
                Some(v) => std::env::set_var("TILIX_ID", v),
                None => std::env::remove_var("TILIX_ID"),
            }
            match prev_terminator_uuid {
                Some(v) => std::env::set_var("TERMINATOR_UUID", v),
                None => std::env::remove_var("TERMINATOR_UUID"),
            }
        }
    }

    /// Serialise tests that mutate `TERM_PROGRAM` through this guard.
    /// Uses the process-wide test env lock so this serializes not just
    /// with itself but with every other env-mutating test in the suite
    /// — otherwise a concurrent test that calls `animated_settings()`
    /// can read whatever value our two `set_var`s have raced into the
    /// env at that instant.
    fn term_program_test_guard() -> crate::test_support::TestEnvLock {
        crate::test_support::lock_test_env()
    }

    #[test]
    fn vscode_uses_calm_rendering_without_changing_text_cadence() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "vscode");
        }
        let mut settings = animated_settings();
        assert!(!settings.low_motion, "default is animated");
        settings.apply_env_overrides();
        assert!(
            settings.low_motion,
            "TERM_PROGRAM=vscode must disable decorative motion"
        );
        assert!(!settings.fancy_animations);
        assert!(
            settings.constrained_frame_rate,
            "TERM_PROGRAM=vscode should cap redraws without changing animation semantics"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn ghostty_term_program_keeps_full_motion_without_the_legacy_30_fps_cap() {
        let _g = term_program_test_guard();
        let _tmux = crate::test_support::EnvVarGuard::remove("TMUX");
        let _sty = crate::test_support::EnvVarGuard::remove("STY");
        let _tilix_id = crate::test_support::EnvVarGuard::remove("TILIX_ID");
        let _terminator_uuid = crate::test_support::EnvVarGuard::remove("TERMINATOR_UUID");
        let _no_animations = crate::test_support::EnvVarGuard::remove("NO_ANIMATIONS");
        // Neutralize the SSH markers: production intentionally caps motion
        // over SSH, and the suite routinely runs inside one.
        let _ssh_client = crate::test_support::EnvVarGuard::remove("SSH_CLIENT");
        let _ssh_connection = crate::test_support::EnvVarGuard::remove("SSH_CONNECTION");
        let _ssh_tty = crate::test_support::EnvVarGuard::remove("SSH_TTY");
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "Ghostty");
        }
        let mut settings = animated_settings();
        assert!(!settings.low_motion, "default is animated");
        settings.apply_env_overrides();
        assert!(!settings.low_motion);
        assert!(settings.fancy_animations);
        assert!(!settings.constrained_frame_rate);
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn ghostty_term_fallback_keeps_full_motion_without_the_legacy_30_fps_cap() {
        let _g = term_program_test_guard();
        let _tmux = crate::test_support::EnvVarGuard::remove("TMUX");
        let _sty = crate::test_support::EnvVarGuard::remove("STY");
        let _tilix_id = crate::test_support::EnvVarGuard::remove("TILIX_ID");
        let _terminator_uuid = crate::test_support::EnvVarGuard::remove("TERMINATOR_UUID");
        let _no_animations = crate::test_support::EnvVarGuard::remove("NO_ANIMATIONS");
        // Neutralize the SSH markers: production intentionally caps motion
        // over SSH, and the suite routinely runs inside one.
        let _ssh_client = crate::test_support::EnvVarGuard::remove("SSH_CLIENT");
        let _ssh_connection = crate::test_support::EnvVarGuard::remove("SSH_CONNECTION");
        let _ssh_tty = crate::test_support::EnvVarGuard::remove("SSH_TTY");
        let prev_program = std::env::var_os("TERM_PROGRAM");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("TERM", "xterm-ghostty");
        }
        let mut settings = Settings::default();
        settings.apply_env_overrides();
        assert!(!settings.low_motion);
        assert!(settings.fancy_animations);
        assert!(!settings.constrained_frame_rate);
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
    }

    #[test]
    fn non_vscode_term_program_does_not_force_low_motion() {
        let _g = term_program_test_guard();
        let _no_animations = crate::test_support::EnvVarGuard::remove("NO_ANIMATIONS");
        let prev = std::env::var_os("TERM_PROGRAM");
        let prev_term = std::env::var_os("TERM");
        let prev_ssh_client = std::env::var_os("SSH_CLIENT");
        let prev_ssh_tty = std::env::var_os("SSH_TTY");
        let prev_tilix_id = std::env::var_os("TILIX_ID");
        let prev_terminator_uuid = std::env::var_os("TERMINATOR_UUID");
        let prev_tmux = std::env::var_os("TMUX");
        let prev_sty = std::env::var_os("STY");
        // SAFETY: serialised by the guard. Clear SSH_* so a real
        // SSH session running the test suite doesn't make this
        // assertion trivially fail — the SSH path is exercised
        // separately by `ssh_session_forces_low_motion_on`.
        unsafe {
            std::env::remove_var("SSH_CLIENT");
            std::env::remove_var("SSH_TTY");
            std::env::remove_var("TERM");
            std::env::remove_var("TILIX_ID");
            std::env::remove_var("TERMINATOR_UUID");
            std::env::remove_var("TMUX");
            std::env::remove_var("STY");
        }
        for program in ["iTerm.app", "Apple_Terminal", "WezTerm", "xterm-256color"] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("TERM_PROGRAM", program);
            }
            let mut s = animated_settings();
            s.apply_env_overrides();
            assert!(
                !s.low_motion,
                "TERM_PROGRAM={program:?} should not force low_motion"
            );
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
            if let Some(v) = prev_ssh_client {
                std::env::set_var("SSH_CLIENT", v);
            }
            if let Some(v) = prev_ssh_tty {
                std::env::set_var("SSH_TTY", v);
            }
            if let Some(v) = prev_tilix_id {
                std::env::set_var("TILIX_ID", v);
            }
            if let Some(v) = prev_terminator_uuid {
                std::env::set_var("TERMINATOR_UUID", v);
            }
            if let Some(v) = prev_tmux {
                std::env::set_var("TMUX", v);
            }
            if let Some(v) = prev_sty {
                std::env::set_var("STY", v);
            }
        }
    }

    #[test]
    fn tilix_and_terminator_cap_redraws_without_disabling_motion() {
        let _g = term_program_test_guard();
        let _no_animations = crate::test_support::EnvVarGuard::remove("NO_ANIMATIONS");
        // Neutralize the SSH markers: production intentionally caps motion
        // over SSH, and the suite routinely runs inside one.
        let _ssh_client = crate::test_support::EnvVarGuard::remove("SSH_CLIENT");
        let _ssh_connection = crate::test_support::EnvVarGuard::remove("SSH_CONNECTION");
        let _ssh_tty = crate::test_support::EnvVarGuard::remove("SSH_TTY");
        let prev_term_program = std::env::var_os("TERM_PROGRAM");
        let prev_tilix_id = std::env::var_os("TILIX_ID");
        let prev_terminator_uuid = std::env::var_os("TERMINATOR_UUID");
        let prev_wt_session = std::env::var_os("WT_SESSION");

        for (var, val) in [
            ("TILIX_ID", "d5b5b5d6-tilix-session"),
            ("TERMINATOR_UUID", "urn:uuid:terminator-session"),
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::remove_var("TERM_PROGRAM");
                std::env::remove_var("TILIX_ID");
                std::env::remove_var("TERMINATOR_UUID");
                std::env::set_var(var, val);
                // A native Windows test process without any modern-terminal
                // marker is intentionally treated as legacy ConHost. This
                // test isolates the VTE signal instead, so keep that separate
                // platform heuristic from changing its motion assertions.
                #[cfg(windows)]
                std::env::set_var("WT_SESSION", "codewhale-test");
            }
            let mut settings = animated_settings();
            assert!(!settings.low_motion, "default is animated");
            settings.apply_env_overrides();
            assert!(
                settings.constrained_frame_rate,
                "{var} must cap redraws to prevent VTE flicker (#1470)"
            );
            assert!(
                !settings.low_motion,
                "{var} must not change motion semantics"
            );
            assert!(
                settings.fancy_animations,
                "{var} must not disable the ocean treatment"
            );
        }

        // SAFETY: cleanup under the guard.
        unsafe {
            match prev_term_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_tilix_id {
                Some(v) => std::env::set_var("TILIX_ID", v),
                None => std::env::remove_var("TILIX_ID"),
            }
            match prev_terminator_uuid {
                Some(v) => std::env::set_var("TERMINATOR_UUID", v),
                None => std::env::remove_var("TERMINATOR_UUID"),
            }
            match prev_wt_session {
                Some(v) => std::env::set_var("WT_SESSION", v),
                None => std::env::remove_var("WT_SESSION"),
            }
        }
    }

    #[test]
    fn termius_term_program_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "Termius");
        }
        let mut settings = animated_settings();
        assert!(!settings.low_motion, "default is animated");
        settings.apply_env_overrides();
        assert!(
            settings.low_motion,
            "TERM_PROGRAM=Termius must enable low_motion to prevent flickering (#1433)"
        );
        assert!(
            !settings.fancy_animations,
            "TERM_PROGRAM=Termius must disable fancy_animations"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn legacy_windows_console_host_detects_unmarked_shell() {
        assert!(legacy_windows_console_host_env([
            None, None, None, None, None, None, None, None
        ]));
    }

    #[test]
    fn legacy_windows_console_host_excludes_modern_terminal_markers() {
        use std::ffi::OsStr;

        let marker = Some(OsStr::new("1"));
        assert!(!legacy_windows_console_host_env([
            marker, None, None, None, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, marker, None, None, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, marker, None, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, marker, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, marker, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, None, marker, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, None, None, marker, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, None, None, None, marker
        ]));
    }

    #[cfg(windows)]
    #[test]
    fn unmarked_windows_console_forces_calm_rendering() {
        let _g = term_program_test_guard();
        let vars = [
            "WT_SESSION",
            "ConEmuPID",
            "TERM_PROGRAM",
            "WEZTERM_EXECUTABLE",
            "WEZTERM_PANE",
            "ALACRITTY_WINDOW_ID",
            "ANSICON",
            "TERM",
            "SSH_CLIENT",
            "SSH_TTY",
            "NO_ANIMATIONS",
            "PTYXIS_VERSION",
        ];
        let prev: Vec<_> = vars
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();

        // SAFETY: serialised by the guard.
        unsafe {
            for name in vars {
                std::env::remove_var(name);
            }
        }

        let mut settings = animated_settings();
        assert!(!settings.low_motion, "default is animated");
        assert!(settings.fancy_animations, "default shows the water strip");
        assert_eq!(settings.synchronized_output, "auto");
        settings.apply_env_overrides();
        assert!(settings.low_motion);
        assert!(!settings.fancy_animations);
        assert!(
            settings.bracketed_paste,
            "env-only conhost fallback must not persistently mutate bracketed_paste (#1102)"
        );
        assert!(
            !settings.effective_bracketed_paste(),
            "legacy Windows console hosts do not support crossterm bracketed paste (#1102)"
        );
        assert_eq!(settings.synchronized_output, "off");

        // SAFETY: cleanup under the guard.
        unsafe {
            for (name, value) in prev {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn ssh_session_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let prev_client = std::env::var_os("SSH_CLIENT");
        let prev_tty = std::env::var_os("SSH_TTY");
        let prev_term_program = std::env::var_os("TERM_PROGRAM");
        for (var, val) in [
            ("SSH_CLIENT", "192.168.1.100 50000 22"),
            ("SSH_TTY", "/dev/pts/0"),
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::remove_var("SSH_CLIENT");
                std::env::remove_var("SSH_TTY");
                // Clear TERM_PROGRAM so the test isolates the SSH signal
                // — otherwise a leaked `TERM_PROGRAM=vscode` from a
                // concurrent test would already have forced low_motion
                // and the SSH-only assertion below would be a tautology.
                std::env::remove_var("TERM_PROGRAM");
                std::env::set_var(var, val);
            }
            let mut s = Settings::default();
            s.apply_env_overrides();
            assert!(
                s.low_motion,
                "{var}={val:?} must enable low_motion to prevent flickering in SSH sessions (#1433)"
            );
            assert!(
                !s.fancy_animations,
                "{var}={val:?} must disable fancy_animations in SSH sessions (#1433)"
            );
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("SSH_CLIENT");
            std::env::remove_var("SSH_TTY");
            if let Some(v) = prev_client {
                std::env::set_var("SSH_CLIENT", v);
            }
            if let Some(v) = prev_tty {
                std::env::set_var("SSH_TTY", v);
            }
            match prev_term_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn terminal_multiplexer_caps_redraws_without_disabling_motion() {
        let _g = term_program_test_guard();
        let vars = [
            "TMUX",
            "STY",
            "TERM_PROGRAM",
            "SSH_CLIENT",
            "SSH_TTY",
            "TILIX_ID",
            "TERMINATOR_UUID",
            "NO_ANIMATIONS",
            "WT_SESSION",
        ];
        let prev: Vec<_> = vars
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();

        for (var, val) in [
            ("TMUX", "/tmp/tmux-501/default,1234,0"),
            ("STY", "1234.pts-0.host"),
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                for name in vars {
                    std::env::remove_var(name);
                }
                std::env::set_var(var, val);
                #[cfg(windows)]
                std::env::set_var("WT_SESSION", "codewhale-test");
            }
            let mut settings = animated_settings();
            assert!(!settings.low_motion, "default is animated");
            assert!(settings.fancy_animations, "default shows the water strip");
            settings.apply_env_overrides();
            assert!(!settings.low_motion, "{var} must preserve authored motion");
            assert!(
                settings.fancy_animations,
                "{var} must preserve Ocean motion"
            );
            assert!(
                settings.constrained_frame_rate,
                "{var}={val:?} must cap redraws under terminal multiplexers"
            );
        }

        // SAFETY: cleanup under the guard.
        unsafe {
            for (name, value) in prev {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────────
    // synchronized_output / Ptyxis flicker detection
    // ────────────────────────────────────────────────────────────────────────

    #[test]
    fn synchronized_output_defaults_to_auto_and_resolves_to_enabled() {
        let s = Settings::default();
        assert_eq!(s.synchronized_output, "auto");
        assert!(
            s.synchronized_output_enabled(),
            "auto must keep DEC 2026 on so terminals that support it stay tear-free"
        );
    }

    #[test]
    fn synchronized_output_off_disables_dec_2026() {
        let s = Settings {
            synchronized_output: "off".to_string(),
            ..Settings::default()
        };
        assert!(!s.synchronized_output_enabled());
    }

    #[test]
    fn synchronized_output_on_keeps_dec_2026_enabled() {
        let s = Settings {
            synchronized_output: "on".to_string(),
            ..Settings::default()
        };
        assert!(s.synchronized_output_enabled());
    }

    #[test]
    fn synchronized_output_set_command_accepts_aliases() {
        let mut s = Settings::default();
        for value in ["auto", "AUTO", "default"] {
            s.set("synchronized_output", value).expect("valid");
            assert_eq!(s.synchronized_output, "auto");
        }
        for value in ["on", "true", "yes", "1", "ENABLED"] {
            s.set("sync_output", value).expect("valid");
            assert_eq!(s.synchronized_output, "on");
        }
        for value in ["off", "false", "no", "0", "DISABLED"] {
            s.set("sync", value).expect("valid");
            assert_eq!(s.synchronized_output, "off");
        }
        let err = s
            .set("synchronized_output", "maybe")
            .expect_err("unknown value rejected");
        assert!(
            err.to_string().contains("synchronized_output"),
            "error names the offending key: {err}"
        );
    }

    #[test]
    fn composer_multiline_mode_defaults_off_and_accepts_boolean_aliases() {
        let mut settings = Settings::default();
        assert!(!settings.composer_multiline_mode);

        settings.set("multiline", "on").expect("enable multiline");
        assert!(settings.composer_multiline_mode);

        settings
            .set("composer_multiline_mode", "false")
            .expect("disable multiline");
        assert!(!settings.composer_multiline_mode);
    }

    #[test]
    fn ptyxis_term_program_flips_synchronized_output_off() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        let prev_ptyxis = std::env::var_os("PTYXIS_VERSION");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "Ptyxis");
            std::env::remove_var("PTYXIS_VERSION");
        }
        let mut s = Settings::default();
        assert_eq!(s.synchronized_output, "auto");
        s.apply_env_overrides();
        assert_eq!(
            s.synchronized_output, "off",
            "Ptyxis 50.x mishandles DEC 2026 — auto must flip to off so VTE 0.84 stops flickering"
        );
        assert!(
            !s.synchronized_output_enabled(),
            "resolved boolean must agree with stored string"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_ptyxis {
                Some(v) => std::env::set_var("PTYXIS_VERSION", v),
                None => std::env::remove_var("PTYXIS_VERSION"),
            }
        }
    }

    #[test]
    fn tabby_uses_calm_rendering_for_stable_ime_cursor() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "Tabby");
        }
        let mut settings = animated_settings();
        settings.apply_env_overrides();
        assert!(settings.low_motion);
        assert!(!settings.fancy_animations);
        assert!(settings.constrained_frame_rate);
        assert_eq!(settings.synchronized_output, "off");
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn ptyxis_version_env_alone_flips_synchronized_output_off() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        let prev_ptyxis = std::env::var_os("PTYXIS_VERSION");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("PTYXIS_VERSION", "50.1");
        }
        let mut s = Settings::default();
        s.apply_env_overrides();
        assert_eq!(
            s.synchronized_output, "off",
            "PTYXIS_VERSION alone is sufficient — Ptyxis sets this even when TERM_PROGRAM isn't propagated"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_ptyxis {
                Some(v) => std::env::set_var("PTYXIS_VERSION", v),
                None => std::env::remove_var("PTYXIS_VERSION"),
            }
        }
    }

    #[test]
    fn ptyxis_does_not_override_user_explicit_on() {
        // Users who set `synchronized_output = "on"` (e.g. to confirm a
        // Ptyxis upgrade fixed it) must keep DEC 2026 even on Ptyxis.
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "ptyxis");
        }
        let mut s = Settings {
            synchronized_output: "on".to_string(),
            ..Settings::default()
        };
        s.apply_env_overrides();
        assert_eq!(
            s.synchronized_output, "on",
            "explicit user override must beat the Ptyxis env heuristic"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn ptyxis_does_not_override_user_explicit_off() {
        // A user with `synchronized_output = "off"` on a non-Ptyxis
        // terminal stays off after env detection (no-op flip).
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "xterm-256color");
        }
        let mut s = Settings {
            synchronized_output: "off".to_string(),
            ..Settings::default()
        };
        s.apply_env_overrides();
        assert_eq!(s.synchronized_output, "off");
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn non_ptyxis_term_programs_keep_synchronized_output_auto() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        let prev_ptyxis = std::env::var_os("PTYXIS_VERSION");
        // SAFETY: clean slate so non-Ptyxis programs don't see a leaked
        // PTYXIS_VERSION from another test.
        unsafe {
            std::env::remove_var("PTYXIS_VERSION");
        }
        for program in [
            "iTerm.app",
            "Apple_Terminal",
            "WezTerm",
            "xterm-256color",
            "gnome-terminal-server",
            // The Ghostty / VS Code paths keep DEC 2026 enabled; both handle
            // synchronized output cleanly even though their motion policies
            // differ.
            "ghostty",
            "vscode",
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("TERM_PROGRAM", program);
            }
            let mut s = Settings::default();
            s.apply_env_overrides();
            assert_eq!(
                s.synchronized_output, "auto",
                "TERM_PROGRAM={program:?} must not opt out of DEC 2026"
            );
            assert!(
                s.synchronized_output_enabled(),
                "resolved boolean for {program:?} must stay enabled"
            );
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_ptyxis {
                Some(v) => std::env::set_var("PTYXIS_VERSION", v),
                None => std::env::remove_var("PTYXIS_VERSION"),
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────────
    // Settings store tests
    // ────────────────────────────────────────────────────────────────────────

    /// Serialise tests that mutate `DEEPSEEK_CONFIG_PATH` through this guard
    /// so the parallel test runner doesn't observe interleaved env values.
    fn config_path_test_guard() -> crate::test_support::TestEnvLock {
        crate::test_support::lock_test_env()
    }

    #[test]
    fn startup_mode_writes_accept_act_plan_operate() {
        let mut settings = Settings::default();

        settings.set("default_mode", "plan").expect("plan mode");
        assert_eq!(settings.default_mode, "plan");
        settings
            .set("default_mode", "normal")
            .expect("legacy normal alias remains harmless");
        assert_eq!(settings.default_mode, "agent");
        settings
            .set("default_mode", "operate")
            .expect("operate is a valid startup mode");
        assert_eq!(settings.default_mode, "operate");
        settings
            .set("default_mode", "act")
            .expect("act alias maps to agent wire value");
        assert_eq!(settings.default_mode, "agent");

        let err = settings
            .set("default_mode", "yolo")
            .expect_err("yolo remains a permission migration alias, not a mode write");
        assert!(
            err.to_string().contains("work (agent), plan, or operate"),
            "{err}"
        );
    }

    #[test]
    fn legacy_startup_modes_migrate_without_losing_permission_intent() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let codewhale_home = tmp.path().join(".codewhale");
        std::fs::create_dir_all(&codewhale_home).expect("codewhale home");
        std::fs::write(
            codewhale_home.join("settings.toml"),
            "default_mode = \"yolo\"\n",
        )
        .expect("legacy settings");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home = EnvVarRestore::set("CODEWHALE_HOME", &codewhale_home);
        let _home = EnvVarRestore::set("HOME", tmp.path());

        let loaded = Settings::load_persisted().expect("load legacy settings");

        assert_eq!(loaded.default_mode, "agent");
        assert_eq!(loaded.permission_posture.as_deref(), Some("full-access"));

        std::fs::write(
            codewhale_home.join("settings.toml"),
            "default_mode = \"operate\"\n",
        )
        .expect("operate startup settings");
        let loaded = Settings::load_persisted().expect("load operate settings");
        assert_eq!(loaded.default_mode, "operate");
        assert_eq!(loaded.permission_posture, None);
    }

    #[test]
    fn settings_path_defaults_to_codewhale_home_for_new_writes() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home = EnvVarRestore::set("CODEWHALE_HOME", tmp.path().join(".codewhale"));
        let _home = EnvVarRestore::set("HOME", tmp.path());

        let got = Settings::path().expect("settings path");

        assert_eq!(got, tmp.path().join(".codewhale").join("settings.toml"));
    }

    #[test]
    fn settings_path_prefers_codewhale_home_even_when_legacy_exists() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy_dir = tmp.path().join(".deepseek");
        std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
        std::fs::write(legacy_dir.join("settings.toml"), "low_motion = true\n")
            .expect("legacy settings");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home = EnvVarRestore::set("CODEWHALE_HOME", tmp.path().join(".codewhale"));
        let _home = EnvVarRestore::set("HOME", tmp.path());

        let got = Settings::path().expect("settings path");

        assert_eq!(got, tmp.path().join(".codewhale").join("settings.toml"));
    }

    #[test]
    fn settings_load_migrates_legacy_deepseek_home_into_codewhale_home_without_explicit_home() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join(".codewhale").join("settings.toml");
        let legacy_dir = tmp.path().join(".deepseek");
        let legacy_home = legacy_dir.join("settings.toml");
        std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
        std::fs::write(&legacy_home, "low_motion = true\n").expect("legacy settings");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home = EnvVarRestore::remove("CODEWHALE_HOME");
        let _home = EnvVarRestore::set("HOME", tmp.path());

        let loaded = Settings::load_persisted().expect("load persisted settings");

        assert!(loaded.low_motion, "legacy settings should still be read");
        assert!(
            primary.exists(),
            "settings load should migrate to primary path"
        );
        let display = loaded.display(codewhale_localization::Locale::En);
        assert!(
            display.contains(&format!("Config file: {}", primary.display())),
            "settings display should surface the canonical codewhale path:\n{display}"
        );
    }

    #[test]
    fn settings_load_read_only_reads_legacy_home_without_creating_primary() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join(".codewhale").join("settings.toml");
        let legacy = tmp.path().join(".deepseek").join("settings.toml");
        let legacy_bytes =
            b"default_mode = \"plan\"\nlow_motion = false\nfancy_animations = true\n";
        std::fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy directory");
        std::fs::write(&legacy, legacy_bytes).expect("legacy settings");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home = EnvVarRestore::remove("CODEWHALE_HOME");
        let _home = EnvVarRestore::set("HOME", tmp.path());
        let _no_animations = EnvVarRestore::set("NO_ANIMATIONS", "1");

        let loaded = Settings::load_read_only().expect("read-only settings load");

        assert_eq!(loaded.default_mode, "plan");
        assert!(loaded.low_motion, "environment overlays still apply");
        assert!(
            !loaded.fancy_animations,
            "environment overlays still apply to parsed legacy settings"
        );
        assert!(
            !primary.exists(),
            "a diagnostic settings read must not create the primary settings path"
        );
        assert_eq!(
            std::fs::read(&legacy).expect("legacy settings after read"),
            legacy_bytes,
            "a diagnostic settings read must not rewrite the legacy settings file"
        );
    }

    #[test]
    fn legacy_route_preferences_ignore_project_settings_and_runtime_overlays() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let global = tmp.path().join("global");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&global).expect("global directory");
        std::fs::create_dir_all(&project).expect("project directory");
        let global_bytes = b"default_provider = \"zai\"\nlow_motion = false\n[provider_models]\nzai = \"GLM-5.3\"\n";
        let project_bytes = b"default_provider = \"openai\"\nlow_motion = false\n[provider_models]\nopenai = \"project-model\"\n";
        let global_settings = global.join(SETTINGS_FILE_NAME);
        let project_settings = project.join(SETTINGS_FILE_NAME);
        std::fs::write(&global_settings, global_bytes).expect("global settings");
        std::fs::write(&project_settings, project_bytes).expect("project settings");
        let _global_home = EnvVarRestore::set("CODEWHALE_HOME", &global);
        let _config_override =
            EnvVarRestore::set("CODEWHALE_CONFIG_PATH", project.join("config.toml"));
        let _legacy_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", project.join("config.toml"));
        let _no_animations = EnvVarRestore::set("NO_ANIMATIONS", "1");

        let legacy =
            Settings::load_legacy_route_preferences_read_only().expect("global legacy preferences");
        assert_eq!(legacy.default_provider.as_deref(), Some("zai"));
        assert_eq!(
            legacy
                .provider_models
                .as_ref()
                .and_then(|models| models.get("zai"))
                .map(String::as_str),
            Some("GLM-5.3")
        );
        assert!(
            !legacy.low_motion,
            "migration must read the persisted value"
        );
        let ordinary = Settings::load_read_only().expect("ordinary project settings");
        assert_eq!(ordinary.default_provider.as_deref(), Some("openai"));
        assert!(
            ordinary.low_motion,
            "ordinary runtime overlays are unchanged"
        );
        assert_eq!(
            std::fs::read(&global_settings).expect("unchanged global settings"),
            global_bytes
        );
        assert_eq!(
            std::fs::read(&project_settings).expect("unchanged project settings"),
            project_bytes
        );

        std::fs::remove_file(&global_settings).expect("remove fixture global settings");
        let missing = Settings::load_legacy_route_preferences_read_only()
            .expect("missing global preferences");
        assert_eq!(missing.default_provider, None);
        assert!(missing.provider_models.is_none());
        assert!(
            !global_settings.exists(),
            "migration reads must not create settings"
        );
        assert!(!global.join("config.toml").exists());
        assert!(!project.join("config.toml").exists());
    }

    #[test]
    fn project_path_guard_does_not_authorize_global_legacy_settings_reads() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _config_override =
            EnvVarRestore::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
        let _global_home = EnvVarRestore::remove("CODEWHALE_HOME");
        let _home = EnvVarRestore::remove("HOME");
        let _userprofile = EnvVarRestore::remove("USERPROFILE");

        assert_eq!(
            settings_path_candidates_for_scope(false),
            (
                Some(crate::test_support::unsealed_test_state_root().join(SETTINGS_FILE_NAME)),
                None,
                None,
            ),
            "a project-only test must stay isolated when reading global preferences"
        );
    }

    #[test]
    fn settings_load_migrates_platform_legacy_fallback_into_codewhale_home_without_explicit_home() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join(".codewhale").join("settings.toml");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home =
            EnvVarRestore::set("CODEWHALE_HOME", primary.parent().expect("primary parent"));
        let legacy_config_dir = tmp
            .path()
            .join("platform-config")
            .join("deepseek")
            .join("settings.toml");
        std::fs::create_dir_all(legacy_config_dir.parent().expect("parent"))
            .expect("legacy config dir");
        std::fs::write(&legacy_config_dir, "low_motion = true\n").expect("legacy settings");

        // Exercise the same load and migration path with explicit candidates.
        // `dirs::config_dir()` uses the Win32 known-folder API on Windows, so
        // APPDATA/XDG environment overrides cannot isolate that process-global
        // location in a parallel test runner.
        let loaded = Settings::load_persisted_from_candidates(
            Some(primary.clone()),
            None,
            Some(legacy_config_dir),
        )
        .expect("load persisted settings");

        assert!(loaded.low_motion, "legacy settings should still be read");
        assert!(
            primary.exists(),
            "legacy fallback should be copied into primary"
        );
        let display = loaded.display(codewhale_localization::Locale::En);
        assert!(
            display.contains(&format!("Config file: {}", primary.display())),
            "settings display should surface the canonical codewhale path:\n{display}"
        );
    }

    #[test]
    fn settings_load_ignores_legacy_files_when_codewhale_home_is_explicit() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let explicit_home = tmp.path().join("isolated-codewhale");
        let legacy_dir = tmp.path().join(".deepseek");
        std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
        std::fs::write(
            legacy_dir.join("settings.toml"),
            "theme = \"dracula\"\ncomposer_density = \"spacious\"\nsidebar_width_percent = 42\n",
        )
        .expect("legacy settings");
        let _config_override = EnvVarRestore::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_home = EnvVarRestore::set("CODEWHALE_HOME", &explicit_home);
        let _home = EnvVarRestore::set("HOME", tmp.path());

        let loaded = Settings::load().expect("load settings");

        assert_eq!(
            loaded.theme, "underwater",
            "explicit CODEWHALE_HOME must not inherit ambient legacy settings"
        );
        assert_eq!(
            loaded.composer_density, "comfortable",
            "explicit CODEWHALE_HOME must not inherit ambient legacy settings"
        );
        assert_eq!(
            loaded.sidebar_width_percent, 28,
            "explicit CODEWHALE_HOME must not inherit ambient legacy settings"
        );
        assert!(
            !explicit_home.join("settings.toml").exists(),
            "ambient legacy settings must not be migrated into explicit CODEWHALE_HOME"
        );
    }

    #[test]
    fn settings_load_migrates_legacy_saved_auto_sidebar_focus_to_rail() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let settings_path = tmp.path().join("settings.toml");
        std::fs::write(&settings_path, "sidebar_focus = \"auto\"\n").expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        // A settings.toml that only names `sidebar_focus = "auto"` — the
        // shipped default — must not silently earn an always-on rail strip.
        assert_eq!(loaded.rail_panel, "tasks");
        assert_eq!(loaded.work_surface_placement, "bottom");
    }

    #[test]
    fn settings_load_migrates_hidden_sidebar_to_rail_off() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let settings_path = tmp.path().join("settings.toml");
        std::fs::write(&settings_path, "sidebar_focus = \"hidden\"\n").expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        assert_eq!(loaded.work_surface_placement, "off");
    }

    #[test]
    fn hidden_legacy_sidebar_does_not_override_an_explicit_new_rail_placement() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let settings_path = tmp.path().join("settings.toml");
        std::fs::write(
            &settings_path,
            "sidebar_focus = \"hidden\"\nwork_surface_placement = \"left\"\n",
        )
        .expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        assert_eq!(loaded.work_surface_placement, "left");
    }

    /// The dead `tui.toml` store is folded into `settings.toml` on load: a
    /// value settings.toml does not own is adopted, a value it owns wins, the
    /// original bytes are moved aside, and unmappable keys are named.
    #[test]
    fn tui_toml_theme_is_folded_when_settings_toml_is_silent() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("settings.toml"),
            "cost_currency = \"usd\"\n",
        )
        .expect("settings");
        let prefs_path = tmp.path().join("tui.toml");
        std::fs::write(&prefs_path, "theme = \"light\"\n").expect("tui prefs");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        assert_eq!(loaded.theme, "light");
        let receipt = loaded.tui_prefs_migration().expect("receipt");
        assert_eq!(
            receipt.folded,
            vec![("theme".to_string(), "light".to_string())]
        );
        assert!(receipt.kept.is_empty());
        assert!(!prefs_path.exists(), "original must be moved aside");
        let backup = receipt.backup.as_ref().expect("backup path");
        assert_eq!(
            std::fs::read_to_string(backup).expect("backup readable"),
            "theme = \"light\"\n",
            "backup keeps the original bytes"
        );
        // The fold is only real once settings.toml owns it on disk.
        let persisted =
            std::fs::read_to_string(tmp.path().join("settings.toml")).expect("settings.toml");
        assert!(persisted.contains("light"), "not persisted: {persisted}");
    }

    #[test]
    fn explicit_settings_theme_wins_over_tui_toml_and_the_receipt_says_so() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("settings.toml"), "theme = \"dark\"\n").expect("settings");
        std::fs::write(tmp.path().join("tui.toml"), "theme = \"light\"\n").expect("tui prefs");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        assert_eq!(loaded.theme, "dark");
        let receipt = loaded.tui_prefs_migration().expect("receipt");
        assert!(receipt.folded.is_empty());
        assert_eq!(
            receipt.kept,
            vec![("theme".to_string(), "light".to_string(), "dark".to_string())]
        );
        assert!(
            !receipt.lines(codewhale_localization::Locale::En).is_empty(),
            "a disagreement must be sayable"
        );
    }

    #[test]
    fn tui_toml_keys_without_a_setting_are_quarantined_never_dropped() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prefs_path = tmp.path().join("tui.toml");
        std::fs::write(
            &prefs_path,
            "theme = \"light\"\nfont_size = 14\n\n[keybinds]\nsubmit = \"ctrl+enter\"\n",
        )
        .expect("tui prefs");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        let receipt = loaded.tui_prefs_migration().expect("receipt");
        assert_eq!(receipt.quarantined, vec!["font_size", "keybinds"]);
        let backup = receipt.backup.as_ref().expect("backup path");
        let preserved = std::fs::read_to_string(backup).expect("backup readable");
        assert!(preserved.contains("font_size = 14"), "{preserved}");
        assert!(preserved.contains("ctrl+enter"), "{preserved}");
        let line = receipt
            .lines(codewhale_localization::Locale::En)
            .join(" ")
            .to_lowercase();
        assert!(line.contains("font_size"), "{line}");
        assert!(line.contains("keybinds"), "{line}");
    }

    #[test]
    fn an_unparseable_tui_toml_is_parked_whole_rather_than_guessed_at() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("tui.toml"), "theme = \n").expect("tui prefs");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");

        let receipt = loaded.tui_prefs_migration().expect("receipt");
        assert_eq!(receipt.quarantined, vec!["tui.toml"]);
        assert!(receipt.folded.is_empty());
        assert!(receipt.backup.is_some(), "bytes must survive");
    }

    #[test]
    fn a_read_only_load_never_moves_tui_toml_aside() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prefs_path = tmp.path().join("tui.toml");
        std::fs::write(&prefs_path, "theme = \"light\"\n").expect("tui prefs");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load_read_only().expect("read-only load");

        assert_eq!(loaded.theme, "light");
        assert!(prefs_path.exists(), "diagnostics must not mutate the disk");
    }

    #[test]
    fn a_second_backup_never_clobbers_the_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prefs_path = tmp.path().join("tui.toml");
        std::fs::write(&prefs_path, "theme = \"light\"\n").expect("first");
        let first = back_up_tui_prefs(&prefs_path).expect("first backup");
        std::fs::write(&prefs_path, "theme = \"dark\"\n").expect("second");
        let second = back_up_tui_prefs(&prefs_path).expect("second backup");

        assert_ne!(first, second);
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "theme = \"light\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(&second).unwrap(),
            "theme = \"dark\"\n"
        );
    }

    /// A successful `set()` marks the canonical key as session-supplied,
    /// whatever spelling was used; aliases report the same layer as the row.
    /// A rejected value marks nothing.
    #[test]
    fn set_marks_session_provenance_for_every_spelling() {
        let cases: &[(&[&str], &str)] = &[
            (&["auto_compact", "compact"], "true"),
            (
                &["auto_compact_threshold_percent", "auto_compact_threshold"],
                "80",
            ),
            (&["calm_mode", "calm"], "true"),
            (
                &["tool_collapse", "tool_collapse_mode", "collapse"],
                "expanded",
            ),
            (&["low_motion", "motion"], "true"),
            (&["fancy_animations", "fancy", "animations"], "true"),
            (&["focus_texture", "texture"], "grain"),
            (
                &["work_surface_placement", "work_surface", "work_rail"],
                "left",
            ),
            (&["rail_panel", "rail"], "tasks"),
            (&["work_surface_top_height", "work_top_height"], "8"),
            (&["work_surface_side_width", "work_side_width"], "40"),
            (&["bracketed_paste", "paste"], "true"),
            (&["paste_burst_detection", "paste_burst"], "true"),
            (&["mention_menu_limit", "mention_limit"], "64"),
            (
                &[
                    "mention_walk_depth",
                    "mention_depth",
                    "completions_walk_depth",
                ],
                "5",
            ),
            (
                &["mention_menu_behavior", "mention_behavior", "mention_menu"],
                "fuzzy",
            ),
            (&["show_thinking", "thinking"], "true"),
            (&["thinking_default_expanded", "thinking_expanded"], "true"),
            (&["thinking_preview_lines", "thinking_preview"], "3"),
            (&["thinking_highlight", "reasoning_highlight"], "true"),
            (&["help_expand_groups", "help_expanded"], "true"),
            (&["pin_last_prompt", "pin_prompt"], "true"),
            (&["show_tool_details", "tool_details"], "true"),
            (&["inline_diffs", "inline_diff", "diffs"], "off"),
            (&["locale", "language"], "en"),
            (&["theme", "ui_theme"], "terminal"),
            (&["background_color", "background", "bg"], "#1a1b26"),
            (&["composer_density", "composer"], "compact"),
            (&["composer_border", "border"], "true"),
            (
                &["composer_multiline_mode", "multiline_mode", "multiline"],
                "true",
            ),
            (&["composer_vim_mode", "vim_mode", "vim"], "vim"),
            (&["transcript_spacing", "spacing"], "compact"),
            (&["status_indicator", "indicator"], "off"),
            (&["synchronized_output", "sync_output", "sync"], "off"),
            (&["workspace_follow_symlinks", "follow_symlinks"], "true"),
            (&["default_mode", "mode"], "plan"),
            (&["context_panel", "context", "session_panel"], "true"),
            (&["sessions_rail", "sessions_panel", "session_rail"], "true"),
            (&["session_auto_resume", "auto_resume"], "true"),
            (&["cost_currency", "currency"], "cny"),
            (&["max_history", "history"], "50"),
            (&["reasoning_effort", "effort"], "low"),
            (&["permission_posture", "permissions"], "ask"),
            (
                &["sandbox_mode", "sandbox", "filesystem_sandbox"],
                "read-only",
            ),
        ];
        for (spellings, value) in cases {
            let canonical = spellings[0];
            // `config set`, `/config` and `config doctor` route and suggest
            // from the schema by this canonical key; a `/set` spelling with
            // no declaration would be settable but unplaceable.
            assert!(
                codewhale_config::setting(canonical).is_some(),
                "`/set {canonical}` is accepted but undeclared in SETTINGS_SCHEMA"
            );
            for spelling in *spellings {
                assert_eq!(
                    Settings::canonical_key(spelling),
                    Some(canonical),
                    "{spelling}"
                );
                let mut settings = Settings::default();
                assert_eq!(settings.provenance(canonical), Layer::Default);
                settings
                    .set(spelling, value)
                    .unwrap_or_else(|error| panic!("set({spelling}) rejected: {error:#}"));
                assert_eq!(
                    settings.provenance(canonical),
                    Layer::SessionOverride,
                    "{spelling} did not mark {canonical} as session"
                );
                assert_eq!(
                    settings.provenance(spelling),
                    Layer::SessionOverride,
                    "{spelling} does not resolve to its own layer"
                );
            }
        }
        // `default_model` has no provenance case above; check its spellings too.
        for spelling in ["default_model", "model"] {
            let canonical = Settings::canonical_key(spelling).expect("model spelling");
            assert!(codewhale_config::setting(canonical).is_some(), "{spelling}");
        }
    }

    #[test]
    fn set_rejection_marks_no_provenance() {
        let mut settings = Settings::default();
        assert!(settings.set("theme", "not-a-theme").is_err());
        assert_eq!(settings.provenance("theme"), Layer::Default);
        assert!(settings.set("no_such_key", "1").is_err());
        assert_eq!(settings.provenance("no_such_key"), Layer::Default);
    }

    /// Keys the document named load as user config; the rest are default.
    #[test]
    fn load_marks_explicit_keys_as_user_config() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("settings.toml"), "theme = \"light\"\n").expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");
        assert_eq!(loaded.provenance("theme"), Layer::UserConfig);
        assert_eq!(loaded.provenance("locale"), Layer::Default);
    }

    /// `/settings` names the keys the load owns, so the text surface says
    /// what the user set instead of printing defaults silently.
    #[test]
    fn display_names_user_configured_keys() {
        let _g = config_path_test_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("settings.toml"), "theme = \"light\"\n").expect("settings");
        let _config_override =
            EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", tmp.path().join("config.toml"));

        let loaded = Settings::load().expect("load settings");
        let text = loaded.display(codewhale_localization::Locale::En);
        assert!(text.contains("from settings.toml: theme"), "{text}");
        assert!(!text.contains("session override"), "{text}");

        let mut session = Settings::default();
        session.set("locale", "en").expect("set locale");
        let text = session.display(codewhale_localization::Locale::En);
        assert!(text.contains("session override: locale"), "{text}");
    }

    #[test]
    fn settings_save_preserves_comments() {
        let _g = config_path_test_guard();
        let tmp = std::env::temp_dir().join("dst_settings_comment_test");
        std::fs::create_dir_all(&tmp).unwrap();
        let config_file = tmp.join("config.toml");
        let _config_override = EnvVarRestore::set("DEEPSEEK_CONFIG_PATH", &config_file);

        // settings.toml lives next to config.toml
        let settings_path = tmp.join("settings.toml");
        std::fs::write(
            &settings_path,
            "# my setting\ncost_currency = \"usd\"\n# trailing\n",
        )
        .unwrap();

        // Load the existing file so we have a real struct to modify.
        let mut settings = Settings::load().expect("load settings");
        settings.cost_currency = "cny".to_string();
        settings.save().expect("save should succeed");

        let body = std::fs::read_to_string(&settings_path).expect("read settings.toml");
        assert!(body.contains("# my setting"), "comment lost: {body}");
        assert!(body.contains("# trailing"), "trailing lost: {body}");
        assert!(body.contains("cny"), "new value not written: {body}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pinned_models_are_exact_ordered_and_round_trip() {
        let mut settings = Settings::default();
        assert!(settings.toggle_pinned_model("zai", "glm-5.2"));
        assert!(settings.toggle_pinned_model("openrouter", "glm-5.2"));
        assert_eq!(settings.pinned_models[0].provider, "zai");
        assert!(settings.move_pinned_model("openrouter", "glm-5.2", -1));
        assert_eq!(settings.pinned_models[0].provider, "openrouter");
        settings.pinned_models[0].label = Some("fast".to_string());
        let encoded = toml::to_string(&settings).unwrap();
        let decoded: Settings = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.pinned_models, settings.pinned_models);
        assert!(!settings.toggle_pinned_model("openrouter", "glm-5.2"));
        assert_eq!(settings.pinned_models.len(), 1);
    }
}
