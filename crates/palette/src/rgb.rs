//! Raw RGB color tokens: `(u8, u8, u8)` tuples, the palette values that
//! do not need a renderer. Available without the `ratatui` feature, so the
//! headless runtime can read them; `tokens` builds the `Color` roles on top.

// Codewhale Whale palette. Semantic colors own state, never decoration alone.
pub const WHALE_BG_RGB: (u8, u8, u8) = (7, 12, 29); // #070C1D Ink field
pub const WHALE_CHROME_RGB: (u8, u8, u8) = (12, 21, 49); // #0C1531 Navy chrome
pub const WHALE_PANEL_RGB: (u8, u8, u8) = (16, 28, 64); // #101C40 Panel surface
pub const WHALE_COMPOSER_RGB: (u8, u8, u8) = (20, 35, 82); // #142352 Stage plate (brand navy)
pub const WHALE_ELEVATED_RGB: (u8, u8, u8) = (26, 44, 99); // #1A2C63 Raised
pub const WHALE_SELECTION_RGB: (u8, u8, u8) = (35, 45, 65); // #232D41 Quiet selection surface
pub const WHALE_TEXT_BODY_RGB: (u8, u8, u8) = (246, 242, 232); // #F6F2E8 Whale Ivory
pub const WHALE_TEXT_SOFT_RGB: (u8, u8, u8) = (182, 192, 212); // #B6C0D4
pub const WHALE_TEXT_MUTED_RGB: (u8, u8, u8) = (147, 160, 184); // #93A0B8
pub const WHALE_TEXT_HINT_RGB: (u8, u8, u8) = (138, 153, 179); // #8A99B3 — AA hint on the ink field
/// Blue Stage grammar: the interaction blue is the primary accent and the
/// Info ink; it has exactly one name. Signal Gold is reserved for the whale
/// mark and human-attention roles ([`WHALE_HUMAN_RGB`]), never for general
/// interaction.
pub const WHALE_ACTION_RGB: (u8, u8, u8) = (106, 166, 220); // #6AA6DC Ombre sky — owns interaction on dark
// No TUI consumer yet; the web reads these through scripts/export-design-tokens.py
// (`--whale-cobalt`, `--whale-ice`), so they are not dead.
pub const WHALE_COBALT_RGB: (u8, u8, u8) = (21, 53, 178); // #1535B2 Ombre cobalt — light-mode action
pub const WHALE_ICE_RGB: (u8, u8, u8) = (221, 238, 249); // #DDEEF9 Ice — structure on dark
pub const WHALE_CYAN_RGB: (u8, u8, u8) = (120, 188, 232); // #78BCE8 Cyan — bounded accents only
pub const WHALE_ACCENT_SECONDARY_RGB: (u8, u8, u8) = (79, 209, 197); // #4FD1C5 Seafoam
// Whale Teams (Signal Cut, CWC 2026-08-15) identity accents. These two brand
// palette entries have no semantic role in the TUI; they exist only so the
// Harbor and Echo whale marks carry their exact CWC accent instead of a
// borrowed state color. Never use them for status, mode, or permission.
pub const WHALE_BRAND_ORANGE_RGB: (u8, u8, u8) = (255, 138, 61); // #FF8A3D Harbor mooring loop
pub const WHALE_BRAND_MAGENTA_RGB: (u8, u8, u8) = (240, 78, 184); // #F04EB8 Echo sonar ticks
pub const WHALE_HUMAN_RGB: (u8, u8, u8) = (246, 196, 83); // #F6C453 Signal Gold
pub const WHALE_WORKING_GREEN_RGB: (u8, u8, u8) = (155, 214, 111); // #9BD66F Working Green
pub const WHALE_ERROR_RGB: (u8, u8, u8) = (255, 134, 178); // #FF86B2 Rose danger
pub const WHALE_ERROR_HOVER_RGB: (u8, u8, u8) = (255, 156, 194); // #FF9CC2
pub const WHALE_ERROR_SURFACE_RGB: (u8, u8, u8) = (43, 21, 34); // #2B1522
pub const WHALE_ERROR_BORDER_RGB: (u8, u8, u8) = WHALE_ERROR_RGB;
pub const WHALE_ERROR_TEXT_RGB: (u8, u8, u8) = (255, 219, 232); // #FFDBE8
pub const WHALE_WARNING_RGB: (u8, u8, u8) = (255, 122, 89); // #FF7A59 Coral warning
pub const WHALE_SUCCESS_RGB: (u8, u8, u8) = WHALE_WORKING_GREEN_RGB; // completed / verified
pub const WHALE_BORDER_RGB: (u8, u8, u8) = (42, 63, 114); // #2A3F72, sky at 25% on stage
pub const WHALE_REASONING_TEXT_RGB: (u8, u8, u8) = (224, 153, 72); // #E09948
pub const WHALE_REASONING_SURFACE_RGB: (u8, u8, u8) = (42, 34, 24); // #2A2218
pub const WHALE_REASONING_TINT_RGB: (u8, u8, u8) = (22, 36, 74); // #16244A

