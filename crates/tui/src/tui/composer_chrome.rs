//! Compatibility policy facade over the adopted kit composer plan.
use crate::tui::app::ComposerDensity;
use codewhale_ratatui::{NativeComposerDensity, native_composer_height};
fn density(value: ComposerDensity) -> NativeComposerDensity {
    match value {
        ComposerDensity::Compact => NativeComposerDensity::Compact,
        ComposerDensity::Comfortable => NativeComposerDensity::Comfortable,
        ComposerDensity::Spacious => NativeComposerDensity::Spacious,
    }
}
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposerChrome {
    pub border_rows: u16,
    pub max_total_rows: u16,
}
#[cfg(test)]
impl ComposerChrome {
    pub fn for_density(value: ComposerDensity, panel: bool) -> Self {
        Self {
            border_rows: if panel { 2 } else { 1 },
            max_total_rows: density(value).max_rows(),
        }
    }
}
pub fn desired_height(
    rows: usize,
    menus: usize,
    available: u16,
    value: ComposerDensity,
    panel: bool,
) -> u16 {
    native_composer_height(rows, menus, available, density(value), panel)
}
#[cfg(test)]
pub use codewhale_ratatui::native_composer_top_padding as top_padding;
#[cfg(test)]
#[path = "composer_chrome_legacy.rs"]
mod legacy;
#[cfg(test)]
pub use legacy::{render_tideline_composer_submit, tideline_composer_geometry};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_composer_respects_density_and_keeps_padding_below_the_caret() {
        for (density, height) in [
            (ComposerDensity::Compact, 2),
            (ComposerDensity::Comfortable, 3),
            (ComposerDensity::Spacious, 4),
        ] {
            assert_eq!(desired_height(1, 0, 8, density, false), height);
        }
        assert_eq!(
            top_padding(1, 2),
            0,
            "one spare row belongs below the input"
        );
        assert_eq!(top_padding(1, 3), 1);
    }

    #[test]
    fn compact_height_sheds_border_before_content() {
        // Only two rows available: keep a border + one content row.
        let height = desired_height(1, 0, 2, ComposerDensity::Comfortable, false);
        assert_eq!(height, 2);
    }

    #[test]
    fn content_growth_expands_past_the_density_cap_up_to_available_room() {
        // Six content rows + border fits under the Comfortable cap of 9.
        let height = desired_height(6, 0, 12, ComposerDensity::Comfortable, false);
        assert_eq!(height, 7, "typed content must grow the composer: {height}");

        let comfortable = desired_height(20, 0, 30, ComposerDensity::Comfortable, false);
        assert_eq!(comfortable, 21, "Comfortable grows past its cap of 9");
        let spacious = desired_height(20, 0, 30, ComposerDensity::Spacious, false);
        assert_eq!(spacious, 21, "Spacious grows past its cap of 12");
    }

    #[test]
    fn spacious_panel_reserves_input_padding_and_both_borders() {
        let height = desired_height(1, 0, 12, ComposerDensity::Spacious, true);
        assert_eq!(height, 5, "panel = 2 borders + 3 input rows, got {height}");
    }
}
