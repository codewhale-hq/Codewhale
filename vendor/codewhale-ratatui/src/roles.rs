// Generated from vendor/codewhale-design/tokens.json 5.4.0 by tests/generated.rs.
// Do not edit. Regenerate: CODEWHALE_BLESS=1 cargo test --test generated

/// The design tokens version these roles were generated from.
pub const TOKENS_VERSION: &str = "5.4.0";

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
    0x0d121b, // Sidebar
    0x030508, // Background
    0x04070c, // Surface
    0x253244, // Hover
    0x182232, // Selected
    0xe8edf2, // Foreground
    0xb8c7d6, // Subagent
    0xaebcc9, // Muted
    0x2a3648, // Border
    0x8a98a6, // BorderStrong
    0x0b48bb, // Primary
    0xffffff, // PrimaryForeground
    0x8fbdff, // Accent
    0x86d3be, // Live
    0xdff7f0, // LivePeak
    0xeab982, // Attention
    0xeea59d, // Danger
    0x8f9ba6, // Hint
    0x7f8c9a, // Dim
    0x22392f, // DiffAddedTint
    0x45302f, // DiffRemovedTint
];

/// dark xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - dark live_peak: 255 -> 195 (nearest shared an index with another ink)
/// - dark diff_add: 236 -> 22 (the cube has no tint this faint; nearest index that keeps the tint's hue)
/// - dark diff_remove: 237 -> 52 (the cube has no tint this faint; nearest index that keeps the tint's hue)
pub(crate) const DARK_256: [u8; Role::COUNT] = [
    233, // Sidebar
    232, // Background
    232, // Surface
    236, // Hover
    235, // Selected
    255, // Foreground
    152, // Subagent
    250, // Muted
    237, // Border
    103, // BorderStrong
    25, // Primary
    231, // PrimaryForeground
    111, // Accent
    115, // Live
    195, // LivePeak
    180, // Attention
    217, // Danger
    247, // Hint
    245, // Dim
    22, // DiffAddedTint
    52, // DiffRemovedTint
];

/// light token colors, `0xRRGGBB`, indexed by [`Role::index`].
pub(crate) const LIGHT: [u32; Role::COUNT] = [
    0xf9fafc, // Sidebar
    0xf1f5f9, // Background
    0xfbfcfd, // Surface
    0xdde6f0, // Hover
    0xd2dde9, // Selected
    0x1b2a35, // Foreground
    0x3b4a56, // Subagent
    0x47515a, // Muted
    0xd5dce3, // Border
    0x6e7883, // BorderStrong
    0x0b48bb, // Primary
    0xffffff, // PrimaryForeground
    0x2056c2, // Accent
    0x1b695a, // Live
    0x0d4035, // LivePeak
    0x7d4e08, // Attention
    0xa32934, // Danger
    0x58616a, // Hint
    0x6f7780, // Dim
    0xd2eadd, // DiffAddedTint
    0xf8e1dd, // DiffRemovedTint
];

/// light xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - light muted_foreground: 239 -> 240 (nearest shared an index with another ink)
/// - light attention: 94 -> 58 (nearest failed its contrast floor)
/// - light hint: 241 -> 59 (nearest failed its contrast floor)
/// - light border_strong: 243 -> 242 (nearest shared an index with another ink)
/// - light diff_add: 253 -> 158 (the cube has no tint this faint; nearest index that keeps the tint's hue)
/// - light diff_remove: 224 -> 224 (the cube has no tint this faint; nearest index that keeps the tint's hue)
pub(crate) const LIGHT_256: [u8; Role::COUNT] = [
    231, // Sidebar
    255, // Background
    231, // Surface
    254, // Hover
    253, // Selected
    235, // Foreground
    239, // Subagent
    240, // Muted
    253, // Border
    242, // BorderStrong
    25, // Primary
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
    0x021030, // Sidebar
    0x000415, // Background
    0x01061a, // Surface
    0x112f65, // Hover
    0x081f4c, // Selected
    0xe8edf2, // Foreground
    0xb8c7d6, // Subagent
    0xaebcc9, // Muted
    0x15336c, // Border
    0x8a98a6, // BorderStrong
    0x0b48bb, // Primary
    0xffffff, // PrimaryForeground
    0x8fbdff, // Accent
    0x86d3be, // Live
    0xdff7f0, // LivePeak
    0xeab982, // Attention
    0xeea59d, // Danger
    0x8f9ba6, // Hint
    0x7f8c9a, // Dim
    0x152530, // DiffAddedTint
    0x261e2b, // DiffRemovedTint
];