// Solarized Light palette RGB tuples
pub const SOLARIZED_BASE03_RGB: (u8, u8, u8) = (0x00, 0x2B, 0x36);
pub const SOLARIZED_BASE02_RGB: (u8, u8, u8) = (0x07, 0x36, 0x42);
pub const SOLARIZED_BASE01_RGB: (u8, u8, u8) = (0x52, 0x66, 0x6D); // lifted for 4.5:1 muted text
pub const SOLARIZED_BASE00_RGB: (u8, u8, u8) = (0x65, 0x7B, 0x83);
pub const SOLARIZED_BASE0_RGB: (u8, u8, u8) = (0x72, 0x81, 0x82); // lifted for 3:1 hint text
pub const SOLARIZED_BASE1_RGB: (u8, u8, u8) = (0x93, 0xA1, 0xA1);
pub const SOLARIZED_BASE2_RGB: (u8, u8, u8) = (0xEE, 0xE8, 0xD5);
pub const SOLARIZED_BASE3_RGB: (u8, u8, u8) = (0xFD, 0xF6, 0xE3);
pub const SOLARIZED_YELLOW_RGB: (u8, u8, u8) = (0xB4, 0x88, 0x00); // lifted for 3:1 on ivory
pub const SOLARIZED_ORANGE_RGB: (u8, u8, u8) = (0xCB, 0x4B, 0x16);
pub const SOLARIZED_RED_RGB: (u8, u8, u8) = (0xDC, 0x32, 0x2F);
pub const SOLARIZED_BLUE_RGB: (u8, u8, u8) = (0x26, 0x8B, 0xD2);
pub const SOLARIZED_CYAN_RGB: (u8, u8, u8) = (0x29, 0x9E, 0x96); // lifted for 3:1 on ivory
pub const SOLARIZED_GREEN_RGB: (u8, u8, u8) = (0x7F, 0x92, 0x00); // lifted for 3:1 on ivory/diff bg
pub const SOLARIZED_PANEL_RGB: (u8, u8, u8) = (0xF0, 0xED, 0xE7);
pub const SOLARIZED_ELEVATED_RGB: (u8, u8, u8) = (0xE4, 0xDF, 0xCF);
pub const SOLARIZED_SELECT_RGB: (u8, u8, u8) = (0xD6, 0xD2, 0xC9);

