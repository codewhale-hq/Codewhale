//! Authored marine poses and their composition with the existing whale.
//! Elapsed time and the resting whale state are illustrative caller inputs.

use std::time::Duration;

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{
    BubbleField, Caps, DistantWhale, FishSchool, Habitat, HabitatDensity, Heading, Jellyfish,
    MarineSnow, Message, MotionMode, Paint, Shoal, Theme, Whale, WhaleState,
};

fn fish(area: Rect, buf: &mut Buffer, theme: &Theme) {
    FishSchool::new(Duration::from_millis(2_400), MotionMode::Full)
        .density(HabitatDensity::Rich)
        .paint(area, buf, theme);
}

fn jelly(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Jellyfish::new(Duration::from_millis(1_800), MotionMode::Full).paint(area, buf, theme);
}

fn bubbles(area: Rect, buf: &mut Buffer, theme: &Theme) {
    BubbleField::new(Duration::from_millis(1_200), MotionMode::Full)
        .density(HabitatDensity::Rich)
        .paint(area, buf, theme);
}

fn shoal(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Shoal::new(Duration::from_millis(9_000), MotionMode::Full)
        .density(HabitatDensity::Rich)
        .paint(area, buf, theme);
}

fn snow(area: Rect, buf: &mut Buffer, theme: &Theme) {
    MarineSnow::new(Duration::from_millis(4_000), MotionMode::Full)
        .density(HabitatDensity::Rich)
        .paint(area, buf, theme);
}

fn distant_whale(area: Rect, buf: &mut Buffer, theme: &Theme) {
    DistantWhale::new(Duration::from_millis(40_000), MotionMode::Full)
        .hold_visit(true)
        .paint(area, buf, theme);
}

fn scene_with(area: Rect, buf: &mut Buffer, theme: &Theme, motion: MotionMode) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    Heading::new("Codewhale / clear water")
        .meta(if motion.animates() {
            "Native habitat"
        } else {
            "Reduced motion"
        })
        .paint(Rect { height: 1, ..area }, buf, theme);
    let words = Rect {
        x: area.x.saturating_add(2),
        y: area.y.saturating_add(4),
        width: area.width.min(32).saturating_sub(2),
        height: area.height.saturating_sub(4).min(7),
    }
    .intersection(area);
    Message::new(
        "One ocean",
        "A small school, a far shoal.\nOne visiting jellyfish.\nSnow, bubbles, a distant whale.",
    )
    .paint(words, buf, theme);
    let whale = Rect {
        x: area.x.saturating_add(area.width.saturating_sub(32) / 2),
        y: area.y.saturating_add(3),
        width: area.width.min(32),
        height: area.height.saturating_sub(3).min(18),
    }
    .intersection(area);
    Whale::new(WhaleState::Rest).paint(whale, buf, theme);
    Habitat::new(Duration::from_millis(40_000), motion)
        .density(HabitatDensity::Rich)
        .hold_jellyfish_visit(true)
        .paint(area, buf, theme);
}

fn scene(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene_with(area, buf, theme, MotionMode::Full);
}

fn ascii(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let ascii = Theme::new(Caps {
        ascii: true,
        ..theme.caps()
    })
    .ground(theme.ground_kind());
    scene_with(area, buf, &ascii, MotionMode::Full);
}

fn reduced(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene_with(area, buf, theme, MotionMode::Reduced);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "fish-school",
            width: 72,
            height: 8,
            draw: fish,
        },
        Entry {
            name: "jellyfish",
            width: 40,
            height: 9,
            draw: jelly,
        },
        Entry {
            name: "bubble-field",
            width: 72,
            height: 9,
            draw: bubbles,
        },
        Entry {
            name: "shoal",
            width: 72,
            height: 12,
            draw: shoal,
        },
        Entry {
            name: "marine-snow",
            width: 72,
            height: 10,
            draw: snow,
        },
        Entry {
            name: "distant-whale",
            width: 72,
            height: 12,
            draw: distant_whale,
        },
        Entry {
            name: "habitat-scene",
            width: 112,
            height: 30,
            draw: scene,
        },
        Entry {
            name: "habitat-ascii",
            width: 84,
            height: 24,
            draw: ascii,
        },
        Entry {
            name: "habitat-reduced",
            width: 112,
            height: 30,
            draw: reduced,
        },
    ]
}
