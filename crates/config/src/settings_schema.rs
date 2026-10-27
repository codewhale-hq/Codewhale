//! One declaration per setting: type, default, and an optional `ui` block.
//!
//! # What this replaces
//!
//! This table is the single source that the settings surface projects. It
//! replaces, and is the reason for deleting, these hand-maintained key tables
//! that used to live beside `ConfigView` in `crates/tui/src/tui/views/mod.rs`:
//!
//! - the `Vec<ConfigRow>` literal's `section:` field (row → heading),
//!   and `ConfigCategory::for_section` / `ConfigRowFacts::category`
//!   (row → rail tab),
//! - `config_boolean_key` and `config_integer_key` (key → editor kind),
//! - `config_choice_values` (key → enum values),
//! - `config_choice_label` / `config_choice_detail` (value → label/detail),
//! - `config_label_message` (key → label string),
//! - `config_hint_for_key` (key → description string).
//!
//! # The rule
//!
//! A setting is declared once, here. `ui: Some(..)` puts it on the settings
//! screen; `ui: None` keeps the declaration (type, default, and the fact that
//! the key is known) without giving it a row. Visibility is a property of the
//! declaration, not of the renderer.
//!
//! `label`, `description`, and the per-value `label`/`description` are
//! *message keys*, not prose: the localization pack owns the text in fifteen
//! languages, this table owns which string a setting shows. An empty key means
//! "no string" — the surface humanizes the setting key instead.
//!
//! `tab` and `group` are ids the settings screen resolves to its rail
//! categories and section headings. Declaration order is render order:
//! distinct tabs appear in the order they are first declared, groups in the
//! order they are first declared within a tab, and rows in declaration order
//! within a group.
//!
//! Two settings take their values from a runtime registry rather than this
//! table (`theme` from the shipped palettes, `locale` from the shipped packs).
//! They are declared `String`; the surface supplies the live value list.
//! `reasoning_effort` declares the canonical nine-spelling effort vocabulary
//! here so `/config` and `/effort` cannot disagree; the live settings screen
//! still narrows that list to the active route's rungs.

/// Fresh terminal installs, theme resets and the settings schema share this default.
pub const DEFAULT_TUI_THEME: &str = "underwater";

/// One selectable value of a [`SettingKind::Enum`] (or a boolean override).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingOption {
    pub value: &'static str,
    /// Message key for the value's label; empty means "show the raw value".
    pub label: &'static str,
    /// Message key for the value's one-line detail; empty means none.
    pub description: &'static str,
}

impl SettingOption {
    const fn new(value: &'static str, label: &'static str, description: &'static str) -> Self {
        Self {
            value,
            label,
            description,
        }
    }
}

/// The value type of a setting, and for closed value sets, the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// `true` / `false`. An empty option slice means the surface's default
    /// on/off labels; a non-empty one overrides them per value.
    Bool(&'static [SettingOption]),
    Int,
    /// A fractional number such as a percent threshold. Values are served and
    /// accepted in plain decimal form; bounds live in the write validator.
    Float,
    Enum(&'static [SettingOption]),
    String,
}

/// What a declared settings row is. `Setting` is a writable preference;
/// `Action` opens another surface (the provider/model pickers, module
/// links); `Diagnostic` is a read-only receipt or managed-policy fact;
/// `Session` is editable for the running session but does not persist
/// through the config store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingRowKind {
    Setting,
    Action,
    Diagnostic,
    Session,
}

/// Where a setting appears, and what it says. Absent ⇒ no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingUi {
    pub tab: &'static str,
    pub group: &'static str,
    /// Message key for the row label; empty ⇒ humanize the setting key.
    pub label: &'static str,
    /// Message key for the row's description sentence.
    pub description: &'static str,
    /// Whether the row is a writable preference, a link to another surface,
    /// a read-only receipt, or a session-scoped override. Surfaces that
    /// cannot open the target or scope the write render non-`Setting` rows
    /// read-only instead of guessing.
    pub row: SettingRowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingDef {
    pub key: &'static str,
    pub kind: SettingKind,
    /// The value in force when nothing is configured, as it is written to
    /// disk. Empty for session-owned rows, actions, and receipts.
    pub default: &'static str,
    pub ui: Option<SettingUi>,
}

impl SettingDef {
    /// The closed value set, when this setting has one. `None` means the
    /// surface supplies values (free text, an integer, or a runtime registry).
    pub fn values(&self) -> Option<Vec<&'static str>> {
        match self.kind {
            SettingKind::Bool(_) => Some(vec!["false", "true"]),
            SettingKind::Enum(options) => Some(options.iter().map(|o| o.value).collect()),
            SettingKind::Int | SettingKind::String | SettingKind::Float => None,
        }
    }

    /// Per-value label/description metadata for `value`, when declared.
    pub fn option(&self, value: &str) -> Option<&'static SettingOption> {
        let options = match self.kind {
            SettingKind::Bool(options) | SettingKind::Enum(options) => options,
            SettingKind::Int | SettingKind::String | SettingKind::Float => return None,
        };
        options.iter().find(|option| option.value == value)
    }

    pub fn is_bool(&self) -> bool {
        matches!(self.kind, SettingKind::Bool(_))
    }

    pub fn is_int(&self) -> bool {
        matches!(self.kind, SettingKind::Int)
    }

    pub fn is_float(&self) -> bool {
        matches!(self.kind, SettingKind::Float)
    }
}