pub const WHALE_DIFF_ADDED_RGB: (u8, u8, u8) = (87, 199, 133); // #57C785
pub const WHALE_DIFF_ADDED_BG_RGB: (u8, u8, u8) = (18, 42, 34); // #122A22
// Raw colors that are remapped by equality must remain distinct across roles.
// These stay in the same perceptual families as action, danger, and human asks
// while preserving mode identity for Terminal and community themes.
pub const WHALE_DIFF_DELETED_BG_RGB: (u8, u8, u8) = (52, 24, 39); // #341827
pub const WHALE_MODE_AGENT_RGB: (u8, u8, u8) = (126, 180, 232); // #7EB4E8
pub const WHALE_MODE_YOLO_RGB: (u8, u8, u8) = (255, 112, 160); // #FF70A0
pub const WHALE_MODE_PLAN_RGB: (u8, u8, u8) = (185, 220, 236); // #B9DCEC Structural Ice
pub const WHALE_MODE_OPERATE_RGB: (u8, u8, u8) = (173, 136, 255); // #AD88FF
pub const WHALE_TOOL_LIVE_RGB: (u8, u8, u8) = WHALE_ACCENT_SECONDARY_RGB;
pub const WHALE_TOOL_ISSUE_RGB: (u8, u8, u8) = WHALE_ERROR_RGB;
pub const WHALE_TOOL_OUTPUT_RGB: (u8, u8, u8) = WHALE_TEXT_SOFT_RGB;
pub const WHALE_TOOL_SURFACE_RGB: (u8, u8, u8) = (15, 26, 58); // #0F1A3A
pub const WHALE_TOOL_ACTIVE_RGB: (u8, u8, u8) = (24, 44, 94); // #182C5E

pub const LIGHT_SURFACE_RGB: (u8, u8, u8) = (244, 247, 251); // #F4F7FB
pub const LIGHT_PANEL_RGB: (u8, u8, u8) = (255, 253, 248); // #FFFDF8
pub const LIGHT_ELEVATED_RGB: (u8, u8, u8) = (232, 238, 248); // #E8EEF8
pub const LIGHT_REASONING_RGB: (u8, u8, u8) = (255, 246, 214); // #FFF6D6
pub const LIGHT_SUCCESS_RGB: (u8, u8, u8) = (223, 247, 231); // #DFF7E7
pub const LIGHT_SUCCESS_FG_RGB: (u8, u8, u8) = (20, 118, 61); // #14763D, readable on every light surface
pub const LIGHT_ERROR_RGB: (u8, u8, u8) = (252, 235, 242); // #FCEBF2
pub const LIGHT_TEXT_BODY_RGB: (u8, u8, u8) = (20, 33, 58); // #14213A
pub const LIGHT_TEXT_MUTED_RGB: (u8, u8, u8) = (91, 103, 128); // #5B6780
pub const LIGHT_TEXT_HINT_RGB: (u8, u8, u8) = (95, 107, 129); // #5F6B81
pub const LIGHT_TEXT_SOFT_RGB: (u8, u8, u8) = (69, 81, 104); // #455168
pub const LIGHT_ACTION_RGB: (u8, u8, u8) = (21, 53, 178); // #1535B2 Ombre cobalt
pub const LIGHT_LIVE_RGB: (u8, u8, u8) = (8, 118, 109); // #08766D
pub const LIGHT_HUMAN_RGB: (u8, u8, u8) = (122, 85, 0); // #7A5500
pub const LIGHT_WARNING_RGB: (u8, u8, u8) = (169, 71, 36); // #A94724
pub const LIGHT_DANGER_RGB: (u8, u8, u8) = (180, 35, 90); // #B4235A
// Mode shades stay in their parent semantic families while remaining distinct
// inputs to the render backend. A `Cell` carries only a `Color`, so reusing the
// exact action/human/danger value here would erase the mode role before ANSI
// adaptation can preserve it.
pub const LIGHT_MODE_AGENT_RGB: (u8, u8, u8) = (22, 54, 178); // #1636B2
pub const LIGHT_MODE_YOLO_RGB: (u8, u8, u8) = (181, 35, 90); // #B5235A
pub const LIGHT_MODE_PLAN_RGB: (u8, u8, u8) = (52, 92, 128); // #345C80 Structural steel-blue
pub const LIGHT_OPERATE_RGB: (u8, u8, u8) = (112, 71, 184); // #7047B8

