//! Data displays: numbers the host or an agent reports, drawn as a
//! sparkline, a bar list or a trend.
//!
//! - The words carry every fact (`now 42  min 3  max 57`); the ink is a
//!   picture of the same numbers and never the only place a value appears.
//! - Each display takes `drawn`, a caller-clock progress in `0..=1` from
//!   [`crate::MotionStep::drawn`]. Under reduced or still motion that is the
//!   end state, so nothing moves and every value is shown.
//! - [`DataDisplay`] is the same vocabulary as data: an agent sends JSON,
//!   the host validates it with [`DataDisplay::from_json`] and paints it with
//!   [`DataDisplay::view`]. [`DataDisplay::describe`] says the same facts as
//!   plain text for a transcript, a screen reader or the model's own context.

use std::borrow::Cow;
use std::fmt;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use serde::{Deserialize, Serialize};

use crate::{Paint, Role, Theme, glyphs, text};

/// The most points a [`DataDisplay`] sparkline or trend may carry.
pub const DATA_MAX_POINTS: usize = 1024;
/// The most rows a [`DataDisplay`] bar list may carry.
pub const DATA_MAX_BARS: usize = 24;
/// The longest label or unit, in characters.
pub const DATA_MAX_LABEL: usize = 120;
/// The rows a trend may ask for.
pub const DATA_TREND_ROWS: std::ops::RangeInclusive<u16> = 3..=16;

const SPARK: [&str; 8] = [
    "\u{2581}", "\u{2582}", "\u{2583}", "\u{2584}", "\u{2585}", "\u{2586}", "\u{2587}", "\u{2588}",
];
const SPARK_ASCII: [&str; 8] = ["_", "_", ".", "-", "-", "=", "#", "#"];
const EIGHTHS: [&str; 8] = [
    "", "\u{258F}", "\u{258E}", "\u{258D}", "\u{258C}", "\u{258B}", "\u{258A}", "\u{2589}",
];
const FULL: &str = "\u{2588}";
const MIN_SPARK: usize = 4;
const MAX_SPARK: usize = 64;
const MIN_BAR: usize = 4;
const BAR_STAGGER: f32 = 0.35;

/// Words the data displays print. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataWords {
    pub now: Cow<'static, str>,
    pub min: Cow<'static, str>,
    pub max: Cow<'static, str>,
    /// Shown when there are no values yet.
    pub empty: Cow<'static, str>,
    /// `24 points` in [`DataDisplay::describe`].
    pub points: Cow<'static, str>,
}

impl Default for DataWords {
    fn default() -> Self {
        Self {
            now: Cow::Borrowed("now"),
            min: Cow::Borrowed("min"),
            max: Cow::Borrowed("max"),
            empty: Cow::Borrowed("no data yet"),
            points: Cow::Borrowed("points"),
        }
    }
}

/// A short form of `value`: `42`, `3.14`, `12.4k`, `1.2M`.
#[must_use]
pub fn data_number(value: f64) -> String {
    if !value.is_finite() {
        return "?".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let a = value.abs();
    let (scaled, suffix) = if a >= 1e9 {
        (value / 1e9, "B")
    } else if a >= 1e6 {
        (value / 1e6, "M")
    } else if a >= 1e4 {
        (value / 1e3, "k")
    } else {
        (value, "")
    };
    let s = if scaled.abs() >= 100.0 {
        format!("{scaled:.0}")
    } else if scaled.abs() >= 10.0 {
        format!("{scaled:.1}")
    } else {
        format!("{scaled:.2}")
    };
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    };
    format!("{s}{suffix}")
}

fn with_unit(value: f64, unit: &str) -> String {
    let n = data_number(value);
    let unit = text::display_safe(unit);
    match unit.as_ref() {
        "" => n,
        "%" => format!("{n}%"),
        u => format!("{n} {u}"),
    }
}

fn unit_f(v: f32) -> f32 {
    if v.is_nan() { 1.0 } else { v.clamp(0.0, 1.0) }
}

fn bounds(values: &[f64]) -> Option<(f64, f64)> {
    let mut it = values.iter().copied().filter(|v| v.is_finite());
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v))))
}

fn level(v: f64, lo: f64, hi: f64, steps: usize) -> usize {
    if !v.is_finite() || steps == 0 {
        return 0;
    }
    if hi <= lo {
        return steps / 2;
    }
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    ((t * (steps - 1) as f64).round() as usize).min(steps - 1)
}

struct Stats {
    full: Vec<Span<'static>>,
    short: Vec<Span<'static>>,
    full_w: usize,
    short_w: usize,
}

