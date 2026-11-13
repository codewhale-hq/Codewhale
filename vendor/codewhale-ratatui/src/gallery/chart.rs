//! Gallery: data displays fully drawn, mid draw-in, empty and from agent JSON.

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{DataBarItem, DataBars, DataDisplay, DataSparkline, DataTrend, Paint, Theme};

fn series() -> Vec<f64> {
    (0..64)
        .map(|i| 120.0 + 60.0 * (f64::from(i) / 6.0).sin() + f64::from((i * 7) % 11) * 3.0)
        .collect()
}

fn items() -> Vec<DataBarItem> {
    vec![
        DataBarItem::new("engine", 412.0),
        DataBarItem::new("tui", 268.0),
        DataBarItem::new("runtime", 131.0),
        DataBarItem::new("protocol", 44.0),
    ]
}

fn row(area: Rect, y: u16, h: u16) -> Rect {
    Rect::new(area.x, area.y.saturating_add(y), area.width, h).intersection(area)
}

fn sparklines(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let values = series();
    DataSparkline::new("Requests", &values)
        .unit("rpm")
        .paint(row(area, 0, 1), buf, theme);
    DataSparkline::new("Requests", &values)
        .unit("rpm")
        .drawn(0.45)
        .paint(row(area, 1, 1), buf, theme);
    DataSparkline::new("Requests", &[]).paint(row(area, 2, 1), buf, theme);
}

fn bars(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let items = items();
    DataBars::new("Tests by crate", &items).paint(row(area, 0, 5), buf, theme);
}

fn bars_growing(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let items = items();
    DataBars::new("Tests by crate", &items)
        .previous(&[300.0, 300.0, 300.0, 300.0])
        .drawn(0.5)
        .paint(row(area, 0, 5), buf, theme);
}

fn trend(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let values = series();
    DataTrend::new("Latency", &values)
        .unit("ms")
        .rows(7)
        .paint(area, buf, theme);
}

fn trend_drawing(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let values = series();
    DataTrend::new("Latency", &values)
        .unit("ms")
        .rows(7)
        .drawn(0.6)
        .paint(area, buf, theme);
}

fn from_agent(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let shown = DataDisplay::from_json(
        r#"{"kind":"bars","label":"Tokens this turn","unit":"tokens",
            "items":[{"label":"read","value":18200},{"label":"wrote","value":2400},
                     {"label":"cached","value":61000}]}"#,
    )
    .expect("gallery fixture is a valid data display");
    shown.view().paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "data-sparkline",
            width: 80,
            height: 3,
            draw: sparklines,
        },
        Entry {
            name: "data-bars",
            width: 60,
            height: 5,
            draw: bars,
        },
        Entry {
            name: "data-bars-growing",
            width: 60,
            height: 5,
            draw: bars_growing,
        },
        Entry {
            name: "data-trend",
            width: 72,
            height: 7,
            draw: trend,
        },
        Entry {
            name: "data-trend-drawing",
            width: 72,
            height: 7,
            draw: trend_drawing,
        },
        Entry {
            name: "data-from-agent",
            width: 60,
            height: 4,
            draw: from_agent,
        },
    ]
}
