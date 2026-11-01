//! Five atmospheres over real components, one entry per [`WaterPalette`].
//!
//! Every entry paints the same illustrative workspace — a heading, the four
//! state words, a deep-water panel with the whale and a little marine life —
//! through the usual components and then finishes it with one
//! [`Ombre::apply`], so the five previews differ only in the atmosphere. All
//! content is illustrative host data, never a live measurement.

use std::time::Duration;

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{
    BubbleField, Depth, FishSchool, HabitatDensity, Heading, Message, MotionMode, Ombre, Paint,
    Panel, Picker, PickerItem, PickerState, Role, State, StatusMark, Theme, WaterPalette, Whale,
    WhaleState,
};

/// The rows a band starts at, clamped to the area it is cut from.
fn band(area: Rect, top: u16, height: u16) -> Rect {
    let top = top.min(area.height);
    Rect::new(
        area.x,
        area.y.saturating_add(top),
        area.width,
        height.min(area.height.saturating_sub(top)),
    )
}

/// One atmosphere over a workspace card, the four state words and the whale,
/// finished last by the ombre.
fn scene(area: Rect, buf: &mut Buffer, theme: &Theme, palette: WaterPalette) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    Heading::new(format!("Codewhale / {} atmosphere", palette.name()))
        .meta("Finishing pass")
        .paint(band(area, 0, 1), buf, theme);
    let body = band(area, 1, area.height.saturating_sub(1));
    if body.is_empty() {
        return;
    }

    let rail_width = 20.min(body.width);
    let rail = Rect {
        width: rail_width,
        ..body
    };
    let inner = Panel::new(Depth::Deep)
        .title("Palettes")
        .aside("wash")
        .draw(rail, buf, theme);
    let items: Vec<PickerItem> = WaterPalette::ALL
        .iter()
        .map(|palette| PickerItem::new(palette.name()))
        .collect();
    let selected = WaterPalette::ALL
        .iter()
        .position(|candidate| *candidate == palette)
        .unwrap_or(0);
    Picker::new(&items, PickerState::new(selected)).paint(inner, buf, theme);

    let gap = u16::from(body.width > rail_width);
    let stage = Rect {
        x: rail.right().saturating_add(gap),
        width: body
            .right()
            .saturating_sub(rail.right().saturating_add(gap)),
        ..body
    };
    if stage.is_empty() {
        return;
    }
    Message::new("You", "Finish this scene with one atmosphere.")
        .role(Role::Accent)
        .paint(band(stage, 0, 2), buf, theme);

    let turn = Panel::new(Depth::Raised)
        .title("Turn")
        .draw(band(stage, 2, 7), buf, theme);
    for (row, state) in [State::Working, State::NeedsYou, State::Failed, State::Done]
        .into_iter()
        .enumerate()
    {
        let row = u16::try_from(row).unwrap_or(0);
        StatusMark::new(state).paint(band(turn, row, 1), buf, theme);
    }

    let water = Panel::new(Depth::Deep).title("Deep water").draw(
        band(stage, 9, stage.height.saturating_sub(9)),
        buf,
        theme,
    );
    let whale = Rect {
        width: 24.min(water.width),
        height: water.height.min(11),
        ..water
    };
    Whale::new(WhaleState::Busy).paint(whale, buf, theme);
    let life = Rect {
        x: water.x.saturating_add(whale.width.saturating_add(1)),
        width: water.width.saturating_sub(whale.width.saturating_add(1)),
        height: water.height.min(7),
        ..water
    };
    if !life.is_empty() {
        FishSchool::new(Duration::from_millis(2_400), MotionMode::Full)
            .density(HabitatDensity::Normal)
            .paint(life, buf, theme);
        BubbleField::new(Duration::from_millis(1_200), MotionMode::Full)
            .density(HabitatDensity::Normal)
            .paint(life, buf, theme);
    }

    // The finishing pass, last: one cohesive atmosphere across the scene.
    Ombre::new(palette).apply(area, buf, theme);
}

fn ocean(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene(area, buf, theme, WaterPalette::Ocean);
}

fn lagoon(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene(area, buf, theme, WaterPalette::Lagoon);
}

fn dusk(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene(area, buf, theme, WaterPalette::Dusk);
}

fn coral(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene(area, buf, theme, WaterPalette::Coral);
}

fn graphite(area: Rect, buf: &mut Buffer, theme: &Theme) {
    scene(area, buf, theme, WaterPalette::Graphite);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "atmosphere-ocean",
            width: 104,
            height: 24,
            draw: ocean,
        },
        Entry {
            name: "atmosphere-lagoon",
            width: 104,
            height: 24,
            draw: lagoon,
        },
        Entry {
            name: "atmosphere-dusk",
            width: 104,
            height: 24,
            draw: dusk,
        },
        Entry {
            name: "atmosphere-coral",
            width: 104,
            height: 24,
            draw: coral,
        },
        Entry {
            name: "atmosphere-graphite",
            width: 104,
            height: 24,
            draw: graphite,
        },
    ]
}