const NOTIFICATION_SOUNDS: &[SettingOption] = &[
    SettingOption::new("legacy", "ConfigChoiceNotificationLegacy", ""),
    SettingOption::new("off", "ConfigValueOff", ""),
    SettingOption::new("whale", "ConfigChoiceNotificationWhale", ""),
    SettingOption::new("bell", "ConfigChoiceNotificationBell", ""),
    SettingOption::new("beep", "ConfigChoiceNotificationBell", ""),
    SettingOption::new("file", "ConfigChoiceNotificationFile", ""),
];

const NOTIFICATION_COMPLETION_SOUNDS: &[SettingOption] = &[
    SettingOption::new("off", "ConfigValueOff", ""),
    SettingOption::new("whale", "ConfigChoiceNotificationWhale", ""),
    SettingOption::new("bell", "ConfigChoiceNotificationBell", ""),
    SettingOption::new("beep", "ConfigChoiceNotificationBell", ""),
    SettingOption::new("file", "ConfigChoiceNotificationFile", ""),
];

const NOTIFICATION_CONDITIONS: &[SettingOption] = &[
    SettingOption::new("always", "ConfigChoiceNotificationAlways", ""),
    SettingOption::new("unfocused", "ConfigChoiceNotificationUnfocused", ""),
    SettingOption::new("never", "ConfigChoiceNotificationNever", ""),
];

const NOTIFICATION_METHODS: &[SettingOption] = &[
    SettingOption::new("auto", "", ""),
    SettingOption::new("off", "", ""),
    SettingOption::new("osc9", "", ""),
    SettingOption::new("bel", "", ""),
    SettingOption::new("kitty", "", ""),
    SettingOption::new("ghostty", "", ""),
];

const NOTIFICATION_SUBAGENTS: &[SettingOption] = &[
    SettingOption::new("off", "", ""),
    SettingOption::new("final-only", "", ""),
    SettingOption::new("always", "", ""),
];

const ON_OFF: &[SettingOption] = &[];

const TELEMETRY: &[SettingOption] = &[
    SettingOption::new("false", "ConfigValueTelemetryOff", ""),
    SettingOption::new("true", "ConfigValueTelemetryOn", ""),
];

const LOW_MOTION: &[SettingOption] = &[
    SettingOption::new("false", "ConfigValueOff", "ConfigChoiceDetailLowMotionOff"),
    SettingOption::new("true", "ConfigValueOn", "ConfigChoiceDetailLowMotionOn"),
];

const FANCY_ANIMATIONS: &[SettingOption] = &[
    SettingOption::new("false", "ConfigValueOff", "ConfigChoiceDetailFancyOff"),
    SettingOption::new("true", "ConfigValueOn", "ConfigChoiceDetailFancyOn"),
];

const SHOW_THINKING: &[SettingOption] = &[
    SettingOption::new(
        "false",
        "ConfigValueOff",
        "ConfigChoiceDetailShowThinkingOff",
    ),
    SettingOption::new("true", "ConfigValueOn", "ConfigChoiceDetailShowThinkingOn"),
];

const THINKING_HIGHLIGHT: &[SettingOption] = &[
    SettingOption::new(
        "false",
        "ConfigValueOff",
        "ConfigChoiceDetailThinkingHighlightOff",
    ),
    SettingOption::new(
        "true",
        "ConfigValueOn",
        "ConfigChoiceDetailThinkingHighlightOn",
    ),
];

const PERMISSION_POSTURE: &[SettingOption] = &[
    SettingOption::new("ask", "ConfigChoiceAsk", "ConfigChoiceDetailAsk"),
    SettingOption::new(
        "auto-review",
        "ConfigChoiceAutoReview",
        "ConfigChoiceDetailAutoReview",
    ),
    SettingOption::new(
        "full-access",
        "ConfigChoiceFullAccess",
        "ConfigChoiceDetailFullAccess",
    ),
];

const APPROVAL_MODE: &[SettingOption] = &[
    SettingOption::new("ask", "ConfigChoiceAsk", "ConfigChoiceDetailAsk"),
    SettingOption::new(
        "auto-review",
        "ConfigChoiceAutoReview",
        "ConfigChoiceDetailAutoReview",
    ),
    SettingOption::new(
        "full-access",
        "ConfigChoiceFullAccess",
        "ConfigChoiceDetailFullAccess",
    ),
];

const APPROVAL_POLICY: &[SettingOption] = &[
    SettingOption::new(
        "use-tui-default",
        "ConfigChoiceUseTuiDefault",
        "ConfigChoiceDetailUseTuiDefault",
    ),
    SettingOption::new("ask", "ConfigChoiceAsk", "ConfigChoiceDetailAsk"),
    SettingOption::new(
        "auto-review",
        "ConfigChoiceAutoReview",
        "ConfigChoiceDetailAutoReview",
    ),
    SettingOption::new(
        "full-access",
        "ConfigChoiceFullAccess",
        "ConfigChoiceDetailFullAccess",
    ),
    // `never` is a managed-policy value only: it is accepted from config.toml and
    // shown read-only, never offered by the editor.
];

const DEFAULT_MODE: &[SettingOption] = &[
    SettingOption::new(
        "agent",
        "ConfigChoiceModeAct",
        "ConfigChoiceDetailModeAgent",
    ),
    SettingOption::new("plan", "ConfigChoiceModePlan", "ConfigChoiceDetailModePlan"),
    SettingOption::new(
        "operate",
        "ConfigChoiceModeOperate",
        "ConfigChoiceDetailModeOperate",
    ),
];