fn stats(values: &[f64], unit: &str, words: &DataWords, theme: &Theme) -> Option<Stats> {
    let (lo, hi) = bounds(values)?;
    let last = values.iter().rev().copied().find(|v| v.is_finite())?;
    let muted = theme.fg(Role::Muted);
    let ink = theme.fg(Role::Foreground);
    let pair = |word: &str, v: f64| {
        [
            Span::styled(format!("{} ", text::display_safe(word)), muted),
            Span::styled(with_unit(v, unit), ink),
        ]
    };
    let short: Vec<Span<'static>> = pair(&words.now, last).into();
    let mut full = short.clone();
    full.push(Span::raw("  "));
    full.extend(pair(&words.min, lo));
    full.push(Span::raw("  "));
    full.extend(pair(&words.max, hi));
    let w = |s: &[Span]| s.iter().map(|s| text::width(&s.content)).sum();
    Some(Stats {
        full_w: w(&full),
        short_w: w(&short),
        full,
        short,
    })
}

fn put(area: Rect, buf: &mut Buffer, row: u16, spans: Vec<Span<'_>>) {
    if row >= area.height {
        return;
    }
    Line::from(spans).render(Rect::new(area.x, area.y + row, area.width, 1), buf);
}

fn title_spans<'a>(label: &str, max: usize, theme: &Theme) -> (Span<'a>, usize) {
    let label = text::display_safe(label);
    let shown = text::truncate(&label, max, theme.ascii()).into_owned();
    let w = text::width(&shown);
    (
        Span::styled(
            shown,
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
        ),
        w,
    )
}

/// `Requests  ▂▃▅▇█▆▄  now 42  min 3  max 57`: one row, newest on the right.
///
/// The window shows the newest values that fit; the words describe the
/// whole series. While `drawn` is below one the line draws in from the left.
#[derive(Clone, Debug)]
pub struct DataSparkline<'a> {
    pub label: Cow<'a, str>,
    pub unit: Cow<'a, str>,
    pub values: &'a [f64],
    pub drawn: f32,
    pub words: DataWords,
}

impl<'a> DataSparkline<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, values: &'a [f64]) -> Self {
        Self {
            label: label.into(),
            unit: Cow::Borrowed(""),
            values,
            drawn: 1.0,
            words: DataWords::default(),
        }
    }

    #[must_use]
    pub fn unit(mut self, unit: impl Into<Cow<'a, str>>) -> Self {
        self.unit = unit.into();
        self
    }

    #[must_use]
    pub fn drawn(mut self, drawn: f32) -> Self {
        self.drawn = unit_f(drawn);
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &DataWords) -> Self {
        self.words = words.clone();
        self
    }
}

impl Paint for DataSparkline<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let width = usize::from(area.width);
        let label_max = if self.label.is_empty() {
            0
        } else {
            (width / 3).max(4)
        };
        let (title, label_w) = title_spans(&self.label, label_max, theme);
        let gap = if label_w > 0 { 2 } else { 0 };
        let mut spans = vec![title, Span::raw(" ".repeat(gap))];
        let Some(stats) = stats(self.values, &self.unit, &self.words, theme) else {
            spans.push(Span::styled(
                text::display_safe(&self.words.empty).into_owned(),
                theme.fg(Role::Muted),
            ));
            put(area, buf, 0, spans);
            return;
        };
        let room = width.saturating_sub(label_w + gap);
        let (words, words_w) = if stats.full_w + 2 + MIN_SPARK <= room {
            (stats.full, stats.full_w)
        } else {
            (stats.short, stats.short_w)
        };
        let cells = room
            .saturating_sub(words_w + 2)
            .min(MAX_SPARK)
            .min(self.values.len());
        if cells >= MIN_SPARK.min(self.values.len()).max(1) {
            let window = &self.values[self.values.len() - cells..];
            let (lo, hi) = bounds(window).unwrap_or((0.0, 0.0));
            let shown = (cells as f32 * self.drawn).ceil() as usize;
            let ramp = if theme.ascii() { SPARK_ASCII } else { SPARK };
            let mut line = String::with_capacity(cells * 3);
            for (i, v) in window.iter().enumerate() {
                line.push_str(if i < shown {
                    ramp[level(*v, lo, hi, ramp.len())]
                } else {
                    " "
                });
            }
            spans.push(Span::styled(line, theme.fg(Role::Accent)));
            spans.push(Span::raw("  "));
        }
        spans.extend(words);
        put(area, buf, 0, spans);
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        1
    }
}

/// One named value in a [`DataBars`] list or a [`DataDisplay::Bars`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataBarItem {
    pub label: String,
    pub value: f64,
}

impl DataBarItem {
    #[must_use]
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }
}