pub const LIGHT_BORDER_RGB: (u8, u8, u8) = (169, 184, 207); // #A9B8CF
pub const LIGHT_SELECTION_RGB: (u8, u8, u8) = (238, 246, 255); // #EEF6FF
pub const GRAYSCALE_SURFACE_RGB: (u8, u8, u8) = (10, 10, 10); // #0A0A0A
pub const GRAYSCALE_PANEL_RGB: (u8, u8, u8) = (18, 18, 18); // #121212
pub const GRAYSCALE_ELEVATED_RGB: (u8, u8, u8) = (31, 31, 31); // #1F1F1F
pub const GRAYSCALE_REASONING_RGB: (u8, u8, u8) = (38, 38, 38); // #262626
pub const GRAYSCALE_SUCCESS_RGB: (u8, u8, u8) = (34, 34, 34); // #222222
pub const GRAYSCALE_ERROR_RGB: (u8, u8, u8) = (42, 42, 42); // #2A2A2A
pub const GRAYSCALE_TEXT_BODY_RGB: (u8, u8, u8) = (236, 236, 236); // #ECECEC
pub const GRAYSCALE_TEXT_MUTED_RGB: (u8, u8, u8) = (180, 180, 180); // #B4B4B4
pub const GRAYSCALE_TEXT_HINT_RGB: (u8, u8, u8) = (138, 138, 138); // #8A8A8A
pub const GRAYSCALE_TEXT_SOFT_RGB: (u8, u8, u8) = (220, 220, 220); // #DCDCDC
pub const GRAYSCALE_BORDER_RGB: (u8, u8, u8) = (96, 96, 96); // #606060
pub const GRAYSCALE_SELECTION_RGB: (u8, u8, u8) = (62, 62, 62); // #3E3E3E

pub const MATRIX_SURFACE_RGB: (u8, u8, u8) = (0, 10, 0); // #000A00
pub const MATRIX_ELEVATED_RGB: (u8, u8, u8) = (0, 51, 0); // #003300
pub const MATRIX_SELECTION_RGB: (u8, u8, u8) = (0, 51, 0); // #003300
pub const MATRIX_TEXT_BODY_RGB: (u8, u8, u8) = (136, 255, 136); // #88FF88
pub const MATRIX_TEXT_MUTED_RGB: (u8, u8, u8) = (0, 169, 0); // #00A900, lifted for 4.5:1
pub const MATRIX_TEXT_HINT_RGB: (u8, u8, u8) = (0, 135, 0); // #008700, lifted for 3:1
pub const MATRIX_TEXT_SOFT_RGB: (u8, u8, u8) = (221, 255, 221); // #DDFFDD
pub const MATRIX_TEXT_DIM_RGB: (u8, u8, u8) = (0, 108, 0); // #006C00, lifted for 3:1
pub const MATRIX_BORDER_RGB: (u8, u8, u8) = (0, 204, 0); // #00CC00