const FOCUS_TEXTURE: &[SettingOption] = &[
    SettingOption::new("off", "ConfigValueOff", ""),
    SettingOption::new("scrim", "", ""),
    SettingOption::new("grain", "", ""),
];

const INLINE_DIFFS: &[SettingOption] = &[
    SettingOption::new("full", "ConfigChoiceDiffFull", ""),
    SettingOption::new("summary", "ConfigChoiceDiffSummary", ""),
    SettingOption::new("off", "ConfigValueOff", ""),
];

const STATUS_INDICATOR: &[SettingOption] = &[
    // `whale` is retired: load migrates whale | 🐳 | 🐋 to the typographic
    // mark, so the editor no longer offers it.
    SettingOption::new("cw", "ConfigChoiceStatusCw", ""),
    SettingOption::new("dots", "ConfigChoiceStatusDots", ""),
    SettingOption::new("off", "ConfigValueOff", ""),
];

const SYNCHRONIZED_OUTPUT: &[SettingOption] = &[
    SettingOption::new("auto", "", ""),
    SettingOption::new("on", "", ""),
    SettingOption::new("off", "", ""),
];

const COST_CURRENCY: &[SettingOption] = &[
    SettingOption::new("usd", "", ""),
    SettingOption::new("cny", "", ""),
];

/// The canonical `ReasoningEffort::as_setting` spellings, in the order
/// `auto, off, minimal, low, medium, high, xhigh, ultra, max`.
///
/// This is the same vocabulary `codewhale_tui::reasoning_preference::
/// ReasoningEffort::parse_strict` accepts (`/effort`), so the settings schema
/// and the command cannot drift. Labels stay empty: an undeclared label means
/// "show the raw value", and a capitalized literal would also have to be
/// localized in every shipped locale to satisfy the schema message-key check.
/// Canonical setting value for a reasoning-effort spelling, accepting the
/// documented aliases. Unknown values are refused with the full vocabulary.
pub fn canonical_reasoning_effort(value: &str) -> Result<&'static str, String> {
    let trimmed = value.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => Ok("off"),
        "minimal" => Ok("minimal"),
        "low" | "minimum" | "light" => Ok("low"),
        "medium" | "mid" => Ok("medium"),
        "high" => Ok("high"),
        "xhigh" => Ok("xhigh"),
        "auto" | "automatic" => Ok("auto"),
        "ultra" | "ultracode" => Ok("ultra"),
        "max" | "maximum" => Ok("max"),
        _ => Err(format!(
            "Unrecognized reasoning effort {trimmed:?}. Expected one of: auto, off, minimal, low, medium, high, xhigh, ultra, or max."
        )),
    }
}

const REASONING_EFFORT: &[SettingOption] = &[
    SettingOption::new("auto", "", ""),
    SettingOption::new("off", "", ""),
    SettingOption::new("minimal", "", ""),
    SettingOption::new("low", "", ""),
    SettingOption::new("medium", "", ""),
    SettingOption::new("high", "", ""),
    SettingOption::new("xhigh", "", ""),
    SettingOption::new("ultra", "", ""),
    SettingOption::new("max", "", ""),
];

const DENSITY: &[SettingOption] = &[
    SettingOption::new("compact", "", ""),
    SettingOption::new("comfortable", "", ""),
    SettingOption::new("spacious", "", ""),
];

const TRANSCRIPT_SPACING: &[SettingOption] = &[
    SettingOption::new("compact", "", ""),
    SettingOption::new("spacious", "", ""),
];

const TOOL_COLLAPSE: &[SettingOption] = &[
    SettingOption::new("compact", "", ""),
    SettingOption::new("expanded", "", ""),
    SettingOption::new("calm", "", ""),
];

const VIM_MODE: &[SettingOption] = &[
    SettingOption::new("normal", "", ""),
    SettingOption::new("vim", "", ""),
];

const MENTION_MENU_BEHAVIOR: &[SettingOption] = &[
    SettingOption::new("fuzzy", "", ""),
    SettingOption::new("browser", "", ""),
];

const WORK_SURFACE_PLACEMENT: &[SettingOption] = &[
    SettingOption::new(
        "top",
        "ConfigChoicePlacementTop",
        "ConfigChoiceDetailPlacementTop",
    ),
    SettingOption::new(
        "bottom",
        "ConfigChoicePlacementBottom",
        "ConfigChoiceDetailPlacementBottom",
    ),
    SettingOption::new(
        "left",
        "ConfigChoicePlacementLeft",
        "ConfigChoiceDetailPlacementLeft",
    ),
    SettingOption::new(
        "right",
        "ConfigChoicePlacementRight",
        "ConfigChoiceDetailPlacementRight",
    ),
    SettingOption::new("off", "ConfigValueOff", "ConfigChoiceDetailPlacementOff"),
];

const RAIL_PANEL: &[SettingOption] = &[
    // The dock's own grammar is lowercase nouns, so the panels the classic
    // sidebar never named ride on their raw value (`RailPanel::title`).
    SettingOption::new(
        "tasks",
        "ConfigChoiceRailTasks",
        "ConfigChoiceDetailRailTasks",
    ),
    SettingOption::new(
        "agents",
        "ConfigChoiceRailAgents",
        "ConfigChoiceDetailRailAgents",
    ),
    SettingOption::new("background", "", ""),
    SettingOption::new("files", "", ""),
    SettingOption::new("notepad", "", ""),
    SettingOption::new(
        "context",
        "ConfigChoiceRailContext",
        "ConfigChoiceDetailRailContext",
    ),
    SettingOption::new("git", "", ""),
    SettingOption::new("price", "", ""),
];

