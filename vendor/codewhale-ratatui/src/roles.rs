// Generated from vendor/codewhale-design/tokens.json 5.0.0 by tests/generated.rs.
// Do not edit. Regenerate: CODEWHALE_BLESS=1 cargo test --test generated

/// The design tokens version these roles were generated from.
pub const TOKENS_VERSION: &str = "5.0.0";

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
    pub const COUNT: usize = 19;
    pub const ALL: [Role; Self::COUNT] = [
        Role::Sidebar,
        Role::Background,
        Role::Surface,
        Role::Hover,
        Role::Selected,
        Role::Foreground,
        Role::Muted,
        Role::Border,
        Role::BorderStrong,
        Role::Primary,
        Role::PrimaryForeground,
        Role::Accent,
        Role::Live,
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
            Role::Muted => Some("muted_foreground"),
            Role::Border => Some("border"),
            Role::BorderStrong => Some("border_strong"),
            Role::Primary => Some("primary"),
            Role::PrimaryForeground => Some("primary_foreground"),
            Role::Accent => Some("accent"),
            Role::Live => Some("live"),
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
    0x303134, // Hover
    0x17191c, // Selected
    0xe8edf2, // Foreground
    0xa8b6c3, // Muted
    0x3b3c3f, // Border
    0x828386, // BorderStrong
    0x0b48bb, // Primary
    0xffffff, // PrimaryForeground
    0x8fbdff, // Accent
    0x9ecbbd, // Live
    0xeab982, // Attention
    0xeea59d, // Danger
    0x8e9aa5, // Hint
    0x848583, // Dim
    0x22392f, // DiffAddedTint
    0x45302f, // DiffRemovedTint
];

/// dark xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - dark live: 151 -> 115 (nearest lost its hue)
/// - dark border_strong: 102 -> 244 (nearest shared an index with another ink)
/// - dark diff_add: 236 -> 22 (the cube has no tint this faint; nearest index that keeps the tint's hue)
/// - dark diff_remove: 237 -> 52 (the cube has no tint this faint; nearest index that keeps the tint's hue)
pub(crate) const DARK_256: [u8; Role::COUNT] = [
    233, // Sidebar
    232, // Background
    232, // Surface
    236, // Hover
    234, // Selected
    255, // Foreground
    249, // Muted
    237, // Border
    244, // BorderStrong
    25, // Primary
    231, // PrimaryForeground
    111, // Accent
    115, // Live
    180, // Attention
    217, // Danger
    247, // Hint
    102, // Dim
    22, // DiffAddedTint
    52, // DiffRemovedTint
];

/// light token colors, `0xRRGGBB`, indexed by [`Role::index`].
pub(crate) const LIGHT: [u32; Role::COUNT] = [
    0xfbfaf6, // Sidebar
    0xf7f4ec, // Background
    0xfcfbf8, // Surface
    0xe8e5e0, // Hover
    0xdfdcd6, // Selected
    0x1b2a35, // Foreground
    0x4a545d, // Muted
    0xd9d5cf, // Border
    0x807c76, // BorderStrong
    0x0b48bb, // Primary
    0xffffff, // PrimaryForeground
    0x245bc7, // Accent
    0x396554, // Live
    0x84530d, // Attention
    0x9e3f36, // Danger
    0x5a626a, // Hint
    0x7a7a77, // Dim
    0xd2eadd, // DiffAddedTint
    0xf8e1dd, // DiffRemovedTint
];

/// light xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - light accent: 26 -> 25 (nearest failed its contrast floor)
/// - light live: 239 -> 23 (nearest lost its hue)
/// - light attention: 94 -> 58 (nearest failed its contrast floor)
/// - light danger: 131 -> 124 (nearest failed its contrast floor)
/// - light hint: 241 -> 59 (nearest failed its contrast floor)
/// - light border_strong: 244 -> 242 (nearest failed its contrast floor)
/// - light diff_add: 253 -> 158 (the cube has no tint this faint; nearest index that keeps the tint's hue)
/// - light diff_remove: 224 -> 224 (the cube has no tint this faint; nearest index that keeps the tint's hue)
pub(crate) const LIGHT_256: [u8; Role::COUNT] = [
    231, // Sidebar
    255, // Background
    231, // Surface
    254, // Hover
    253, // Selected
    235, // Foreground
    240, // Muted
    188, // Border
    242, // BorderStrong
    25, // Primary
    231, // PrimaryForeground
    25, // Accent
    23, // Live
    58, // Attention
    124, // Danger
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
    0x162f60, // Hover
    0x05173a, // Selected
    0xe8edf2, // Foreground
    0xa8b6c3, // Muted
    0x1e3a70, // Border
    0x828386, // BorderStrong
    0x0b48bb, // Primary
    0xffffff, // PrimaryForeground
    0x8fbdff, // Accent
    0x9ecbbd, // Live
    0xeab982, // Attention
    0xeea59d, // Danger
    0x8e9aa5, // Hint
    0x848583, // Dim
    0x192430, // DiffAddedTint
    0x261e2b, // DiffRemovedTint
];