// Shoreline — the TUI's charcoal theme.
//
// Warm charcoal ground and warm paper sheet, one restrained blue, and the
// whale's ivory ink on both sides. This is the charcoal alternative to the
// terminal's navy "Underwater" default in 0.10.0. The
// 0.10.0 action pair uses glacial blue on charcoal and deep ocean blue on
// paper.
//
// Every pair audited by `contrast::theme_contrast_violations` clears its
// floor: body roles clear 4.5:1 on all four surfaces, hint/dim and the
// status roles clear 3:1.
pub const SHORELINE_SURFACE_RGB: (u8, u8, u8) = (33, 31, 35); // #211F23 warm charcoal field
pub const SHORELINE_PANEL_RGB: (u8, u8, u8) = (43, 40, 46); // #2B282E raised plate
pub const SHORELINE_ELEVATED_RGB: (u8, u8, u8) = (53, 49, 58); // #35313A
pub const SHORELINE_COMPOSER_RGB: (u8, u8, u8) = (43, 40, 46); // #2B282E
pub const SHORELINE_CHROME_RGB: (u8, u8, u8) = (26, 24, 28); // #1A181C recessed chrome
pub const SHORELINE_SELECTION_RGB: (u8, u8, u8) = (44, 70, 84); // #2C4654 deep ocean selection
pub const SHORELINE_TEXT_BODY_RGB: (u8, u8, u8) = (242, 236, 229); // #F2ECE5
pub const SHORELINE_TEXT_SOFT_RGB: (u8, u8, u8) = (217, 210, 220); // #D9D2DC
pub const SHORELINE_TEXT_MUTED_RGB: (u8, u8, u8) = (176, 167, 178); // #B0A7B2
pub const SHORELINE_TEXT_HINT_RGB: (u8, u8, u8) = (154, 145, 159); // #9A919F
pub const SHORELINE_TEXT_DIM_RGB: (u8, u8, u8) = (126, 117, 131); // #7E7583
pub const SHORELINE_BORDER_RGB: (u8, u8, u8) = (73, 66, 77); // #49424D
pub const SHORELINE_ACTION_RGB: (u8, u8, u8) = (103, 184, 214); // #67B8D6 glacial blue — action and whale identity
pub const SHORELINE_LIVE_RGB: (u8, u8, u8) = (127, 214, 198); // #7FD6C6 the live lane
pub const SHORELINE_HUMAN_RGB: (u8, u8, u8) = (246, 196, 83); // #F6C453 Signal Gold, the human lane
pub const SHORELINE_ERROR_RGB: (u8, u8, u8) = (255, 143, 168); // #FF8FA8
pub const SHORELINE_ERROR_HOVER_RGB: (u8, u8, u8) = (255, 166, 187); // #FFA6BB
pub const SHORELINE_ERROR_SURFACE_RGB: (u8, u8, u8) = (58, 32, 41); // #3A2029
pub const SHORELINE_ERROR_TEXT_RGB: (u8, u8, u8) = (255, 220, 230); // #FFDCE6
pub const SHORELINE_WARNING_RGB: (u8, u8, u8) = (240, 168, 104); // #F0A868
pub const SHORELINE_SUCCESS_RGB: (u8, u8, u8) = (163, 217, 119); // #A3D977
pub const SHORELINE_STATUS_WORKING_RGB: (u8, u8, u8) = SHORELINE_LIVE_RGB;
// Mode badges keep the shipped mode ramp: these four are proof-tested distinct
// from one another and from every semantic lane, and a mode badge is identity,
// not decoration. `mode_agent` must differ from `accent_primary` or
// `adapt::theme_semantic_foreground_role` resolves the action lane to a mode
// role (`ModeAgent`) before it ever reaches `Action`.
pub const SHORELINE_MODE_AGENT_RGB: (u8, u8, u8) = (126, 180, 232); // #7EB4E8
pub const SHORELINE_MODE_YOLO_RGB: (u8, u8, u8) = (255, 112, 160); // #FF70A0
pub const SHORELINE_MODE_PLAN_RGB: (u8, u8, u8) = (185, 220, 236); // #B9DCEC
pub const SHORELINE_MODE_OPERATE_RGB: (u8, u8, u8) = (173, 136, 255); // #AD88FF
pub const SHORELINE_DIFF_ADDED_FG_RGB: (u8, u8, u8) = (127, 214, 160); // #7FD6A0
pub const SHORELINE_DIFF_ADDED_BG_RGB: (u8, u8, u8) = (30, 44, 37); // #1E2C25
pub const SHORELINE_DIFF_DELETED_FG_RGB: (u8, u8, u8) = (255, 154, 171); // #FF9AAB
pub const SHORELINE_DIFF_DELETED_BG_RGB: (u8, u8, u8) = (51, 32, 42); // #33202A