/// Rail tab ids.
pub const TAB_APPEARANCE: &str = "appearance";
pub const TAB_MODELS: &str = "models";
pub const TAB_WORK: &str = "work";
pub const TAB_TOOLS: &str = "tools";
pub const TAB_TRUST: &str = "trust";
pub const TAB_MOTION: &str = "motion";
pub const TAB_ADVANCED: &str = "advanced";

const fn ui(
    tab: &'static str,
    group: &'static str,
    label: &'static str,
    description: &'static str,
) -> Option<SettingUi> {
    Some(SettingUi {
        tab,
        group,
        label,
        description,
        row: SettingRowKind::Setting,
    })
}

/// A row that opens another surface rather than editing a value in place.
const fn ui_action(
    tab: &'static str,
    group: &'static str,
    label: &'static str,
    description: &'static str,
) -> Option<SettingUi> {
    Some(SettingUi {
        tab,
        group,
        label,
        description,
        row: SettingRowKind::Action,
    })
}

/// A read-only receipt row — a managed-policy fact, live route value, or
/// descriptive pointer, never a writable control.
const fn ui_diagnostic(
    tab: &'static str,
    group: &'static str,
    label: &'static str,
    description: &'static str,
) -> Option<SettingUi> {
    Some(SettingUi {
        tab,
        group,
        label,
        description,
        row: SettingRowKind::Diagnostic,
    })
}

/// A row editable for the running session only; it does not persist through
/// the config store.
const fn ui_session(
    tab: &'static str,
    group: &'static str,
    label: &'static str,
    description: &'static str,
) -> Option<SettingUi> {
    Some(SettingUi {
        tab,
        group,
        label,
        description,
        row: SettingRowKind::Session,
    })
}

const fn def(
    key: &'static str,
    kind: SettingKind,
    default: &'static str,
    ui: Option<SettingUi>,
) -> SettingDef {
    SettingDef {
        key,
        kind,
        default,
        ui,
    }
}