/// A title row, then one row per item: `opus  ██████████▍  12.4k tokens`.
///
/// Bars scale to the largest value and fill in eighths of a cell. While
/// `drawn` is below one each bar grows from its `previous` value (or zero)
/// to its value, a little after the bar above it. A negative value draws no
/// bar; its words keep the real number.
#[derive(Clone, Debug)]
pub struct DataBars<'a> {
    pub label: Cow<'a, str>,
    pub unit: Cow<'a, str>,
    pub items: &'a [DataBarItem],
    /// The values each row grows from, by index. Missing rows grow from zero.
    pub previous: &'a [f64],
    pub drawn: f32,
    pub words: DataWords,
}

impl<'a> DataBars<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, items: &'a [DataBarItem]) -> Self {
        Self {
            label: label.into(),
            unit: Cow::Borrowed(""),
            items,
            previous: &[],
            drawn: 1.0,
            words: DataWords::default(),
        }
    }

    #[must_use]
    pub fn unit(mut self, unit: impl Into<Cow<'a, str>>) -> Self {
        self.unit = unit.into();
        self
    }

    #[must_use]
    pub fn previous(mut self, previous: &'a [f64]) -> Self {
        self.previous = previous;
        self
    }

    #[must_use]
    pub fn drawn(mut self, drawn: f32) -> Self {
        self.drawn = unit_f(drawn);
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &DataWords) -> Self {
        self.words = words.clone();
        self
    }

    fn row_progress(&self, row: usize) -> f64 {
        let n = self.items.len();
        let delay = if n > 1 {
            BAR_STAGGER * row as f32 / (n - 1) as f32
        } else {
            0.0
        };
        f64::from(((self.drawn - delay) / (1.0 - BAR_STAGGER)).clamp(0.0, 1.0))
    }

    fn bar(&self, eighths: usize, ascii: bool) -> String {
        if ascii {
            "#".repeat(eighths.div_ceil(8))
        } else {
            let mut s = FULL.repeat(eighths / 8);
            s.push_str(EIGHTHS[eighths % 8]);
            s
        }
    }
}

impl Paint for DataBars<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let width = usize::from(area.width);
        let ascii = theme.ascii();
        let mut row = 0;
        if !self.label.is_empty() {
            let (title, _) = title_spans(&self.label, width, theme);
            put(area, buf, 0, vec![title]);
            row = 1;
        }
        if self.items.is_empty() {
            put(
                area,
                buf,
                row,
                vec![Span::styled(
                    text::display_safe(&self.words.empty).into_owned(),
                    theme.fg(Role::Muted),
                )],
            );
            return;
        }
        let values: Vec<String> = self
            .items
            .iter()
            .map(|i| with_unit(i.value, &self.unit))
            .collect();
        let value_w = values.iter().map(|v| text::width(v)).max().unwrap_or(0);
        let labels: Vec<Cow<'_, str>> = self
            .items
            .iter()
            .map(|i| text::display_safe(&i.label))
            .collect();
        let label_w = labels
            .iter()
            .map(|l| text::width(l))
            .max()
            .unwrap_or(0)
            .min((width / 3).max(4));
        let cells = width.saturating_sub(label_w + 2 + 2 + value_w);
        let peak = self
            .items
            .iter()
            .map(|i| i.value)
            .chain(self.previous.iter().copied())
            .filter(|v| v.is_finite())
            .fold(0.0_f64, f64::max);
        for (i, (item, value)) in self.items.iter().zip(&values).enumerate() {
            let y = row + i as u16;
            if y >= area.height {
                break;
            }
            let mut spans = vec![
                Span::styled(
                    text::pad(&labels[i], label_w, ascii),
                    theme.fg(Role::Foreground),
                ),
                Span::raw("  "),
            ];
            if cells >= MIN_BAR {
                let from = self
                    .previous
                    .get(i)
                    .copied()
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0);
                let to = if item.value.is_finite() {
                    item.value
                } else {
                    0.0
                };
                let shown = from + (to - from) * self.row_progress(i);
                let frac = if peak > 0.0 {
                    (shown / peak).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let eighths = (frac * cells as f64 * 8.0).round() as usize;
                let bar = self.bar(eighths, ascii);
                let bar_w = text::width(&bar);
                spans.push(Span::styled(bar, theme.fg(Role::Accent)));
                spans.push(Span::raw(" ".repeat(cells.saturating_sub(bar_w) + 2)));
            }
            spans.push(Span::styled(value.clone(), theme.fg(Role::Muted)));
            put(area, buf, y, spans);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        let title = u16::from(!self.label.is_empty());
        title + (self.items.len().max(1)).min(usize::from(u16::MAX - 1)) as u16
    }
}

