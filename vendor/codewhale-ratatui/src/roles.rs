// Generated from vendor/codewhale-design/tokens.json 2.3.0 by tests/generated.rs.
// Do not edit. Regenerate: CODEWHALE_BLESS=1 cargo test --test generated

/// The design tokens version these roles were generated from.
pub const TOKENS_VERSION: &str = "2.3.0";

/// What a color is for. Components name roles; they never name colors.
/// Names follow `tokens.json`, so one vocabulary covers the desktop app,
/// the web and the terminal. Two roles the tokens do not name are
/// derived from ones they do ([`Role::is_derived`]).
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
    /// Filled actions and switches, in neutral ink.
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
    /// The quietest ink: disabled and decorative words, never the only carrier of meaning. Derived: `muted_foreground` receded toward `background` until it just holds 3:1 on every ground.
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
            Role::Dim => None,
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
        matches!(self, Role::Hint | Role::Dim)
    }
}

/// dark token colors, `0xRRGGBB`, indexed by [`Role::index`].
pub(crate) const DARK: [u32; Role::COUNT] = [
    0x191a1c, // Sidebar
    0x202123, // Background
    0x2a2b2e, // Surface
    0x303134, // Hover
    0x37393d, // Selected
    0xefeeeb, // Foreground
    0xb8b8b4, // Muted
    0x3b3c3f, // Border
    0x828386, // BorderStrong
    0xe6e4df, // Primary
    0x202123, // PrimaryForeground
    0xa9c0de, // Accent
    0x9ec7b2, // Live
    0xe8b077, // Attention
    0xe39a90, // Danger
    0xa3a3a0, // Hint
    0x838381, // Dim
    0x22392f, // DiffAddedTint
    0x45302f, // DiffRemovedTint
];

/// dark xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - dark primary: 254 -> 230 (nearest lost its hue)
/// - dark danger: 174 -> 210 (nearest failed its contrast floor)
/// - dark hint: 247 -> 248 (nearest failed its contrast floor)
/// - dark dim: 244 -> 245 (nearest failed its contrast floor)
/// - dark diff_add: 236 -> 22 (the cube has no tint this faint; nearest index that keeps the hue of live)
/// - dark diff_remove: 237 -> 52 (the cube has no tint this faint; nearest index that keeps the hue of danger)
pub(crate) const DARK_256: [u8; Role::COUNT] = [
    234, // Sidebar
    235, // Background
    236, // Surface
    236, // Hover
    237, // Selected
    255, // Foreground
    249, // Muted
    237, // Border
    102, // BorderStrong
    230, // Primary
    235, // PrimaryForeground
    146, // Accent
    151, // Live
    180, // Attention
    210, // Danger
    248, // Hint
    245, // Dim
    22, // DiffAddedTint
    52, // DiffRemovedTint
];

/// light token colors, `0xRRGGBB`, indexed by [`Role::index`].
pub(crate) const LIGHT: [u32; Role::COUNT] = [
    0xf0ede8, // Sidebar
    0xfaf8f5, // Background
    0xffffff, // Surface
    0xe8e5e0, // Hover
    0xdfdcd6, // Selected
    0x28292b, // Foreground
    0x4f504d, // Muted
    0xd9d5cf, // Border
    0x807c76, // BorderStrong
    0x2e2f31, // Primary
    0xfaf8f5, // PrimaryForeground
    0x425c82, // Accent
    0x3a6352, // Live
    0x86520d, // Attention
    0x9e3f36, // Danger
    0x60615e, // Hint
    0x7d7d7a, // Dim
    0xd2eadd, // DiffAddedTint
    0xf8e1dd, // DiffRemovedTint
];

/// light xterm 256-color indices (16..=255 only; 0..=15 belong to the
/// user's profile). Nearest index, except where contrast needed a move:
/// - light primary: 236 -> 17 (nearest lost its hue)
/// - light accent: 60 -> 240 (nearest failed its contrast floor)
/// - light live: 239 -> 23 (nearest lost its hue)
/// - light attention: 94 -> 58 (nearest failed its contrast floor)
/// - light danger: 131 -> 124 (nearest failed its contrast floor)
/// - light dim: 244 -> 243 (nearest failed its contrast floor)
/// - light border_strong: 244 -> 242 (nearest failed its contrast floor)
/// - light diff_add: 253 -> 158 (the cube has no tint this faint; nearest index that keeps the hue of live)
/// - light diff_remove: 224 -> 224 (the cube has no tint this faint; nearest index that keeps the hue of danger)
pub(crate) const LIGHT_256: [u8; Role::COUNT] = [
    255, // Sidebar
    231, // Background
    231, // Surface
    254, // Hover
    253, // Selected
    235, // Foreground
    239, // Muted
    188, // Border
    242, // BorderStrong
    17, // Primary
    231, // PrimaryForeground
    240, // Accent
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
    0x06183c, // Sidebar
    0x0a1f47, // Background
    0x122957, // Surface
    0x162f60, // Hover
    0x1b376b, // Selected
    0xefeeeb, // Foreground
    0xb8b8b4, // Muted
    0x1e3a70, // Border
    0x828386, // BorderStrong
    0xe6e4df, // Primary
    0x202123, // PrimaryForeground
    0xa9c0de, // Accent
    0x9ec7b2, // Live
    0xe8b077, // Attention
    0xe39a90, // Danger
    0xa3a3a0, // Hint
    0x838381, // Dim
    0x223a58, // DiffAddedTint
    0x2d3353, // DiffRemovedTint
];