/// Every setting the shell knows about, in render order.
pub const SETTINGS_SCHEMA: &[SettingDef] = &[
    // ── appearance ──────────────────────────────────────────────────────
    def(
        "theme",
        SettingKind::String,
        DEFAULT_TUI_THEME,
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelTheme",
            // Described by its shipped value list, not prose (`config_hint_for_key`).
            "",
        ),
    ),
    def(
        "locale",
        SettingKind::String,
        "auto",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelLocale",
            // Described by its shipped value list, not prose (`config_hint_for_key`).
            "",
        ),
    ),
    def(
        "background_color",
        SettingKind::String,
        "",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelBackground",
            "ConfigHintBackgroundColor",
        ),
    ),
    // No sentence anywhere justifies this prototype toggle, so it keeps its
    // declaration and loses its row.
    def(
        "focus_texture",
        SettingKind::Enum(FOCUS_TEXTURE),
        "off",
        None,
    ),
    def(
        "pet_mode",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelPetMode",
            "ConfigHintPetMode",
        ),
    ),
    def(
        "calm_mode",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelCalmMode",
            "ConfigHintCalmMode",
        ),
    ),
    def(
        "show_thinking",
        SettingKind::Bool(SHOW_THINKING),
        "false",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelShowThinking",
            "ConfigHintShowThinking",
        ),
    ),
    def(
        "thinking_default_expanded",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_APPEARANCE,
            "display",
            "",
            "ConfigHintThinkingDefaultExpanded",
        ),
    ),
    def(
        "thinking_preview_lines",
        SettingKind::Int,
        "2",
        ui(
            TAB_APPEARANCE,
            "display",
            "",
            "ConfigHintThinkingPreviewLines",
        ),
    ),
    def(
        "thinking_highlight",
        SettingKind::Bool(THINKING_HIGHLIGHT),
        "true",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelThinkingHighlight",
            "ConfigHintThinkingHighlight",
        ),
    ),
    def(
        "help_expand_groups",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(TAB_APPEARANCE, "display", "", "ConfigHintHelpExpandGroups"),
    ),
    def(
        "contextual_tips",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelContextualTips",
            "ConfigHintContextualTips",
        ),
    ),
    def(
        "pin_last_prompt",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(TAB_APPEARANCE, "display", "", "ConfigHintPinLastPrompt"),
    ),
    def(
        "show_tool_details",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelShowToolDetails",
            "ConfigHintBooleanValues",
        ),
    ),
    def(
        "inline_diffs",
        SettingKind::Enum(INLINE_DIFFS),
        "full",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelInlineDiffs",
            "ConfigHintInlineDiffs",
        ),
    ),
    // No sentence: the glyph set is discoverable from the row's own values.
    def(
        "status_indicator",
        SettingKind::Enum(STATUS_INDICATOR),
        "cw",
        None,
    ),
    def(
        "synchronized_output",
        SettingKind::Enum(SYNCHRONIZED_OUTPUT),
        "auto",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelSynchronizedOutput",
            "ConfigHintSynchronizedOutput",
        ),
    ),
    def(
        "cost_currency",
        SettingKind::Enum(COST_CURRENCY),
        "usd",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelCostCurrency",
            "ConfigHintCostCurrency",
        ),
    ),
    def(
        "transcript_spacing",
        SettingKind::Enum(TRANSCRIPT_SPACING),
        "spacious",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelTranscriptSpacing",
            "ConfigHintDensity",
        ),
    ),
    def(
        "tool_collapse",
        SettingKind::Enum(TOOL_COLLAPSE),
        "compact",
        ui(
            TAB_APPEARANCE,
            "display",
            "ConfigLabelToolCollapse",
            "ConfigHintToolCollapse",
        ),
    ),
    // ── models & providers ──────────────────────────────────────────────
    def(
        "provider",
        SettingKind::String,
        "",
        ui_action(
            TAB_MODELS,
            "provider",
            "ConfigLabelProvider",
            "ConfigHintProvider",
        ),
    ),
    def(
        "model",
        SettingKind::String,
        "",
        ui_action(TAB_MODELS, "model", "ConfigLabelModel", "ConfigHintModel"),
    ),
    def(
        "reasoning_effort",
        SettingKind::Enum(REASONING_EFFORT),
        "",
        ui(
            TAB_MODELS,
            "model",
            "ConfigLabelReasoningEffort",
            "ConfigHintReasoningEffort",
        ),
    ),
    // Sub-agent fan-out depth, next to the model rows that drive it. Fleet
    // membership itself lives in the /fleet menu, so a one-row Fleet tab
    // would only restate this table.
    def(
        "fleet.exec.max_spawn_depth",
        SettingKind::Int,
        "3",
        ui_diagnostic(
            TAB_MODELS,
            "model",
            "ConfigLabelFleetSpawnDepth",
            "ConfigHintFleetMaxSpawnDepth",
        ),
    ),
    // ── work ────────────────────────────────────────────────────────────
    def(
        "composer_density",
        SettingKind::Enum(DENSITY),
        "comfortable",
        ui(
            TAB_WORK,
            "composer",
            "ConfigLabelComposerDensity",
            "ConfigHintDensity",
        ),
    ),
    def(
        "composer_border",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "composer",
            "ConfigLabelComposerBorder",
            "ConfigHintBooleanValues",
        ),
    ),
    def(
        "composer_multiline_mode",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_WORK,
            "composer",
            "ConfigLabelComposerMultilineMode",
            "ConfigHintComposerMultilineMode",
        ),
    ),
    // Settable via `/set`, surfaced through the composer keymap rather than a
    // settings row.
    def(
        "composer_vim_mode",
        SettingKind::Enum(VIM_MODE),
        "normal",
        None,
    ),
    // Terminal protocol toggle; available through `/set` but not given a row.
    def("bracketed_paste", SettingKind::Bool(ON_OFF), "true", None),
    def(
        "paste_burst_detection",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "composer",
            "ConfigLabelPasteBurstDetection",
            "ConfigHintBooleanValues",
        ),
    ),
    // Mention-completion tuning knobs; exposed via `/set`, not the settings row.
    def("mention_menu_limit", SettingKind::Int, "128", None),
    def(
        "mention_menu_behavior",
        SettingKind::Enum(MENTION_MENU_BEHAVIOR),
        "fuzzy",
        None,
    ),
    def("mention_walk_depth", SettingKind::Int, "10", None),
    // Workspace discovery option for symlinked layouts; advanced, `/set`-only.
    def(
        "workspace_follow_symlinks",
        SettingKind::Bool(ON_OFF),
        "false",
        None,
    ),
    def(
        "work_surface_placement",
        SettingKind::Enum(WORK_SURFACE_PLACEMENT),
        "bottom",
        ui(
            TAB_WORK,
            "workbar",
            "ConfigLabelWorkSurfacePlacement",
            "ConfigHintWorkSurfacePlacement",
        ),
    ),
    def(
        "work_surface_top_height",
        SettingKind::Int,
        "8",
        ui(
            TAB_WORK,
            "workbar",
            "ConfigLabelTopHeight",
            "ConfigHintWorkSurfaceTopHeight",
        ),
    ),
    def(
        "work_surface_side_width",
        SettingKind::Int,
        "30",
        ui(
            TAB_WORK,
            "workbar",
            "ConfigLabelSideWidth",
            "ConfigHintWorkSurfaceSideWidth",
        ),
    ),
    def(
        "rail_panel",
        SettingKind::Enum(RAIL_PANEL),
        "tasks",
        ui(TAB_WORK, "workbar", "", "ConfigHintRailPanel"),
    ),
    // Sidebar panel toggles; driven by view actions and startup flags, not rows.
    def("context_panel", SettingKind::Bool(ON_OFF), "false", None),
    def("sessions_rail", SettingKind::Bool(ON_OFF), "false", None),
    def(
        "session_auto_resume",
        SettingKind::Bool(ON_OFF),
        "false",
        None,
    ),
    def(
        "auto_compact",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_WORK,
            "history",
            "ConfigLabelAutoCompact",
            "ConfigHintBooleanValues",
        ),
    ),
    def(
        "auto_compact_threshold_percent",
        SettingKind::Float,
        "80",
        ui(
            TAB_WORK,
            "history",
            "ConfigLabelAutoCompactThreshold",
            "ConfigHintAutoCompactThreshold",
        ),
    ),
    // A computed receipt from auto_compact + threshold; no user-facing row.
    def("effective_auto_compact", SettingKind::String, "", None),
    def(
        "max_history",
        SettingKind::Int,
        "100",
        ui(
            TAB_WORK,
            "history",
            "ConfigLabelMaxHistory",
            "ConfigHintMaxHistory",
        ),
    ),
    def(
        "goal_command",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_WORK,
            "session",
            "ConfigLabelGoalCommand",
            "ConfigHintGoalCommand",
        ),
    ),
    def(
        "workflow",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_WORK,
            "workflow",
            "ConfigLabelWorkflow",
            "ConfigHintWorkflow",
        ),
    ),
    def(
        "notifications.quiet",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationQuiet",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.sound",
        SettingKind::Enum(NOTIFICATION_SOUNDS),
        "legacy",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationSound",
            "ConfigHintNotificationSound",
        ),
    ),
    def(
        "notifications.condition",
        SettingKind::Enum(NOTIFICATION_CONDITIONS),
        "unfocused",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationCondition",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.method",
        SettingKind::Enum(NOTIFICATION_METHODS),
        "auto",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationMethod",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.threshold_secs",
        SettingKind::Int,
        "30",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationThreshold",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.include_summary",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationSummary",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.subagent_completion",
        SettingKind::Enum(NOTIFICATION_SUBAGENTS),
        "final-only",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationSubagents",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.events.turn-complete",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationTurnComplete",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.events.subagent-terminal",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationSubagentTerminal",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.events.approval-needed",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationApprovalNeeded",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.events.input-needed",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationInputNeeded",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.events.elevation-needed",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationElevationNeeded",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.events.model-notify",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationModelNotify",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.completion_sound",
        SettingKind::Enum(NOTIFICATION_COMPLETION_SOUNDS),
        "off",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationCompletionSound",
            "ConfigHintNotificationLegacy",
        ),
    ),
    def(
        "notifications.sound_file",
        SettingKind::String,
        "",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationSoundFile",
            "ConfigHintNotificationSound",
        ),
    ),
    def(
        "notifications.event_sound.enabled",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationEventSoundEnabled",
            "ConfigHintNotificationLegacy",
        ),
    ),
    def(
        "notifications.event_sound.events",
        SettingKind::String,
        "[\"turn-complete\", \"approval-needed\"]",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationEventSoundEvents",
            "ConfigHintNotificationLegacy",
        ),
    ),
    def(
        "notifications.event_sound.min_interval_ms",
        SettingKind::Int,
        "2000",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationEventSoundInterval",
            "ConfigHintNotificationPolicy",
        ),
    ),
    def(
        "notifications.event_sound.quiet",
        SettingKind::Bool(ON_OFF),
        "false",
        ui(
            TAB_WORK,
            "workflow",
            "ConfigLabelNotificationEventSoundQuiet",
            "ConfigHintNotificationLegacy",
        ),
    ),
    // ── tools & MCP ─────────────────────────────────────────────────────
    def(
        "mcp_open",
        SettingKind::String,
        "",
        ui_action(TAB_TOOLS, "mcp", "ConfigLabelMcpOpen", "ConfigHintMcpOpen"),
    ),
    def(
        "mcp_reconnect",
        SettingKind::String,
        "",
        ui_action(
            TAB_TOOLS,
            "mcp",
            "ConfigLabelMcpReconnect",
            "ConfigHintMcpReconnect",
        ),
    ),
    def(
        "mcp_diagnose",
        SettingKind::String,
        "",
        ui_action(
            TAB_TOOLS,
            "mcp",
            "ConfigLabelMcpDiagnose",
            "ConfigHintMcpDiagnose",
        ),
    ),
    def(
        "plugins_open",
        SettingKind::String,
        "",
        ui_action(
            TAB_TOOLS,
            "mcp",
            "ConfigLabelPluginsOpen",
            "ConfigHintPluginsOpen",
        ),
    ),
    def(
        "mcp_config_path",
        SettingKind::String,
        "",
        ui(
            TAB_TOOLS,
            "mcp",
            "ConfigLabelMcpConfigPath",
            "ConfigHintMcpConfigPath",
        ),
    ),
    // ── trust ───────────────────────────────────────────────────────────
    def(
        "sandbox_details",
        SettingKind::String,
        "",
        ui_action(
            TAB_TRUST,
            "permissions",
            "SetupStepTrustSandboxTitle",
            "SetupStepTrustSandboxWhy",
        ),
    ),
    def(
        "approval_mode",
        SettingKind::Enum(APPROVAL_MODE),
        "",
        ui_session(
            TAB_TRUST,
            "permissions",
            "ConfigLabelApprovalMode",
            "ConfigHintApprovalMode",
        ),
    ),
    def(
        "permission_posture",
        SettingKind::Enum(PERMISSION_POSTURE),
        "ask",
        ui(
            TAB_TRUST,
            "permissions",
            "ConfigLabelPermissionPosture",
            "ConfigHintPermissionPosture",
        ),
    ),
    def(
        "approval_policy",
        SettingKind::Enum(APPROVAL_POLICY),
        "ask",
        ui(
            TAB_TRUST,
            "permissions",
            "ConfigLabelApprovalPolicy",
            "ConfigHintApprovalPolicy",
        ),
    ),
    def(
        "managed_approval_policy",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_TRUST,
            "permissions",
            "ConfigLabelManagedApprovalPolicy",
            "ConfigHintManagedApprovalPolicy",
        ),
    ),
    def(
        "default_mode",
        SettingKind::Enum(DEFAULT_MODE),
        "agent",
        ui(
            TAB_TRUST,
            "permissions",
            "ConfigLabelDefaultMode",
            "ConfigHintDefaultMode",
        ),
    ),
    def(
        "allow_shell",
        SettingKind::Bool(ON_OFF),
        "true",
        ui(
            TAB_TRUST,
            "permissions",
            "ConfigLabelAllowShell",
            "ConfigHintAllowShell",
        ),
    ),
    def(
        "managed_allow_shell",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_TRUST,
            "permissions",
            "ConfigLabelManagedAllowShell",
            "ConfigHintManagedAllowShell",
        ),
    ),
    def(
        "telemetry",
        SettingKind::Bool(TELEMETRY),
        "false",
        ui(
            TAB_TRUST,
            "network",
            "ConfigLabelTelemetry",
            "ConfigHintTelemetry",
        ),
    ),
    // ── motion ──────────────────────────────────────────────────────────
    def(
        "low_motion",
        SettingKind::Bool(LOW_MOTION),
        "false",
        ui(
            TAB_MOTION,
            "display",
            "ConfigLabelLowMotion",
            "ConfigHintLowMotion",
        ),
    ),
    def(
        "fancy_animations",
        SettingKind::Bool(FANCY_ANIMATIONS),
        "true",
        ui(
            TAB_MOTION,
            "display",
            "ConfigLabelFancyAnimations",
            "ConfigHintFancyAnimations",
        ),
    ),
    // ── advanced ────────────────────────────────────────────────────────
    def(
        "base_url",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "provider",
            "ConfigLabelBaseUrlDeepseek",
            "ConfigHintBaseUrl",
        ),
    ),
    def(
        "provider_url",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "provider",
            "ConfigLabelProviderUrl",
            "ConfigHintProviderUrl",
        ),
    ),
    def(
        "context_window",
        SettingKind::Int,
        "",
        ui_diagnostic(TAB_ADVANCED, "provider", "", "ConfigHintContextWindow"),
    ),
    def(
        "effective_context_window",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "provider",
            "",
            "ConfigHintEffectiveContextWindow",
        ),
    ),
    // Trust receipts: which external credential file a provider may read, and
    // under what access. Read-only here; `/provider` owns changing them.
    def(
        "external_credentials.openai-codex",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "provider",
            "",
            "ConfigHintExternalCredentials",
        ),
    ),
    def(
        "external_credentials.xai",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "provider",
            "",
            "ConfigHintExternalCredentials",
        ),
    ),
    // Derived fast-sibling receipt, retired from the table: the /model picker
    // already names the fast sibling where a choice actually happens, and no
    // backend reads `fast_model` as a persisted key.
    def("fast_model", SettingKind::String, "", None),
    // A transport timeout; advanced networking, available through `/set` only.
    def("stream_chunk_timeout_secs", SettingKind::Int, "900", None),
    // Canonical transport keys. Runtime resolution stays in TUI Config.
    def("stream.open_timeout_secs", SettingKind::Int, "45", None),
    def("stream.chunk_timeout_secs", SettingKind::Int, "900", None),
    def("stream.max_resumes", SettingKind::Int, "3", None),
    def(
        "stream.max_transparent_retries",
        SettingKind::Int,
        "2",
        None,
    ),
    def("stream.max_stream_errors", SettingKind::Int, "5", None),
    def("stream.max_duration_secs", SettingKind::Int, "1800", None),
    def("stream.max_content_mb", SettingKind::Int, "10", None),
    def("stream.connect_timeout_secs", SettingKind::Int, "30", None),
    def("stream.tcp_keepalive_secs", SettingKind::Int, "30", None),
    def(
        "stream.http2_keep_alive_interval_secs",
        SettingKind::Int,
        "15",
        None,
    ),
    def(
        "stream.http2_keep_alive_timeout_secs",
        SettingKind::Int,
        "20",
        None,
    ),
    def(
        "stream.force_http1",
        SettingKind::Bool(ON_OFF),
        "false",
        None,
    ),
    // DeepSeek-only legacy fallback: the runtime still reads it, but it is
    // not a live choice, so it stays settable through `/set` without a row.
    def("default_model", SettingKind::String, "", None),
    // Beta vision flag: the feature backend stays live, but the row goes —
    // feature state is diagnosed where vision runs, not in Advanced.
    def("features.vision_model", SettingKind::String, "", None),
    def(
        "features.subagents",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "experimental",
            "",
            "ConfigHintFeatureSubagents",
        ),
    ),
    def(
        "features.web_search",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "experimental",
            "",
            "ConfigHintFeatureWebSearch",
        ),
    ),
    def(
        "features.apply_patch",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "experimental",
            "",
            "ConfigHintFeatureApplyPatch",
        ),
    ),
    def(
        "features.mcp",
        SettingKind::String,
        "",
        ui_diagnostic(TAB_ADVANCED, "experimental", "", "ConfigHintFeatureMcp"),
    ),
    def(
        "features.exec_policy",
        SettingKind::String,
        "",
        ui_diagnostic(
            TAB_ADVANCED,
            "experimental",
            "",
            "ConfigHintFeatureExecPolicy",
        ),
    ),
    // ── persisted field names without a row ─────────────────────────────
    // Every field `Settings` serializes to settings.toml has a declaration;
    // these have `ui: None` because their value is owned elsewhere:
    // route pickers, `/set` aliases with their own canonical row, or
    // internal one-way flags. Declaration order is render order, and hidden
    // entries render nothing, so they live together at the end.
    //
    // Canonical names behind a `/set` alias row: `set()` accepts both
    // spellings, the row carries the alias.
    def("tool_collapse_mode", SettingKind::String, "compact", None),
    def("max_input_history", SettingKind::Int, "100", None),
    // Written by the route pickers, not by a settings row (the provider /
    // model rows persist to config.toml via `set_config_value`).
    def("default_provider", SettingKind::String, "", None),
    // Trust posture with a `/set` entry point but no row; surfaced where
    // sandboxing acts, not as a settings sentence.
    def("sandbox_mode", SettingKind::String, "", None),
    // Route memory written by the pickers: per-provider defaults, enabled
    // chooser sets, and pinned routes. Structured values no row could edit.
    def("provider_models", SettingKind::String, "", None),
    def("enabled_models", SettingKind::String, "", None),
    def("pinned_models", SettingKind::String, "", None),
    // One-way internal flags: shown an intro, shown a deprecation, counted
    // tip impressions. Read back to suppress repeats, never edited.
    def(
        "feature_intro_shown",
        SettingKind::Bool(ON_OFF),
        "false",
        None,
    ),
    def(
        "yolo_deprecation_shown",
        SettingKind::Bool(ON_OFF),
        "false",
        None,
    ),
    // Round 3 work-bar placement migration ran once (`top` → `bottom`).
    def(
        "work_surface_bottom_migrated",
        SettingKind::Bool(ON_OFF),
        "false",
        None,
    ),
    def("behavioral_tip_impressions", SettingKind::String, "", None),
    // Footer key-hint use counts: a hint retires to its bare state once its
    // binding has been used enough times. Written by the footer, never edited.
    def("footer_hint_uses", SettingKind::String, "", None),
];