/// A caption row, then a braille line chart with its range on the left.
///
/// While `drawn` is below one the line draws from the left and its pen, the
/// newest drawn cell, is in body ink. Too small for a chart, it paints a
/// [`DataSparkline`] instead.
#[derive(Clone, Debug)]
pub struct DataTrend<'a> {
    pub label: Cow<'a, str>,
    pub unit: Cow<'a, str>,
    pub values: &'a [f64],
    pub drawn: f32,
    pub rows: u16,
    pub words: DataWords,
}

impl<'a> DataTrend<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, values: &'a [f64]) -> Self {
        Self {
            label: label.into(),
            unit: Cow::Borrowed(""),
            values,
            drawn: 1.0,
            rows: 6,
            words: DataWords::default(),
        }
    }

    #[must_use]
    pub fn unit(mut self, unit: impl Into<Cow<'a, str>>) -> Self {
        self.unit = unit.into();
        self
    }

    #[must_use]
    pub fn drawn(mut self, drawn: f32) -> Self {
        self.drawn = unit_f(drawn);
        self
    }

    /// Rows including the caption.
    #[must_use]
    pub fn rows(mut self, rows: u16) -> Self {
        self.rows = rows.max(1);
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &DataWords) -> Self {
        self.words = words.clone();
        self
    }

    fn sparkline(&self) -> DataSparkline<'_> {
        DataSparkline {
            label: Cow::Borrowed(&self.label),
            unit: Cow::Borrowed(&self.unit),
            values: self.values,
            drawn: self.drawn,
            words: self.words.clone(),
        }
    }
}

fn sample(values: &[f64], columns: usize) -> Vec<f64> {
    let n = values.len();
    if n == 0 || columns == 0 {
        return Vec::new();
    }
    if n == 1 || columns == 1 {
        return vec![values[n - 1]; columns];
    }
    (0..columns)
        .map(|c| {
            let at = c as f64 * (n - 1) as f64 / (columns - 1) as f64;
            let (i, t) = (at.floor() as usize, at.fract());
            let a = values[i];
            let b = values[(i + 1).min(n - 1)];
            a + (b - a) * t
        })
        .collect()
}

fn braille_bit(dx: usize, dy: usize) -> u8 {
    match (dx, dy) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (0, _) => 0x40,
        _ => 0x80,
    }
}

impl Paint for DataTrend<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let Some((lo, hi)) = bounds(self.values) else {
            self.sparkline().paint(area, buf, theme);
            return;
        };
        let ascii = theme.ascii();
        let hi_s = with_unit(hi, &self.unit);
        let lo_s = with_unit(lo, &self.unit);
        let axis_w = text::width(&hi_s).max(text::width(&lo_s));
        let plot_rows = usize::from(area.height.min(self.rows)).saturating_sub(1);
        let plot_w = usize::from(area.width).saturating_sub(axis_w + 2);
        if plot_rows < 2 || plot_w < MIN_SPARK * 2 {
            self.sparkline().paint(area, buf, theme);
            return;
        }

        let (title, label_w) = title_spans(&self.label, usize::from(area.width) / 2, theme);
        let mut caption = vec![title];
        if let Some(stats) = stats(self.values, &self.unit, &self.words, theme) {
            let room = usize::from(area.width).saturating_sub(label_w + 2);
            let (words, w) = if stats.full_w <= room {
                (stats.full, stats.full_w)
            } else {
                (stats.short, stats.short_w)
            };
            if w <= room {
                caption.push(Span::raw("  "));
                caption.extend(words);
            }
        }
        put(area, buf, 0, caption);

        let dots_w = plot_w * 2;
        let dots_h = plot_rows * 4;
        let points = sample(self.values, dots_w);
        let shown = ((dots_w as f32 * self.drawn).ceil() as usize).min(dots_w);
        let mut cells = vec![0u8; plot_w * plot_rows];
        let y_of = |v: f64| dots_h - 1 - level(v, lo, hi, dots_h);
        let mut prev: Option<usize> = None;
        for (x, v) in points.iter().take(shown).enumerate() {
            let y = y_of(*v);
            let (a, b) = match prev {
                Some(p) if p < y => (p + 1, y),
                Some(p) if p > y => (y, p - 1),
                _ => (y, y),
            };
            for yy in a..=b.max(a) {
                cells[(yy / 4) * plot_w + x / 2] |= braille_bit(x % 2, yy % 4);
            }
            prev = Some(y);
        }
        let pen = (shown < dots_w && shown > 0).then(|| (shown - 1) / 2);

        let axis_style = theme.fg(Role::Muted);
        let rail = glyphs::pick("\u{2502}", ascii);
        for r in 0..plot_rows {
            let y = area.y + 1 + r as u16;
            let tick = if r == 0 {
                hi_s.as_str()
            } else if r == plot_rows - 1 {
                lo_s.as_str()
            } else {
                ""
            };
            let tick = format!("{tick:>axis_w$} ");
            buf.set_stringn(area.x, y, &tick, axis_w + 1, axis_style);
            buf.set_stringn(
                area.x + (axis_w + 1) as u16,
                y,
                rail,
                1,
                theme.fg(Role::Dim),
            );
            for c in 0..plot_w {
                let bits = cells[r * plot_w + c];
                if bits == 0 {
                    continue;
                }
                let x = area.x + (axis_w + 2 + c) as u16;
                let style: Style = if pen == Some(c) {
                    theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
                } else {
                    theme.fg(Role::Accent)
                };
                let glyph = char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ');
                let mut encoded = [0; 4];
                let symbol: &str = if ascii {
                    glyphs::braille_ascii_fallback(glyph).unwrap_or(".")
                } else {
                    glyph.encode_utf8(&mut encoded)
                };
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(symbol).set_style(style);
                }
            }
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        self.rows
    }
}

