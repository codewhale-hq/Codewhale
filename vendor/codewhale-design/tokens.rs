// Generated from Codewhale design 5.3.0; sha256 0d9acc771562a7b50fc9473e23ebccbff7cbc56962639277943a0cb0131ea83e. Do not edit.
#![allow(dead_code)]

pub const VERSION: &str = "5.3.0";
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub background: u32,
    pub foreground: u32,
    pub surface: u32,
    pub muted_foreground: u32,
    pub disabled: u32,
    pub border: u32,
    pub sidebar: u32,
    pub primary: u32,
    pub primary_foreground: u32,
    pub accent: u32,
    pub hover: u32,
    pub selected: u32,
    pub attention: u32,
    pub live: u32,
    pub danger: u32,
    pub border_strong: u32,
    pub ocean_depth: u32,
    pub diff_add: u32,
    pub diff_remove: u32,
}
pub const DARK: Colors = Colors {
    background: 0x030508,
    foreground: 0xe8edf2,
    surface: 0x04070c,
    muted_foreground: 0xa8b6c3,
    disabled: 0x848583,
    border: 0x3b3c3f,
    sidebar: 0x0d121b,
    primary: 0x0b48bb,
    primary_foreground: 0xffffff,
    accent: 0x8fbdff,
    hover: 0x303134,
    selected: 0x17191c,
    attention: 0xeab982,
    live: 0x9ecbbd,
    danger: 0xeea59d,
    border_strong: 0x828386,
    ocean_depth: 0x202123,
    diff_add: 0x22392f,
    diff_remove: 0x45302f,
};
pub const LIGHT: Colors = Colors {
    background: 0xf1f5f9,
    foreground: 0x1b2a35,
    surface: 0xfbfcfd,
    muted_foreground: 0x4a545d,
    disabled: 0x6f7780,
    border: 0xd5dce3,
    sidebar: 0xf9fafc,
    primary: 0x0b48bb,
    primary_foreground: 0xffffff,
    accent: 0x245bc7,
    hover: 0xe3e9ef,
    selected: 0xd8e0e8,
    attention: 0x84530d,
    live: 0x396554,
    danger: 0x9e3f36,
    border_strong: 0x737d88,
    ocean_depth: 0xf6f9fc,
    diff_add: 0xd2eadd,
    diff_remove: 0xf8e1dd,
};
pub fn colors(dark: bool) -> Colors {
    if dark { DARK } else { LIGHT }
}
pub const WATER_DARK_0: u32 = 0x0b1e3a;
pub const WATER_DARK_35: u32 = 0x071326;
pub const WATER_DARK_75: u32 = 0x04090f;
pub const WATER_DARK_100: u32 = 0x030508;
pub const GLASS_DARK_CLEAR_FILL: u32 = 0x141c28;
pub const GLASS_DARK_CLEAR_FILL_ALPHA: f32 = 0.4;
pub const GLASS_DARK_CLEAR_BLUR_PX: f32 = 12.0;
pub const GLASS_DARK_FROSTED_FILL: u32 = 0x141c28;
pub const GLASS_DARK_FROSTED_FILL_ALPHA: f32 = 0.58;
pub const GLASS_DARK_FROSTED_BLUR_PX: f32 = 24.0;
pub const GLASS_DARK_SOLID_FILL: u32 = 0x141c28;
pub const GLASS_DARK_SOLID_FILL_ALPHA: f32 = 1.0;
pub const GLASS_DARK_SOLID_BLUR_PX: f32 = 0.0;
pub const GLASS_DARK_BORDER: u32 = 0xffffff;
pub const GLASS_DARK_BORDER_ALPHA: f32 = 0.09;
pub const GLASS_DARK_HIGHLIGHT: u32 = 0xffffff;
pub const GLASS_DARK_HIGHLIGHT_ALPHA: f32 = 0.14;
pub const SHEET_DARK_FILL: u32 = 0x05080d;
pub const SHEET_DARK_FILL_ALPHA: f32 = 0.74;
pub const SHEET_DARK_BORDER: u32 = 0xffffff;
pub const SHEET_DARK_BORDER_ALPHA: f32 = 0.05;
pub const CARD_DARK_FILL: u32 = 0xffffff;
pub const CARD_DARK_FILL_ALPHA: f32 = 0.035;
pub const CARD_DARK_BORDER: u32 = 0xffffff;
pub const CARD_DARK_BORDER_ALPHA: f32 = 0.08;
pub const WATER_LIGHT_0: u32 = 0xbfe1f2;
pub const WATER_LIGHT_30: u32 = 0xdceef3;
pub const WATER_LIGHT_62: u32 = 0xe7f0f6;
pub const WATER_LIGHT_100: u32 = 0xf1f5f9;
pub const GLASS_LIGHT_CLEAR_FILL: u32 = 0xffffff;
pub const GLASS_LIGHT_CLEAR_FILL_ALPHA: f32 = 0.4;
pub const GLASS_LIGHT_CLEAR_BLUR_PX: f32 = 12.0;
pub const GLASS_LIGHT_FROSTED_FILL: u32 = 0xffffff;
pub const GLASS_LIGHT_FROSTED_FILL_ALPHA: f32 = 0.55;
pub const GLASS_LIGHT_FROSTED_BLUR_PX: f32 = 24.0;
pub const GLASS_LIGHT_SOLID_FILL: u32 = 0xffffff;
pub const GLASS_LIGHT_SOLID_FILL_ALPHA: f32 = 1.0;
pub const GLASS_LIGHT_SOLID_BLUR_PX: f32 = 0.0;
pub const GLASS_LIGHT_BORDER: u32 = 0xffffff;
pub const GLASS_LIGHT_BORDER_ALPHA: f32 = 0.75;
pub const GLASS_LIGHT_HIGHLIGHT: u32 = 0xffffff;
pub const GLASS_LIGHT_HIGHLIGHT_ALPHA: f32 = 0.75;
pub const SHEET_LIGHT_FILL: u32 = 0xfcfdfe;
pub const SHEET_LIGHT_FILL_ALPHA: f32 = 0.88;
pub const SHEET_LIGHT_BORDER: u32 = 0x1f3340;
pub const SHEET_LIGHT_BORDER_ALPHA: f32 = 0.1;
pub const CARD_LIGHT_FILL: u32 = 0xffffff;
pub const CARD_LIGHT_FILL_ALPHA: f32 = 1.0;
pub const CARD_LIGHT_BORDER: u32 = 0x1f3340;
pub const CARD_LIGHT_BORDER_ALPHA: f32 = 0.1;
pub const FONT_FAMILY: &str = "Geist Sans";
pub const FONT_MONO_FAMILY: &str = "Geist Mono";
pub const FONT_FALLBACKS: &[&str] = &[
    "PingFang SC",
    "Hiragino Sans GB",
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
];
pub const SPACING_HAIRLINE: f32 = 2.0;
pub const SPACING_COMPACT: f32 = 4.0;
pub const SPACING_TIGHT: f32 = 6.0;
pub const SPACING_CONTROL: f32 = 8.0;
pub const SPACING_SNUG: f32 = 10.0;
pub const SPACING_GROUP: f32 = 12.0;
pub const SPACING_SECTION: f32 = 16.0;
pub const SPACING_LARGE: f32 = 24.0;
pub const SPACING_PAGE: f32 = 32.0;
pub const RADIUS_SMALL: f32 = 6.0;
pub const RADIUS_FIELD: f32 = 8.0;
pub const RADIUS_CONTROL: f32 = 10.0;
pub const RADIUS_PANEL: f32 = 18.0;
pub const RADIUS_SHEET: f32 = 18.0;
pub const RADIUS_WINDOW: f32 = 14.0;
pub const RADIUS_CARD: f32 = 14.0;
pub const RADIUS_COMPOSER: f32 = 24.0;
pub const RADIUS_CHIP: f32 = 999.0;
pub const FOCUS_WIDTH: f32 = 2.0;
pub const FOCUS_OFFSET: f32 = 3.0;
pub const ICONS_GRID: f32 = 24.0;
pub const ICONS_STROKE: f32 = 1.7;
pub const MOTION_SPRING_STIFFNESS: f32 = 420.0;
pub const MOTION_SPRING_DAMPING: f32 = 42.0;
pub const MOTION_SPRING_MASS: f32 = 1.0;
pub const MOTION_PET_POLL_MS: f32 = 30.0;
pub const MOTION_REDUCED_POLL_MS: f32 = 500.0;
pub const MOTION_DURATION_STATE_MS: f32 = 120.0;
pub const MOTION_DURATION_ARRIVE_MS: f32 = 180.0;
pub const MOTION_DURATION_EXIT_MS: f32 = 120.0;
pub const MOTION_DURATION_PANEL_MS: f32 = 340.0;
pub const MOTION_DURATION_LINGER_MS: f32 = 1200.0;
pub const MOTION_ARRIVE_RISE_PX: f32 = 4.0;
pub const MOTION_LOOP_BEAT_MS: f32 = 1200.0;
pub const MOTION_LOOP_BREATHE_MS: f32 = 2400.0;
pub const MOTION_LOOP_SPIN_MS: f32 = 1000.0;
pub const MOTION_LOOP_FPS_CAP: f32 = 15.0;
pub const STREAM_REVEAL_FIRST_MS: f32 = 100.0;
pub const STREAM_REVEAL_LAG_MS: f32 = 500.0;
pub const STREAM_RATE_WINDOW_MS: f32 = 1000.0;
pub const STREAM_INSTANT_GRAPHEMES: f32 = 8.0;
pub const STREAM_MAX_PENDING_BYTES: f32 = 16384.0;
pub const GLOW_BLUR_PX: f32 = 6.0;
pub const GLOW_PRIMARY_ALPHA: f32 = 0.12;
pub const GLOW_LIVE_ALPHA: f32 = 0.35;
pub const GLOW_ATTENTION_ALPHA: f32 = 0.4;
pub const ELEVATION_POPOVER_Y_PX: f32 = 4.0;
pub const ELEVATION_POPOVER_BLUR_PX: f32 = 8.0;
pub const ELEVATION_POPOVER_ALPHA_DARK: f32 = 0.4;
pub const ELEVATION_POPOVER_ALPHA_LIGHT: f32 = 0.16;
pub const ELEVATION_DIALOG_Y_PX: f32 = 12.0;
pub const ELEVATION_DIALOG_BLUR_PX: f32 = 32.0;
pub const ELEVATION_DIALOG_ALPHA_DARK: f32 = 0.5;
pub const ELEVATION_DIALOG_ALPHA_LIGHT: f32 = 0.2;
pub const EASING_OUT: [f32; 4] = [0.0, 0.0, 0.2, 1.0];
pub const EASING_IN: [f32; 4] = [0.4, 0.0, 1.0, 1.0];
pub const EASING_PANEL: [f32; 4] = [0.2, 0.1, 0.16, 1.0];
pub const OMBRE_RISE: u32 = 0x1775c9;
pub const OMBRE_DEEP: u32 = 0x0b48bb;
pub const SELECTION_OPACITY: f32 = 0.28;
pub const PRIMARY_HOVER_OPACITY: f32 = 0.9;
pub const MONO_PX: f32 = 13.0;
pub const TYPE_ROLE_CAPTION_SIZE_PX: f32 = 11.5;
pub const TYPE_ROLE_CAPTION_LINE_PX: f32 = 16.0;
pub const TYPE_ROLE_CAPTION_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_LABEL_SIZE_PX: f32 = 12.5;
pub const TYPE_ROLE_LABEL_LINE_PX: f32 = 18.0;
pub const TYPE_ROLE_LABEL_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_CONTROL_SIZE_PX: f32 = 13.0;
pub const TYPE_ROLE_CONTROL_LINE_PX: f32 = 18.0;
pub const TYPE_ROLE_CONTROL_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_BODY_SIZE_PX: f32 = 14.0;
pub const TYPE_ROLE_BODY_LINE_PX: f32 = 22.0;
pub const TYPE_ROLE_BODY_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_PROSE_SIZE_PX: f32 = 16.0;
pub const TYPE_ROLE_PROSE_LINE_PX: f32 = 26.0;
pub const TYPE_ROLE_PROSE_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_HEADING_SIZE_PX: f32 = 15.0;
pub const TYPE_ROLE_HEADING_LINE_PX: f32 = 24.0;
pub const TYPE_ROLE_HEADING_WEIGHT: f32 = 550.0;
pub const TYPE_ROLE_TITLE_SIZE_PX: f32 = 22.0;
pub const TYPE_ROLE_TITLE_LINE_PX: f32 = 28.0;
pub const TYPE_ROLE_TITLE_WEIGHT: f32 = 550.0;
pub const TYPE_ROLE_DISPLAY_SIZE_PX: f32 = 28.0;
pub const TYPE_ROLE_DISPLAY_LINE_PX: f32 = 34.0;
pub const TYPE_ROLE_DISPLAY_WEIGHT: f32 = 550.0;
pub const TYPE_ROLE_CODE_SIZE_PX: f32 = 13.0;
pub const TYPE_ROLE_CODE_LINE_PX: f32 = 20.0;
pub const TYPE_ROLE_CODE_WEIGHT: f32 = 400.0;
pub const SIZE_CONTROL_PX: f32 = 28.0;
pub const SIZE_ROW_PX: f32 = 30.0;
pub const SIZE_TOUCH_TARGET_PX: f32 = 44.0;
pub const SIZE_ICON_PX: f32 = 16.0;
pub const SIZE_ICON_LARGE_PX: f32 = 20.0;
pub const SIZE_SIDEBAR_PX: f32 = 272.0;
pub const SIZE_PANEL_PX: f32 = 300.0;
pub const BREAKPOINTS_COMPACT_PX: f32 = 720.0;
pub const BREAKPOINTS_MEDIUM_PX: f32 = 900.0;
pub const BREAKPOINTS_WIDE_PX: f32 = 1200.0;
pub const TYPE_BODY_PX: f32 = 14.0;
pub const TYPE_CAPTION_PX: f32 = 11.5;
pub const TYPE_PROSE_PX: f32 = 16.0;
pub const TYPE_HEADING_PX: f32 = 15.0;
pub const TYPE_TITLE_PX: f32 = 22.0;
