// Generated from Codewhale design 5.5.0; sha256 b154b28c796626428f20b38db59ee6449aa876831ae04d4ddceb3cb1fce8185c. Do not edit.
#![allow(dead_code)]

pub const VERSION: &str = "5.5.0";
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub background: u32,
    pub foreground: u32,
    pub surface: u32,
    pub muted_foreground: u32,
    pub subagent: u32,
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
    pub live_peak: u32,
    pub danger: u32,
    pub border_strong: u32,
    pub ocean_depth: u32,
    pub diff_add: u32,
    pub diff_remove: u32,
}
pub const DARK: Colors = Colors {
    background: 0x05070b,
    foreground: 0xedf1f7,
    surface: 0x080b11,
    muted_foreground: 0xb2bfcd,
    subagent: 0xbccad9,
    disabled: 0x828f9e,
    border: 0x263040,
    sidebar: 0x0f131c,
    primary: 0x0b4cc4,
    primary_foreground: 0xffffff,
    accent: 0x94beff,
    hover: 0x202938,
    selected: 0x18202e,
    attention: 0xf0be82,
    live: 0x7fdfc8,
    live_peak: 0xe3fcf4,
    danger: 0xf4a8a0,
    border_strong: 0x8a97a8,
    ocean_depth: 0x0b111c,
    diff_add: 0x1d352a,
    diff_remove: 0x422b2c,
};
pub const LIGHT: Colors = Colors {
    background: 0xf4f7fc,
    foreground: 0x162230,
    surface: 0xfefeff,
    muted_foreground: 0x45515f,
    subagent: 0x374553,
    disabled: 0x68727f,
    border: 0xd5dde8,
    sidebar: 0xfcfdfe,
    primary: 0x0b4cc4,
    primary_foreground: 0xffffff,
    accent: 0x1552c6,
    hover: 0xe2eaf4,
    selected: 0xd6e1f0,
    attention: 0x7b4b06,
    live: 0x146857,
    live_peak: 0x0b3f33,
    danger: 0xa42531,
    border_strong: 0x687381,
    ocean_depth: 0xf7f9fc,
    diff_add: 0xd2ebdd,
    diff_remove: 0xf8e1dd,
};
pub fn colors(dark: bool) -> Colors {
    if dark { DARK } else { LIGHT }
}
pub const WATER_DARK_0: u32 = 0x0d1422;
pub const WATER_DARK_36: u32 = 0x090e19;
pub const WATER_DARK_74: u32 = 0x060910;
pub const WATER_DARK_100: u32 = 0x05070b;
pub const GLASS_DARK_CLEAR_FILL: u32 = 0x141a25;
pub const GLASS_DARK_CLEAR_FILL_ALPHA: f32 = 0.42;
pub const GLASS_DARK_CLEAR_BLUR_PX: f32 = 14.0;
pub const GLASS_DARK_FROSTED_FILL: u32 = 0x141a25;
pub const GLASS_DARK_FROSTED_FILL_ALPHA: f32 = 0.64;
pub const GLASS_DARK_FROSTED_BLUR_PX: f32 = 28.0;
pub const GLASS_DARK_SOLID_FILL: u32 = 0x141a25;
pub const GLASS_DARK_SOLID_FILL_ALPHA: f32 = 1.0;
pub const GLASS_DARK_SOLID_BLUR_PX: f32 = 0.0;
pub const GLASS_DARK_BORDER: u32 = 0xffffff;
pub const GLASS_DARK_BORDER_ALPHA: f32 = 0.1;
pub const GLASS_DARK_HIGHLIGHT: u32 = 0xffffff;
pub const GLASS_DARK_HIGHLIGHT_ALPHA: f32 = 0.16;
pub const SHEET_DARK_FILL: u32 = 0x090c13;
pub const SHEET_DARK_FILL_ALPHA: f32 = 0.8;
pub const SHEET_DARK_BORDER: u32 = 0xffffff;
pub const SHEET_DARK_BORDER_ALPHA: f32 = 0.075;
pub const CARD_DARK_FILL: u32 = 0xffffff;
pub const CARD_DARK_FILL_ALPHA: f32 = 0.04;
pub const CARD_DARK_BORDER: u32 = 0xffffff;
pub const CARD_DARK_BORDER_ALPHA: f32 = 0.095;
pub const CHANGE_BAR_DARK_FILL: u32 = 0xffffff;
pub const CHANGE_BAR_DARK_FILL_ALPHA: f32 = 0.045;
pub const CHANGE_BAR_DARK_BORDER: u32 = 0xffffff;
pub const CHANGE_BAR_DARK_BORDER_ALPHA: f32 = 0.1;
pub const SUBAGENT_TAG_DARK_FILL: u32 = 0x94beff;
pub const SUBAGENT_TAG_DARK_FILL_ALPHA: f32 = 0.09;
pub const SUBAGENT_TAG_DARK_BORDER: u32 = 0x94beff;
pub const SUBAGENT_TAG_DARK_BORDER_ALPHA: f32 = 0.24;
pub const WORKBAR_DARK_FILL: u32 = 0x0e131d;
pub const WORKBAR_DARK_FILL_ALPHA: f32 = 0.74;
pub const WORKBAR_DARK_BORDER: u32 = 0xffffff;
pub const WORKBAR_DARK_BORDER_ALPHA: f32 = 0.09;
pub const WORKBAR_DARK_TRACK: u32 = 0xffffff;
pub const WORKBAR_DARK_TRACK_ALPHA: f32 = 0.13;
pub const WATER_LIGHT_0: u32 = 0xe2ebf7;
pub const WATER_LIGHT_32: u32 = 0xe9f0f9;
pub const WATER_LIGHT_65: u32 = 0xeff4fb;
pub const WATER_LIGHT_100: u32 = 0xf4f7fc;
pub const GLASS_LIGHT_CLEAR_FILL: u32 = 0xffffff;
pub const GLASS_LIGHT_CLEAR_FILL_ALPHA: f32 = 0.48;
pub const GLASS_LIGHT_CLEAR_BLUR_PX: f32 = 14.0;
pub const GLASS_LIGHT_FROSTED_FILL: u32 = 0xffffff;
pub const GLASS_LIGHT_FROSTED_FILL_ALPHA: f32 = 0.72;
pub const GLASS_LIGHT_FROSTED_BLUR_PX: f32 = 28.0;
pub const GLASS_LIGHT_SOLID_FILL: u32 = 0xffffff;
pub const GLASS_LIGHT_SOLID_FILL_ALPHA: f32 = 1.0;
pub const GLASS_LIGHT_SOLID_BLUR_PX: f32 = 0.0;
pub const GLASS_LIGHT_BORDER: u32 = 0x182838;
pub const GLASS_LIGHT_BORDER_ALPHA: f32 = 0.11;
pub const GLASS_LIGHT_HIGHLIGHT: u32 = 0xffffff;
pub const GLASS_LIGHT_HIGHLIGHT_ALPHA: f32 = 0.88;
pub const SHEET_LIGHT_FILL: u32 = 0xffffff;
pub const SHEET_LIGHT_FILL_ALPHA: f32 = 0.92;
pub const SHEET_LIGHT_BORDER: u32 = 0x182838;
pub const SHEET_LIGHT_BORDER_ALPHA: f32 = 0.09;
pub const CARD_LIGHT_FILL: u32 = 0xffffff;
pub const CARD_LIGHT_FILL_ALPHA: f32 = 1.0;
pub const CARD_LIGHT_BORDER: u32 = 0x182838;
pub const CARD_LIGHT_BORDER_ALPHA: f32 = 0.1;
pub const CHANGE_BAR_LIGHT_FILL: u32 = 0x1b2a35;
pub const CHANGE_BAR_LIGHT_FILL_ALPHA: f32 = 0.035;
pub const CHANGE_BAR_LIGHT_BORDER: u32 = 0x1f3340;
pub const CHANGE_BAR_LIGHT_BORDER_ALPHA: f32 = 0.11;
pub const SUBAGENT_TAG_LIGHT_FILL: u32 = 0x1b2a35;
pub const SUBAGENT_TAG_LIGHT_FILL_ALPHA: f32 = 0.055;
pub const SUBAGENT_TAG_LIGHT_BORDER: u32 = 0x1f3340;
pub const SUBAGENT_TAG_LIGHT_BORDER_ALPHA: f32 = 0.14;
pub const WORKBAR_LIGHT_FILL: u32 = 0xeef3f8;
pub const WORKBAR_LIGHT_FILL_ALPHA: f32 = 0.78;
pub const WORKBAR_LIGHT_BORDER: u32 = 0x1f3340;
pub const WORKBAR_LIGHT_BORDER_ALPHA: f32 = 0.1;
pub const WORKBAR_LIGHT_TRACK: u32 = 0x1b2a35;
pub const WORKBAR_LIGHT_TRACK_ALPHA: f32 = 0.11;
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
pub const SPACING_PROSE: f32 = 14.0;
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
pub const MOTION_DURATION_FLASH_IN_MS: f32 = 90.0;
pub const MOTION_ARRIVE_RISE_PX: f32 = 4.0;
pub const MOTION_LOOP_BEAT_MS: f32 = 1200.0;
pub const MOTION_LOOP_BREATHE_MS: f32 = 2400.0;
pub const MOTION_LOOP_SPIN_MS: f32 = 1000.0;
pub const MOTION_LOOP_FPS_CAP: f32 = 15.0;
pub const MOTION_SWEEP_ACTIVE_MS: f32 = 1800.0;
pub const MOTION_SWEEP_REST_MS: f32 = 600.0;
pub const MOTION_SWEEP_DELAY_MS: f32 = 400.0;
pub const MOTION_SWEEP_WIDTH_CELLS: f32 = 9.0;
pub const MOTION_SWEEP_STRENGTH: f32 = 0.78;
pub const MOTION_BREATHE_MIN_ALPHA: f32 = 0.18;
pub const MOTION_BREATHE_MAX_ALPHA: f32 = 0.55;
pub const MOTION_FLASH_EARN_MS: f32 = 1000.0;
pub const MOTION_STALL_QUIET_MS: f32 = 30000.0;
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
pub const EASING_SINE: [f32; 4] = [0.37, 0.0, 0.63, 1.0];
pub const OMBRE_RISE: u32 = 0x176fd0;
pub const OMBRE_DEEP: u32 = 0x0b4cc4;
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
pub const TYPE_ROLE_CONTROL_LINE_PX: f32 = 20.0;
pub const TYPE_ROLE_CONTROL_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_BODY_SIZE_PX: f32 = 14.0;
pub const TYPE_ROLE_BODY_LINE_PX: f32 = 22.0;
pub const TYPE_ROLE_BODY_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_READING_SIZE_PX: f32 = 15.0;
pub const TYPE_ROLE_READING_LINE_PX: f32 = 25.0;
pub const TYPE_ROLE_READING_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_PROSE_SIZE_PX: f32 = 16.0;
pub const TYPE_ROLE_PROSE_LINE_PX: f32 = 27.0;
pub const TYPE_ROLE_PROSE_WEIGHT: f32 = 400.0;
pub const TYPE_ROLE_HEADING_SIZE_PX: f32 = 15.0;
pub const TYPE_ROLE_HEADING_LINE_PX: f32 = 22.0;
pub const TYPE_ROLE_HEADING_WEIGHT: f32 = 550.0;
pub const TYPE_ROLE_SUBHEADING_SIZE_PX: f32 = 17.0;
pub const TYPE_ROLE_SUBHEADING_LINE_PX: f32 = 26.0;
pub const TYPE_ROLE_SUBHEADING_WEIGHT: f32 = 550.0;
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
pub const SIZE_SESSION_ROW_PX: f32 = 44.0;
pub const SIZE_TURN_LINE_PX: f32 = 28.0;
pub const SIZE_CHANGE_BAR_PX: f32 = 36.0;
pub const SIZE_TOUCH_TARGET_PX: f32 = 44.0;
pub const SIZE_ICON_PX: f32 = 16.0;
pub const SIZE_ICON_LARGE_PX: f32 = 20.0;
pub const SIZE_SIDEBAR_PX: f32 = 268.0;
pub const SIZE_READING_PX: f32 = 680.0;
pub const SIZE_COMPOSER_PX: f32 = 720.0;
pub const SIZE_PANEL_PX: f32 = 320.0;
pub const SIZE_PANEL_WIDE_PX: f32 = 560.0;
pub const BREAKPOINTS_COMPACT_PX: f32 = 720.0;
pub const BREAKPOINTS_MEDIUM_PX: f32 = 900.0;
pub const BREAKPOINTS_WIDE_PX: f32 = 1200.0;
pub const TYPE_BODY_PX: f32 = 14.0;
pub const TYPE_CAPTION_PX: f32 = 11.5;
pub const TYPE_PROSE_PX: f32 = 16.0;
pub const TYPE_HEADING_PX: f32 = 15.0;
pub const TYPE_TITLE_PX: f32 = 22.0;