/// A data display as data: what an agent sends to show numbers.
///
/// ```
/// use codewhale_ratatui::DataDisplay;
///
/// let shown = DataDisplay::from_json(
///     r#"{"kind":"bars","label":"Tokens by model","unit":"tokens",
///         "items":[{"label":"opus","value":12400},{"label":"haiku","value":3100}]}"#,
/// )
/// .unwrap();
/// assert_eq!(
///     shown.describe(&Default::default()),
///     "Tokens by model: opus 12.4k tokens, haiku 3100 tokens"
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataDisplay {
    Sparkline {
        label: String,
        #[serde(default)]
        unit: String,
        values: Vec<f64>,
    },
    Bars {
        label: String,
        #[serde(default)]
        unit: String,
        items: Vec<DataBarItem>,
    },
    Trend {
        label: String,
        #[serde(default)]
        unit: String,
        values: Vec<f64>,
        #[serde(default)]
        rows: Option<u16>,
    },
}

/// Why a [`DataDisplay`] was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataDisplayError {
    Parse(String),
    TooManyPoints { count: usize },
    TooManyBars { count: usize },
    NotFinite { index: usize },
    LabelTooLong { chars: usize },
    Rows { rows: u16 },
}

impl fmt::Display for DataDisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "not a data display: {e}"),
            Self::TooManyPoints { count } => {
                write!(f, "{count} points; at most {DATA_MAX_POINTS}")
            }
            Self::TooManyBars { count } => write!(f, "{count} bars; at most {DATA_MAX_BARS}"),
            Self::NotFinite { index } => write!(f, "value {index} is not a finite number"),
            Self::LabelTooLong { chars } => {
                write!(f, "a label of {chars} characters; at most {DATA_MAX_LABEL}")
            }
            Self::Rows { rows } => write!(
                f,
                "{rows} rows; a trend takes {} to {}",
                DATA_TREND_ROWS.start(),
                DATA_TREND_ROWS.end()
            ),
        }
    }
}

impl std::error::Error for DataDisplayError {}

/// The JSON Schema for [`DataDisplay`], for an agent's tool definition.
pub const DATA_DISPLAY_SCHEMA: &str = r#"{
  "type": "object",
  "description": "Numbers to show the person: a one-row sparkline, a list of labelled bars, or a multi-row trend.",
  "required": ["kind", "label"],
  "properties": {
    "kind": { "enum": ["sparkline", "bars", "trend"] },
    "label": { "type": "string", "maxLength": 120 },
    "unit": { "type": "string", "maxLength": 120, "description": "Shown after each number, e.g. ms, tokens, %." },
    "values": { "type": "array", "items": { "type": "number" }, "maxItems": 1024, "description": "Oldest first. For sparkline and trend." },
    "items": {
      "type": "array",
      "maxItems": 24,
      "description": "For bars.",
      "items": {
        "type": "object",
        "required": ["label", "value"],
        "properties": { "label": { "type": "string", "maxLength": 120 }, "value": { "type": "number" } },
        "additionalProperties": false
      }
    },
    "rows": { "type": "integer", "minimum": 3, "maximum": 16, "description": "Trend height including its caption." }
  }
}"#;

impl DataDisplay {
    /// Parse and [`validate`](Self::validate) an agent's JSON.
    pub fn from_json(json: &str) -> Result<Self, DataDisplayError> {
        let shown: Self =
            serde_json::from_str(json).map_err(|e| DataDisplayError::Parse(e.to_string()))?;
        shown.validate()?;
        Ok(shown)
    }