/// The declaration for `key`, if the shell knows it.
pub fn setting(key: &str) -> Option<&'static SettingDef> {
    SETTINGS_SCHEMA.iter().find(|def| def.key == key)
}

/// Position of `key` in declaration order; `None` for unknown keys.
pub fn setting_index(key: &str) -> Option<usize> {
    SETTINGS_SCHEMA.iter().position(|def| def.key == key)
}

/// Distinct `ui.tab` ids, in the order they are first declared.
pub fn schema_tabs() -> Vec<&'static str> {
    let mut tabs: Vec<&'static str> = Vec::new();
    for ui in SETTINGS_SCHEMA.iter().filter_map(|def| def.ui.as_ref()) {
        if !tabs.contains(&ui.tab) {
            tabs.push(ui.tab);
        }
    }
    tabs
}

/// Distinct `ui.group` ids within `tab`, in the order they are first declared.
pub fn schema_groups(tab: &str) -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = Vec::new();
    for ui in SETTINGS_SCHEMA
        .iter()
        .filter_map(|def| def.ui.as_ref())
        .filter(|ui| ui.tab == tab)
    {
        if !groups.contains(&ui.group) {
            groups.push(ui.group);
        }
    }
    groups
}

/// Settings declared with a row, in declaration order.
pub fn schema_rows() -> impl Iterator<Item = &'static SettingDef> {
    SETTINGS_SCHEMA.iter().filter(|def| def.ui.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn schema_keys_are_unique() {
        let mut seen = BTreeSet::new();
        for def in SETTINGS_SCHEMA {
            assert!(seen.insert(def.key), "duplicate setting key: {}", def.key);
        }
        assert_eq!(seen.len(), SETTINGS_SCHEMA.len());
    }

    #[test]
    fn every_row_declares_a_tab_group_and_description() {
        for def in schema_rows() {
            let ui = def.ui.as_ref().expect("schema_rows filters on ui");
            assert!(!ui.tab.is_empty(), "{} has an empty tab", def.key);
            assert!(!ui.group.is_empty(), "{} has an empty group", def.key);
            // theme and locale are described by their shipped value lists,
            // which the TUI composes at render time; no prose key exists.
            if matches!(def.key, "theme" | "locale") {
                continue;
            }
            assert!(
                !ui.description.is_empty(),
                "{} has a row but no sentence justifying it",
                def.key
            );
        }
    }

    #[test]
    fn declaration_order_groups_rows_by_tab_then_group() {
        // The surface renders runs of rows; a tab or group that reappears
        // after another one would paint two headings with the same name.
        let mut seen_tabs: Vec<&str> = Vec::new();
        let mut seen_pairs: Vec<(&str, &str)> = Vec::new();
        let mut current: Option<(&str, &str)> = None;
        for def in schema_rows() {
            let ui = def.ui.as_ref().expect("row");
            if current.map(|(tab, _)| tab) != Some(ui.tab) {
                assert!(!seen_tabs.contains(&ui.tab), "tab {} is split", ui.tab);
                seen_tabs.push(ui.tab);
            }
            if current != Some((ui.tab, ui.group)) {
                assert!(
                    !seen_pairs.contains(&(ui.tab, ui.group)),
                    "group {}/{} is split",
                    ui.tab,
                    ui.group
                );
                seen_pairs.push((ui.tab, ui.group));
            }
            current = Some((ui.tab, ui.group));
        }
        assert_eq!(schema_tabs(), seen_tabs);
    }

    #[test]
    fn enum_values_are_unique_and_defaults_are_declared_values() {
        for def in SETTINGS_SCHEMA {
            if let Some(values) = def.values() {
                let unique: BTreeSet<_> = values.iter().collect();
                assert_eq!(unique.len(), values.len(), "{} repeats a value", def.key);
                if !def.default.is_empty() {
                    assert!(
                        values.contains(&def.default),
                        "{} defaults to {} which is not one of its values",
                        def.key,
                        def.default
                    );
                }
            }
        }
    }
}