// Shoreline Light — the same system on warm paper.
pub const SHORELINE_LIGHT_SURFACE_RGB: (u8, u8, u8) = (245, 240, 233); // #F5F0E9
pub const SHORELINE_LIGHT_PANEL_RGB: (u8, u8, u8) = (236, 229, 224); // #ECE5E0
pub const SHORELINE_LIGHT_ELEVATED_RGB: (u8, u8, u8) = (255, 252, 247); // #FFFCF7
pub const SHORELINE_LIGHT_COMPOSER_RGB: (u8, u8, u8) = (251, 247, 241); // #FBF7F1
pub const SHORELINE_LIGHT_CHROME_RGB: (u8, u8, u8) = (237, 231, 224); // #EDE7E0
pub const SHORELINE_LIGHT_SELECTION_RGB: (u8, u8, u8) = (201, 224, 231); // #C9E0E7 pale ocean selection
pub const SHORELINE_LIGHT_TEXT_BODY_RGB: (u8, u8, u8) = (48, 40, 50); // #302832
pub const SHORELINE_LIGHT_TEXT_SOFT_RGB: (u8, u8, u8) = (74, 65, 76); // #4A414C
pub const SHORELINE_LIGHT_TEXT_MUTED_RGB: (u8, u8, u8) = (107, 96, 110); // #6B606E
pub const SHORELINE_LIGHT_TEXT_HINT_RGB: (u8, u8, u8) = (117, 112, 128); // #757080
pub const SHORELINE_LIGHT_TEXT_DIM_RGB: (u8, u8, u8) = (138, 130, 144); // #8A8290
pub const SHORELINE_LIGHT_BORDER_RGB: (u8, u8, u8) = (215, 206, 213); // #D7CED5
pub const SHORELINE_LIGHT_ACTION_RGB: (u8, u8, u8) = (0, 102, 132); // #006684 deep ocean blue on warm paper
pub const SHORELINE_LIGHT_LIVE_RGB: (u8, u8, u8) = (31, 122, 107); // #1F7A6B
pub const SHORELINE_LIGHT_HUMAN_RGB: (u8, u8, u8) = (122, 85, 0); // #7A5500 Signal Gold at AA on paper
pub const SHORELINE_LIGHT_ERROR_RGB: (u8, u8, u8) = (180, 35, 90); // #B4235A
pub const SHORELINE_LIGHT_ERROR_SURFACE_RGB: (u8, u8, u8) = (251, 228, 236); // #FBE4EC
pub const SHORELINE_LIGHT_ERROR_TEXT_RGB: (u8, u8, u8) = (122, 18, 53); // #7A1235
pub const SHORELINE_LIGHT_WARNING_RGB: (u8, u8, u8) = (143, 85, 20); // #8F5514
pub const SHORELINE_LIGHT_SUCCESS_RGB: (u8, u8, u8) = (47, 107, 58); // #2F6B3A
pub const SHORELINE_LIGHT_MODE_AGENT_RGB: (u8, u8, u8) = (22, 54, 178); // #1636B2 distinct from LIGHT action
pub const SHORELINE_LIGHT_MODE_YOLO_RGB: (u8, u8, u8) = (181, 35, 90); // #B5235A
pub const SHORELINE_LIGHT_MODE_PLAN_RGB: (u8, u8, u8) = (52, 92, 128); // #345C80
pub const SHORELINE_LIGHT_MODE_OPERATE_RGB: (u8, u8, u8) = (112, 71, 184); // #7047B8
pub const SHORELINE_LIGHT_DIFF_ADDED_FG_RGB: (u8, u8, u8) = (31, 107, 69); // #1F6B45
pub const SHORELINE_LIGHT_DIFF_ADDED_BG_RGB: (u8, u8, u8) = (226, 242, 230); // #E2F2E6
pub const SHORELINE_LIGHT_DIFF_DELETED_FG_RGB: (u8, u8, u8) = (168, 40, 80); // #A82850
pub const SHORELINE_LIGHT_DIFF_DELETED_BG_RGB: (u8, u8, u8) = (251, 230, 236); // #FBE6EC

// Semantic colors
pub const BORDER_COLOR_RGB: (u8, u8, u8) = WHALE_BORDER_RGB;
