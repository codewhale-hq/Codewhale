// Generated from vendor/codewhale-design/tokens.json 5.5.0 by tests/generated.rs.
// Do not edit. Regenerate: CODEWHALE_BLESS=1 cargo test --test generated

/// The design tokens version these roles were generated from.
pub const TOKENS_VERSION: &str = "5.5.0";

/// What a color is for. Components name roles; they never name colors.
/// Names follow `tokens.json`, so one vocabulary covers the desktop app,
/// the web and the terminal. Hint is derived from the token colors.
/// See [`Role::is_derived`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// The deepest ground: rails and things put away.
    Sidebar,
    /// The stage.
    Background,
    /// A raised panel.
    Surface,
    /// A row under the pointer.
    Hover,
    /// The selected row.
    Selected,
    /// Body ink.
    Foreground,
    /// Calm non-interactive ink for sub-agent and external-agent tags.
    Subagent,
    /// Ink that recedes: details, verbs in hints, asides.
    Muted,
    /// Quiet lines that separate.
    Border,
    /// Edges that identify a control or a decision.
    BorderStrong,
    /// The primary action fill.
    Primary,
    /// Ink on a `Primary` ground.
    PrimaryForeground,
    /// The one quiet blue: links, focus and selection.
    Accent,
    /// Work happening now, and work done.
    Live,
    /// Crest ink of the live action verb sweep.
    LivePeak,
    /// Needs you.
    Attention,
    /// Failed or destructive.
    Danger,
    /// Ink one step quieter than `Muted`: asides and placeholders. Derived: `muted_foreground` receded toward `background` until it just holds 4.5:1 on every ground.
    Hint,
    /// The quietest ink: disabled and decorative words, never the only carrier of meaning.
    Dim,
    /// Ground behind an added line, under `Foreground` ink: the `diff_add` token.
    DiffAddedTint,
    /// Ground behind a removed line, under `Foreground` ink: the `diff_remove` token.
    DiffRemovedTint,
}

impl Role {
    pub const COUNT: usize = 21;
    pub const ALL: [Role; Self::COUNT] = [
        Role::Sidebar,
        Role::Background,
        Role::Surface,
        Role::Hover,
        Role::Selected,
        Role::Foreground,
        Role::Subagent,
        Role::Muted,
        Role::Border,
        Role::BorderStrong,
        Role::Primary,
        Role::PrimaryForeground,
        Role::Accent,
        Role::Live,
        Role::LivePeak,
        Role::Attention,
        Role::Danger,
        Role::Hint,
        Role::Dim,
        Role::DiffAddedTint,
        Role::DiffRemovedTint,
    ];

    /// The `tokens.json` key this role reads, or `None` for a role the
    /// kit derives from other tokens ([`Role::is_derived`]).
    #[must_use]
    pub const fn token_name(self) -> Option<&'static str> {
        match self {
            Role::Sidebar => Some("sidebar"),
            Role::Background => Some("background"),
            Role::Surface => Some("surface"),
            Role::Hover => Some("hover"),
            Role::Selected => Some("selected"),
            Role::Foreground => Some("foreground"),
            Role::Subagent => Some("subagent"),
            Role::Muted => Some("muted_foreground"),
            Role::Border => Some("border"),
            Role::BorderStrong => Some("border_strong"),
            Role::Primary => Some("primary"),
            Role::PrimaryForeground => Some("primary_foreground"),
            Role::Accent => Some("accent"),
            Role::Live => Some("live"),
            Role::LivePeak => Some("live_peak"),
            Role::Attention => Some("attention"),
            Role::Danger => Some("danger"),
            Role::Hint => None,
            Role::Dim => Some("disabled"),
            Role::DiffAddedTint => Some("diff_add"),
            Role::DiffRemovedTint => Some("diff_remove"),
        }
    }

    /// Position in [`Role::ALL`] and in the color tables.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Whether the kit derives this role from other tokens, because
    /// `tokens.json` does not name it.
    #[must_use]
    pub const fn is_derived(self) -> bool {
        matches!(self, Role::Hint)
    }
}

/// dark token colors, `0xRRGGBB`, indexed by [`Role::index`].
pub(crate) const DARK: [u32; Role::COUNT] = [
    0x0f131c, // Sidebar
    0x05070b, // Background
    0x080b11, // Surface
    0x202938, // Hover
    0x18202e, // Selected
    0xedf1f7, // Foreground
    0xbccad9, // Subagent
    0xb2bfcd, // Muted
    0x263040, // Border
    0x8a97a8, // BorderStrong
    0x0b4cc4, // Primary
    0xffffff, // PrimaryForeground
    0x94beff, // Accent
    0x7fdfc8, // Live
    0xe3fcf4, // LivePeak
    0xf0be82, // Attention
    0xf4a8a0, // Danger
    0x8c97a2, // Hint
    0x828f9e, // Dim
    0x1d352a, // DiffAddedTint
    0x422b2c, // DiffRemovedTint
];