    /// Refuse what the displays cannot show honestly: non-finite numbers,
    /// more points or bars than the limits, overlong labels, a trend's rows
    /// outside [`DATA_TREND_ROWS`].
    pub fn validate(&self) -> Result<(), DataDisplayError> {
        let label_ok = |s: &str| {
            let chars = s.chars().count();
            if chars > DATA_MAX_LABEL {
                Err(DataDisplayError::LabelTooLong { chars })
            } else {
                Ok(())
            }
        };
        let finite = |values: &mut dyn Iterator<Item = f64>| {
            values
                .enumerate()
                .find(|(_, v)| !v.is_finite())
                .map_or(Ok(()), |(index, _)| {
                    Err(DataDisplayError::NotFinite { index })
                })
        };
        match self {
            Self::Sparkline {
                label,
                unit,
                values,
            }
            | Self::Trend {
                label,
                unit,
                values,
                ..
            } => {
                label_ok(label)?;
                label_ok(unit)?;
                if values.len() > DATA_MAX_POINTS {
                    return Err(DataDisplayError::TooManyPoints {
                        count: values.len(),
                    });
                }
                finite(&mut values.iter().copied())?;
                if let Self::Trend {
                    rows: Some(rows), ..
                } = self
                    && !DATA_TREND_ROWS.contains(rows)
                {
                    return Err(DataDisplayError::Rows { rows: *rows });
                }
                Ok(())
            }
            Self::Bars { label, unit, items } => {
                label_ok(label)?;
                label_ok(unit)?;
                if items.len() > DATA_MAX_BARS {
                    return Err(DataDisplayError::TooManyBars { count: items.len() });
                }
                for item in items {
                    label_ok(&item.label)?;
                }
                finite(&mut items.iter().map(|i| i.value))
            }
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Sparkline { label, .. }
            | Self::Bars { label, .. }
            | Self::Trend { label, .. } => label,
        }
    }

    /// The same facts as plain text: `Latency: now 42 ms, min 3 ms, max 57
    /// ms (24 points)`.
    #[must_use]
    pub fn describe(&self, words: &DataWords) -> String {
        let label = text::display_safe(self.label());
        match self {
            Self::Sparkline { unit, values, .. } | Self::Trend { unit, values, .. } => {
                let (Some((lo, hi)), Some(last)) =
                    (bounds(values), values.iter().rev().find(|v| v.is_finite()))
                else {
                    return format!("{label}: {}", words.empty);
                };
                format!(
                    "{label}: {} {}, {} {}, {} {} ({} {})",
                    words.now,
                    with_unit(*last, unit),
                    words.min,
                    with_unit(lo, unit),
                    words.max,
                    with_unit(hi, unit),
                    values.len(),
                    words.points,
                )
            }
            Self::Bars { unit, items, .. } => {
                if items.is_empty() {
                    return format!("{label}: {}", words.empty);
                }
                let rows: Vec<String> = items
                    .iter()
                    .map(|i| {
                        format!(
                            "{} {}",
                            text::display_safe(&i.label),
                            with_unit(i.value, unit)
                        )
                    })
                    .collect();
                format!("{label}: {}", rows.join(", "))
            }
        }
    }

    /// A paintable view, fully drawn.
    #[must_use]
    pub fn view(&self) -> DataDisplayView<'_> {
        DataDisplayView {
            display: self,
            drawn: 1.0,
            previous: Vec::new(),
            words: DataWords::default(),
        }
    }
}

/// A [`DataDisplay`] ready to paint, with its draw-in progress.
#[derive(Clone, Debug)]
pub struct DataDisplayView<'a> {
    pub display: &'a DataDisplay,
    pub drawn: f32,
    previous: Vec<f64>,
    pub words: DataWords,
}

impl DataDisplayView<'_> {
    #[must_use]
    pub fn drawn(mut self, drawn: f32) -> Self {
        self.drawn = unit_f(drawn);
        self
    }

    /// Bars grow from the value the same label had in `before`.
    #[must_use]
    pub fn from(mut self, before: &DataDisplay) -> Self {
        if let (DataDisplay::Bars { items, .. }, DataDisplay::Bars { items: old, .. }) =
            (self.display, before)
        {
            self.previous = items
                .iter()
                .map(|i| {
                    old.iter()
                        .find(|o| o.label == i.label)
                        .map_or(0.0, |o| o.value)
                })
                .collect();
        }
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &DataWords) -> Self {
        self.words = words.clone();
        self
    }
}

