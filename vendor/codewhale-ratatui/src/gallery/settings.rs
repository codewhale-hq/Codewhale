//! Settings facts are caller-owned; the gallery supplies deterministic examples.
use super::Entry;
use crate::{
    Paint, SessionGroupHeader, SettingDetail, SettingRow, SettingsRail, SettingsSection, Theme,
};
use ratatui::{buffer::Buffer, layout::Rect};

fn setting_rows(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let rows = [
        SettingRow::new("Theme", "Shoreline")
            .source("this project")
            .changed(true)
            .modified(true)
            .selected(true),
        SettingRow::new("Motion", "Reduced").source("your settings"),
        SettingRow::new("Context window", "200k tokens")
            .apply("Read only")
            .locked("Set by your admin"),
    ];
    let mut y = area.y;
    for row in rows {
        let height = row
            .height(area.width, theme)
            .min(area.bottom().saturating_sub(y));
        if height == 0 {
            return;
        }
        row.paint(Rect { y, height, ..area }, buf, theme);
        y += height;
    }
}

fn setting_detail(area: Rect, buf: &mut Buffer, theme: &Theme) {
    SettingDetail::new(
        "Theme",
        "Sets the colors Codewhale paints. Shoreline follows your terminal's light or dark ground.",
    )
    .default_value("System")
    .source("saved to this project")
    .modified(true)
    .paint(area, buf, theme);
}

fn setting_rail(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let sections = [
        SettingsSection::new("General", 4),
        SettingsSection::new("Providers", 0),
        SettingsSection::new("Models", 2),
        SettingsSection::new("Appearance", 3),
    ];
    SettingsRail::new(&sections, 0)
        .searching(true)
        .paint(area, buf, theme);
}

fn session_group_headers(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let cue = if theme.ascii() {
        "Tab - 1 needs you"
    } else {
        "Tab · 1 needs you"
    };
    let headers = [
        SessionGroupHeader::in_progress(3).attention_cue(cue),
        SessionGroupHeader::idle(5),
    ];
    for (i, header) in headers.iter().enumerate() {
        let y = area.y.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        if y >= area.bottom() {
            break;
        }
        header.paint(
            Rect {
                y,
                height: 1,
                ..area
            },
            buf,
            theme,
        );
    }
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "setting-row",
            width: 80,
            height: 6,
            draw: setting_rows,
        },
        Entry {
            name: "setting-detail",
            width: 80,
            height: 7,
            draw: setting_detail,
        },
        Entry {
            name: "settings-rail",
            width: 28,
            height: 6,
            draw: setting_rail,
        },
        Entry {
            name: "session-group-header",
            width: 56,
            height: 2,
            draw: session_group_headers,
        },
    ]
}