/// dark xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - dark hint: 246 -> 247 (nearest failed its contrast floor)
/// - dark border_strong: 103 -> 246 (nearest shared an index with another ink)
/// - dark diff_add: 235 -> 23 (the cube has no tint this faint; nearest index that keeps the tint's hue)
/// - dark diff_remove: 236 -> 52 (the cube has no tint this faint; nearest index that keeps the tint's hue)
pub(crate) const DARK_256: [u8; Role::COUNT] = [
    233, // Sidebar
    232, // Background
    232, // Surface
    236, // Hover
    235, // Selected
    255, // Foreground
    152, // Subagent
    146, // Muted
    236, // Border
    246, // BorderStrong
    26, // Primary
    231, // PrimaryForeground
    111, // Accent
    116, // Live
    195, // LivePeak
    216, // Attention
    217, // Danger
    247, // Hint
    103, // Dim
    23, // DiffAddedTint
    52, // DiffRemovedTint
];

/// light token colors, `0xRRGGBB`, indexed by [`Role::index`].
pub(crate) const LIGHT: [u32; Role::COUNT] = [
    0xfcfdfe, // Sidebar
    0xf4f7fc, // Background
    0xfefeff, // Surface
    0xe2eaf4, // Hover
    0xd6e1f0, // Selected
    0x162230, // Foreground
    0x374553, // Subagent
    0x45515f, // Muted
    0xd5dde8, // Border
    0x687381, // BorderStrong
    0x0b4cc4, // Primary
    0xffffff, // PrimaryForeground
    0x1552c6, // Accent
    0x146857, // Live
    0x0b3f33, // LivePeak
    0x7b4b06, // Attention
    0xa42531, // Danger
    0x586370, // Hint
    0x68727f, // Dim
    0xd2ebdd, // DiffAddedTint
    0xf8e1dd, // DiffRemovedTint
];

/// light xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - light accent: 26 -> 25 (nearest failed its contrast floor)
/// - light live_peak: 235 -> 236 (nearest shared an index with another ink)
/// - light attention: 94 -> 58 (nearest failed its contrast floor)
/// - light hint: 241 -> 59 (nearest failed its contrast floor)
/// - light border_strong: 243 -> 242 (nearest shared an index with another ink)
/// - light diff_add: 253 -> 158 (the cube has no tint this faint; nearest index that keeps the tint's hue)
/// - light diff_remove: 224 -> 224 (the cube has no tint this faint; nearest index that keeps the tint's hue)
pub(crate) const LIGHT_256: [u8; Role::COUNT] = [
    231, // Sidebar
    231, // Background
    231, // Surface
    255, // Hover
    189, // Selected
    235, // Foreground
    238, // Subagent
    239, // Muted
    253, // Border
    242, // BorderStrong
    26, // Primary
    231, // PrimaryForeground
    25, // Accent
    23, // Live
    236, // LivePeak
    58, // Attention
    125, // Danger
    59, // Hint
    243, // Dim
    158, // DiffAddedTint
    224, // DiffRemovedTint
];

/// The blue ombre (`Ground::Ocean`): the dark table with its grounds and
/// quiet line tinted 0.5 toward `LOGO_BOTTOM` at their own luminance,
/// so every contrast floor holds. The diff tints are re-mixed over its own
/// background. Truecolor only; 256 colors use `DARK_256`.
pub(crate) const OCEAN: [u32; Role::COUNT] = [
    0x031131, // Sidebar
    0x01061a, // Background
    0x010923, // Surface
    0x0d2656, // Hover
    0x071e48, // Selected
    0xedf1f7, // Foreground
    0xbccad9, // Subagent
    0xb2bfcd, // Muted
    0x112d62, // Border
    0x8a97a8, // BorderStrong
    0x0b4cc4, // Primary
    0xffffff, // PrimaryForeground
    0x94beff, // Accent
    0x7fdfc8, // Live
    0xe3fcf4, // LivePeak
    0xf0be82, // Attention
    0xf4a8a0, // Danger
    0x8c97a2, // Hint
    0x828f9e, // Dim
    0x152936, // DiffAddedTint
    0x28202f, // DiffRemovedTint
];