impl Paint for DataDisplayView<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        match self.display {
            DataDisplay::Sparkline {
                label,
                unit,
                values,
            } => DataSparkline::new(label.as_str(), values)
                .unit(unit.as_str())
                .drawn(self.drawn)
                .with_words(&self.words)
                .paint(area, buf, theme),
            DataDisplay::Bars { label, unit, items } => DataBars::new(label.as_str(), items)
                .unit(unit.as_str())
                .previous(&self.previous)
                .drawn(self.drawn)
                .with_words(&self.words)
                .paint(area, buf, theme),
            DataDisplay::Trend {
                label,
                unit,
                values,
                rows,
            } => DataTrend::new(label.as_str(), values)
                .unit(unit.as_str())
                .rows(rows.unwrap_or(6))
                .drawn(self.drawn)
                .with_words(&self.words)
                .paint(area, buf, theme),
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        match self.display {
            DataDisplay::Sparkline { .. } => 1,
            DataDisplay::Bars { label, items, .. } => {
                DataBars::new(label.as_str(), items).height(width, theme)
            }
            DataDisplay::Trend { rows, .. } => rows.unwrap_or(6),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn series() -> Vec<f64> {
        (0..48)
            .map(|i| 40.0 + 25.0 * (f64::from(i) / 5.0).sin() + f64::from(i % 7))
            .collect()
    }

    fn bars() -> Vec<DataBarItem> {
        vec![
            DataBarItem::new("opus", 12_400.0),
            DataBarItem::new("sonnet", 7_900.0),
            DataBarItem::new("haiku", 3_100.0),
            DataBarItem::new("cache", 0.0),
        ]
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(data_number(42.0), "42");
        assert_eq!(data_number(1.23456), "1.23");
        assert_eq!(data_number(12_400.0), "12.4k");
        assert_eq!(data_number(1_250_000.0), "1.25M");
        assert_eq!(data_number(-0.0), "0");
        assert_eq!(data_number(f64::NAN), "?");
        assert_eq!(with_unit(87.0, "%"), "87%");
        assert_eq!(with_unit(87.0, "ms"), "87 ms");
    }

    #[test]
    fn every_display_keeps_the_rules() {
        let values = series();
        let items = bars();
        for drawn in [0.0, 0.4, 1.0] {
            testing::assert_rules(1, |a, b, t| {
                DataSparkline::new("Requests", &values)
                    .unit("rpm")
                    .drawn(drawn)
                    .paint(a, b, t);
            });
            testing::assert_rules(5, |a, b, t| {
                DataBars::new("Tokens by model", &items)
                    .drawn(drawn)
                    .paint(a, b, t);
            });
            testing::assert_rules(6, |a, b, t| {
                DataTrend::new("Latency", &values)
                    .unit("ms")
                    .drawn(drawn)
                    .paint(a, b, t);
            });
        }
    }

    #[test]
    fn the_words_carry_every_value_before_anything_is_drawn() {
        let values = series();
        let items = bars();
        for frame in testing::frames(1, |a, b, t| {
            DataSparkline::new("Requests", &values)
                .drawn(0.0)
                .paint(a, b, t);
        }) {
            assert!(
                frame
                    .text()
                    .contains(&format!("now {}", data_number(values[47])))
            );
        }
        for frame in testing::frames(5, |a, b, t| {
            DataBars::new("Tokens", &items).drawn(0.0).paint(a, b, t);
        }) {
            let text = frame.text();
            assert!(text.contains("12.4k") && text.contains("3100"), "{text}");
            assert!(!text.contains(FULL) && !text.contains('#'), "{text}");
        }
    }

    #[test]
    fn bars_reach_their_value_and_scale_to_the_largest() {
        let items = bars();
        let frames = testing::frames(5, |a, b, t| {
            DataBars::new("", &items).paint(a, b, t);
        });
        let text = frames[0].text();
        let lines: Vec<&str> = text.lines().collect();
        let full = |l: &str| l.matches(FULL).count();
        assert!(full(lines[0]) > full(lines[1]) && full(lines[1]) > full(lines[2]));
        assert_eq!(full(lines[3]), 0);
    }

    #[test]
    fn bars_glide_from_their_previous_values() {
        let items = vec![DataBarItem::new("a", 10.0)];
        let previous = [5.0];
        let at = |drawn: f32| {
            let buf = testing::render(40, 1, |area, buf| {
                DataBars::new("", &items)
                    .previous(&previous)
                    .drawn(drawn)
                    .paint(area, buf, &testing::Profile::DarkTrue.theme());
            });
            testing::text(&buf).matches(FULL).count()
        };
        assert!(at(0.0) > 0);
        assert!(at(0.0) < at(1.0));
    }

    #[test]
    fn the_trend_draws_in_from_the_left_with_a_pen() {
        let values = series();
        let theme = testing::Profile::DarkTrue.theme();
        let dots = |drawn: f32| {
            let buf = testing::render(60, 6, |area, buf| {
                DataTrend::new("Latency", &values)
                    .drawn(drawn)
                    .paint(area, buf, &theme);
            });
            testing::text(&buf)
                .chars()
                .filter(|c| ('\u{2801}'..='\u{28FF}').contains(c))
                .count()
        };
        assert_eq!(dots(0.0), 0);
        assert!(dots(0.5) > 0 && dots(0.5) < dots(1.0));
    }

    #[test]
    fn a_tiny_trend_falls_back_to_a_sparkline() {
        let values = series();
        let buf = testing::render(40, 1, |area, buf| {
            DataTrend::new("Latency", &values).paint(
                area,
                buf,
                &testing::Profile::DarkTrue.theme(),
            );
        });
        assert!(testing::text(&buf).contains(SPARK[7]) || testing::text(&buf).contains("now"));
    }

    #[test]
    fn empty_displays_say_so() {
        let buf = testing::render(40, 2, |area, buf| {
            DataBars::new("Tokens", &[]).paint(area, buf, &testing::Profile::DarkTrue.theme());
        });
        assert!(testing::text(&buf).contains("no data yet"));
        let buf = testing::render(40, 6, |area, buf| {
            DataTrend::new("Latency", &[]).paint(area, buf, &testing::Profile::DarkTrue.theme());
        });
        assert!(testing::text(&buf).contains("no data yet"));
    }

    #[test]
    fn agent_json_is_validated_before_paint() {
        let ok = DataDisplay::from_json(
            r#"{"kind":"trend","label":"Latency","unit":"ms","values":[3,9,4],"rows":5}"#,
        )
        .unwrap();
        assert_eq!(
            ok.describe(&DataWords::default()),
            "Latency: now 4 ms, min 3 ms, max 9 ms (3 points)"
        );
        assert_eq!(ok.view().height(80, &testing::Profile::DarkTrue.theme()), 5);

        let refused = |json: &str| DataDisplay::from_json(json).unwrap_err();
        assert!(matches!(
            refused(r#"{"kind":"pie","label":"x"}"#),
            DataDisplayError::Parse(_)
        ));
        assert_eq!(
            refused(r#"{"kind":"trend","label":"x","values":[1],"rows":40}"#),
            DataDisplayError::Rows { rows: 40 }
        );
        let long = "x".repeat(DATA_MAX_LABEL + 1);
        assert_eq!(
            refused(&format!(
                r#"{{"kind":"sparkline","label":"{long}","values":[]}}"#
            )),
            DataDisplayError::LabelTooLong {
                chars: DATA_MAX_LABEL + 1
            }
        );
        let many = vec!["1"; DATA_MAX_POINTS + 1].join(",");
        assert_eq!(
            refused(&format!(
                r#"{{"kind":"sparkline","label":"x","values":[{many}]}}"#
            )),
            DataDisplayError::TooManyPoints {
                count: DATA_MAX_POINTS + 1
            }
        );
        let built = DataDisplay::Sparkline {
            label: "x".into(),
            unit: String::new(),
            values: vec![1.0, f64::INFINITY],
        };
        assert_eq!(
            built.validate(),
            Err(DataDisplayError::NotFinite { index: 1 })
        );
    }

    #[test]
    fn the_schema_is_json_and_names_every_kind() {
        let schema: serde_json::Value = serde_json::from_str(DATA_DISPLAY_SCHEMA).unwrap();
        let kinds = &schema["properties"]["kind"]["enum"];
        for kind in ["sparkline", "bars", "trend"] {
            assert!(kinds.as_array().unwrap().iter().any(|k| k == kind));
        }
    }

    #[test]
    fn bars_view_grows_from_the_same_label() {
        let before = DataDisplay::Bars {
            label: "t".into(),
            unit: String::new(),
            items: vec![DataBarItem::new("b", 8.0), DataBarItem::new("a", 2.0)],
        };
        let after = DataDisplay::Bars {
            label: "t".into(),
            unit: String::new(),
            items: vec![DataBarItem::new("a", 4.0), DataBarItem::new("c", 1.0)],
        };
        assert_eq!(after.view().from(&before).previous, vec![2.0, 0.0]);
    }

    #[test]
    fn snapshot() {
        let values = series();
        let items = bars();
        insta::assert_snapshot!(
            "data-displays",
            testing::snapshot(12, |area, buf, theme| {
                let row = |y: u16, h: u16| {
                    Rect::new(area.x, area.y + y, area.width, h).intersection(area)
                };
                DataSparkline::new("Requests", &values)
                    .unit("rpm")
                    .paint(row(0, 1), buf, theme);
                DataBars::new("Tokens by model", &items)
                    .unit("tokens")
                    .paint(row(2, 5), buf, theme);
                DataTrend::new("Latency", &values)
                    .unit("ms")
                    .rows(5)
                    .paint(row(7, 5), buf, theme);
            })
        );
    }
}
