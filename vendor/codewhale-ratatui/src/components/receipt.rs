//! Receipts: what work cost, as rows a person can scan.
//!
//! A [`Receipt`] is one row (`✓ Edited summary.md  Done  4 s  $0.01`) and a
//! [`ReceiptTable`] aligns many of them in columns. Numbers are right-aligned
//! on the unit or the decimal point, there are no boxes or stripes, and a
//! value nobody reported shows as `—`, never `0`: a zero is a measurement and
//! an unknown is not. Totals say when they cover only the rows that reported.
//!
//! The formatting helpers ([`format_duration`], [`format_tokens`],
//! [`format_cost`], [`format_bytes`], [`format_count`]) are compact and
//! locale-neutral: no thousands separators, binary bytes (`KiB`), rounding
//! only where the row is too narrow for more digits, and no value that
//! rounds a real quantity down to zero.
//!
//! Modelled on the engine's roster receipts (`crates/tui/src/tui/
//! agent_roster.rs` and `agent_roster::format_duration` / `format_tokens`,
//! `Hmbown/CodeWhale` `58b1dd3dd`), whose "absent receipts render as `—`"
//! rule this keeps.

use std::{borrow::Cow, time::Duration};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{Paint, Role, State, StateWords, Theme, glyphs, text};

/// The mark for a value nobody reported. Zero is a measurement; this is not.
pub const UNKNOWN_VALUE: &str = "\u{2014}";
/// [`UNKNOWN_VALUE`] in ASCII-safe terminals.
pub const UNKNOWN_VALUE_ASCII: &str = "-";

/// `—`, or `-` where marks are ASCII-safe.
#[must_use]
pub const fn unknown_value(ascii: bool) -> &'static str {
    if ascii {
        UNKNOWN_VALUE_ASCII
    } else {
        UNKNOWN_VALUE
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// `850 ms`, `12 s`, `4m 06s`, `1h 02m`, `2d 03h`: the spinner's spelling
/// ([`crate::duration`]), continued past a day. Fractions are dropped, not
/// rounded up. A real duration that is not zero but shorter than a
/// millisecond reads `<1 ms`, never `0 ms`.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        let ms = d.as_millis();
        if ms == 0 && !d.is_zero() {
            return "<1 ms".to_string();
        }
        format!("{ms} ms")
    } else if secs < 60 {
        format!("{secs} s")
    } else if secs < 3_600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3_600)
    }
}

/// The value `n / unit`, in tenths, rounded half up. Integer arithmetic, so
/// no float error reaches a count.
fn tenths(n: u64, unit: u128) -> u128 {
    (u128::from(n) * 10 + unit / 2) / unit
}

/// `value` in tenths as `12.4`, or `12` when the fraction is zero.
fn trim_tenths(tenths: u128) -> String {
    let (whole, frac) = (tenths / 10, tenths % 10);
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

/// Scale `n` by steps of `base` through `units`, one decimal, promoting a
/// unit when rounding would show `1000` of the one below (`999.95k` is `1M`,
/// not `1000k`). With `whole_from` set, values at or above it drop the
/// decimal (`12 MiB`); the promotion check uses what is shown.
fn scaled(n: u64, base: u128, units: &[&str], whole_from: Option<u128>) -> String {
    let mut step = 0;
    let mut unit = 1u128;
    while step + 1 < units.len() && u128::from(n) >= unit * base {
        unit *= base;
        step += 1;
    }
    loop {
        let mut shown = tenths(n, unit);
        if let Some(limit) = whole_from
            && shown >= limit * 10
        {
            shown = (shown + 5) / 10 * 10;
        }
        if shown >= base * 10 && step + 1 < units.len() {
            unit *= base;
            step += 1;
            continue;
        }
        return format!("{}{}", trim_tenths(shown), units[step]);
    }
}

/// `812`, `12.4k`, `1.2M`, `3B`: a token count, one decimal at most. Below a
/// thousand it is exact.
#[must_use]
pub fn format_tokens(tokens: u64) -> String {
    scaled(tokens, 1_000, &["", "k", "M", "B", "T"], None)
}

/// A count of things (files, calls): exact below ten thousand, then compact
/// as [`format_tokens`] is. No separators: `1234`, not `1,234`.
#[must_use]
pub fn format_count(count: u64) -> String {
    if count < 10_000 {
        count.to_string()
    } else {
        format_tokens(count)
    }
}

/// `812 B`, `1.5 KiB`, `12 MiB`: binary units, one decimal below ten of a
/// unit and none above, so a column of them keeps its width.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        return format!("{bytes} B");
    }
    scaled(
        bytes,
        1_024,
        &[" B", " KiB", " MiB", " GiB", " TiB", " PiB", " EiB"],
        Some(10),
    )
}

/// A cost in millionths of the currency's unit, with its symbol: `$0.38`,
/// `$12.40`. Under one cent it keeps four decimals (`$0.0042`) so cheap work
/// is not shown as free; under a hundredth of a cent it reads `<$0.0001`.
/// A true zero reads `$0.00`: it is a measurement.
#[must_use]
pub fn format_cost(micros: u64, symbol: &str) -> String {
    if micros == 0 {
        return format!("{symbol}0.00");
    }
    if micros < 50 {
        return format!("<{symbol}0.0001");
    }
    if micros < 9_950 {
        let tenths_of_cents = (micros + 50) / 100;
        return format!("{symbol}0.{tenths_of_cents:04}");
    }
    let cents = (u128::from(micros) + 5_000) / 10_000;
    format!("{symbol}{}.{:02}", cents / 100, cents % 100)
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A cost: millionths of a currency unit, its symbol, and whether it is an
/// estimate (shown with `~`, never as a plain figure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cost {
    pub micros: u64,
    pub symbol: Cow<'static, str>,
    /// What to print where marks are ASCII-safe: `$`, or `EUR ` for `€`.
    pub ascii_symbol: Cow<'static, str>,
    pub estimate: bool,
}

impl Cost {
    /// `micros` millionths of a unit, in `symbol`. The ASCII form is the
    /// symbol when it is ASCII and empty otherwise; set it with
    /// [`Cost::ascii_symbol`].
    #[must_use]
    pub fn new(micros: u64, symbol: impl Into<Cow<'static, str>>) -> Self {
        let symbol = symbol.into();
        let ascii_symbol = if symbol.is_ascii() {
            symbol.clone()
        } else {
            Cow::Borrowed("")
        };
        Self {
            micros,
            symbol,
            ascii_symbol,
            estimate: false,
        }
    }

    /// US dollars from whole cents.
    #[must_use]
    pub fn usd_cents(cents: u64) -> Self {
        Self::new(cents.saturating_mul(10_000), "$")
    }

    #[must_use]
    pub fn ascii_symbol(mut self, symbol: impl Into<Cow<'static, str>>) -> Self {
        self.ascii_symbol = symbol.into();
        self
    }

    #[must_use]
    pub fn estimate(mut self) -> Self {
        self.estimate = true;
        self
    }
}

/// One cell of a receipt. `None` is "nobody reported this", and shows as
/// [`UNKNOWN_VALUE`], never as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptValue {
    /// Words the caller already formatted (a time of day, a model name).
    Text(Cow<'static, str>),
    Duration(Option<Duration>),
    Tokens(Option<u64>),
    Count(Option<u64>),
    Bytes(Option<u64>),
    Cost(Option<Cost>),
}

/// How a column lines its numbers up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    /// The end of the text: units line up.
    End,
    /// The decimal point (or the end of the digits): `$0.38` over `$12.40`.
    Point,
}

impl ReceiptValue {
    #[must_use]
    pub fn text(text: impl Into<Cow<'static, str>>) -> Self {
        Self::Text(text.into())
    }

    /// Whether this value is the unknown mark.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(
            self,
            Self::Duration(None)
                | Self::Tokens(None)
                | Self::Count(None)
                | Self::Bytes(None)
                | Self::Cost(None)
        )
    }

    fn is_estimate(&self) -> bool {
        matches!(self, Self::Cost(Some(c)) if c.estimate)
    }

    fn align(&self) -> Align {
        match self {
            Self::Bytes(_) | Self::Cost(_) => Align::Point,
            _ => Align::End,
        }
    }

    /// The text this value shows.
    #[must_use]
    pub fn render(&self, ascii: bool) -> String {
        match self {
            Self::Text(t) => text::display_safe(t).into_owned(),
            Self::Duration(Some(d)) => format_duration(*d),
            Self::Tokens(Some(n)) => format_tokens(*n),
            Self::Count(Some(n)) => format_count(*n),
            Self::Bytes(Some(n)) => format_bytes(*n),
            Self::Cost(Some(c)) => {
                let symbol = if ascii { &c.ascii_symbol } else { &c.symbol };
                let figure = format_cost(c.micros, &text::display_safe(symbol));
                if c.estimate {
                    format!("~{figure}")
                } else {
                    figure
                }
            }
            _ => unknown_value(ascii).to_string(),
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Self::Text(_) => 0,
            Self::Duration(_) => 1,
            Self::Tokens(_) => 2,
            Self::Count(_) => 3,
            Self::Bytes(_) => 4,
            Self::Cost(_) => 5,
        }
    }
}

/// What a column of values adds up to, and whether every row reported.
struct Sum {
    value: ReceiptValue,
    /// Some rows reported and some did not: the figure is a floor.
    partial: bool,
}

/// Add up a column. Mixed kinds, text, and costs in different currencies
/// have no total (`None`): a sum of unlike things is not a fact.
fn sum(values: &[&ReceiptValue]) -> Option<Sum> {
    let first = values.first()?;
    let kind = first.kind();
    if kind == 0 || values.iter().any(|v| v.kind() != kind) {
        return None;
    }
    let known = values.iter().filter(|v| !v.is_unknown()).count();
    let partial = known > 0 && known < values.len();
    let fold = |pick: &dyn Fn(&ReceiptValue) -> Option<u128>| -> Option<u128> {
        let mut total = None;
        for v in values {
            if let Some(n) = pick(v) {
                total = Some(total.unwrap_or(0u128).saturating_add(n));
            }
        }
        total
    };
    let clamp = |n: u128| u64::try_from(n).unwrap_or(u64::MAX);
    let value = match first {
        ReceiptValue::Duration(_) => {
            let total = fold(&|v| match v {
                ReceiptValue::Duration(Some(d)) => Some(d.as_nanos()),
                _ => None,
            });
            ReceiptValue::Duration(total.map(|n| {
                let secs = u64::try_from(n / 1_000_000_000).unwrap_or(u64::MAX);
                Duration::new(secs, (n % 1_000_000_000) as u32)
            }))
        }
        ReceiptValue::Tokens(_) | ReceiptValue::Count(_) | ReceiptValue::Bytes(_) => {
            let total = fold(&|v| match v {
                ReceiptValue::Tokens(Some(n))
                | ReceiptValue::Count(Some(n))
                | ReceiptValue::Bytes(Some(n)) => Some(u128::from(*n)),
                _ => None,
            })
            .map(clamp);
            match first {
                ReceiptValue::Tokens(_) => ReceiptValue::Tokens(total),
                ReceiptValue::Count(_) => ReceiptValue::Count(total),
                _ => ReceiptValue::Bytes(total),
            }
        }
        ReceiptValue::Cost(_) => {
            let mut symbol: Option<&Cost> = None;
            for v in values {
                if let ReceiptValue::Cost(Some(c)) = v {
                    match symbol {
                        None => symbol = Some(c),
                        Some(s) if s.symbol == c.symbol => {}
                        Some(_) => return None,
                    }
                }
            }
            let total = fold(&|v| match v {
                ReceiptValue::Cost(Some(c)) => Some(u128::from(c.micros)),
                _ => None,
            });
            ReceiptValue::Cost(match (symbol, total) {
                (Some(s), Some(n)) => Some(Cost {
                    micros: clamp(n),
                    symbol: s.symbol.clone(),
                    ascii_symbol: s.ascii_symbol.clone(),
                    estimate: values.iter().any(|v| v.is_estimate()),
                }),
                _ => None,
            })
        }
        ReceiptValue::Text(_) => return None,
    };
    Some(Sum { value, partial })
}

// ---------------------------------------------------------------------------
// Words, densities, rows
// ---------------------------------------------------------------------------

/// The words a receipt prints beyond its caller's text. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptWords {
    /// After the unknown mark in the legend: `— not reported`.
    pub unknown: Cow<'static, str>,
    /// After `~` in the legend: `~ estimated`.
    pub estimate: Cow<'static, str>,
    /// After `≥` in the legend: `≥ some rows did not report`.
    pub partial: Cow<'static, str>,
    /// The totals row's label.
    pub total: Cow<'static, str>,
    /// The header over the state-word column.
    pub outcome: Cow<'static, str>,
    /// `+3 more`, when rows do not fit.
    pub more: Cow<'static, str>,
}

impl Default for ReceiptWords {
    fn default() -> Self {
        Self {
            unknown: Cow::Borrowed("not reported"),
            estimate: Cow::Borrowed("estimated"),
            partial: Cow::Borrowed("some rows did not report"),
            total: Cow::Borrowed("Total"),
            outcome: Cow::Borrowed("Outcome"),
            more: Cow::Borrowed("more"),
        }
    }
}

/// How much a table shows, chosen by width ([`ReceiptDensity::for_width`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceiptDensity {
    /// Header, every column, the state word.
    Full,
    /// The essential columns and the state word.
    Compact,
    /// The mark, the label and the first essential column.
    Minimal,
}

impl ReceiptDensity {
    /// 64 columns and up are `Full`, 44 and up `Compact`, narrower
    /// `Minimal`.
    #[must_use]
    pub const fn for_width(width: u16) -> Self {
        if width >= 64 {
            Self::Full
        } else if width >= 44 {
            Self::Compact
        } else {
            Self::Minimal
        }
    }
}

/// One value column: its header (the unit, once) and whether it survives the
/// narrower densities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptColumn {
    pub header: Cow<'static, str>,
    pub essential: bool,
}

impl ReceiptColumn {
    #[must_use]
    pub fn new(header: impl Into<Cow<'static, str>>) -> Self {
        Self {
            header: header.into(),
            essential: false,
        }
    }

    /// Keep this column in `Compact` and `Minimal`.
    #[must_use]
    pub fn essential(mut self) -> Self {
        self.essential = true;
        self
    }
}

/// One row: a state's mark, a label, the state's word, then values.
///
/// `✓ Edited summary.md   Done   4 s   $0.01`. As a [`Paint`] it draws one
/// row with each value as wide as it is; put several in a [`ReceiptTable`]
/// to align them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub state: State,
    pub label: Cow<'static, str>,
    /// The state's word; defaults to [`State::word`].
    pub word: Cow<'static, str>,
    pub values: Vec<ReceiptValue>,
    /// Draws `→`: there is a detail to open.
    pub opens: bool,
}

impl Receipt {
    #[must_use]
    pub fn new(state: State, label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            state,
            label: label.into(),
            word: Cow::Borrowed(state.word()),
            values: Vec::new(),
            opens: false,
        }
    }

    /// The state's word from the host's [`StateWords`].
    #[must_use]
    pub fn with_words(mut self, words: &StateWords) -> Self {
        self.word = Cow::Owned(words.get(self.state).to_owned());
        self
    }

    #[must_use]
    pub fn word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.word = word.into();
        self
    }

    #[must_use]
    pub fn value(mut self, value: ReceiptValue) -> Self {
        self.values.push(value);
        self
    }

    #[must_use]
    pub fn values(mut self, values: impl IntoIterator<Item = ReceiptValue>) -> Self {
        self.values.extend(values);
        self
    }

    #[must_use]
    pub fn opens(mut self) -> Self {
        self.opens = true;
        self
    }
}

/// A value split at its alignment point.
struct Split {
    lead: String,
    tail: String,
    role: Role,
}

fn split(value: &ReceiptValue, ascii: bool) -> Split {
    let shown = value.render(ascii);
    let role = if value.is_unknown() {
        Role::Hint
    } else if value.is_estimate() {
        Role::Muted
    } else {
        Role::Foreground
    };
    let at = match value.align() {
        Align::End => shown.len(),
        Align::Point => shown.find('.').unwrap_or_else(|| {
            shown
                .rfind(|c: char| c.is_ascii_digit())
                .map_or(shown.len(), |i| i + 1)
        }),
    };
    Split {
        lead: shown[..at].to_string(),
        tail: shown[at..].to_string(),
        role,
    }
}

/// The widths of one value column.
#[derive(Clone, Copy, Default)]
struct Slot {
    lead: usize,
    tail: usize,
}

impl Slot {
    fn width(self) -> usize {
        self.lead + self.tail
    }

    fn take(&mut self, s: &Split) {
        self.lead = self.lead.max(text::width(&s.lead));
        self.tail = self.tail.max(text::width(&s.tail));
    }
}

/// One row of the grid, before it is spans.
struct GridRow {
    mark: Option<(&'static str, Role)>,
    label: String,
    label_role: Role,
    bold: bool,
    word: String,
    cells: Vec<Option<Split>>,
    opens: bool,
}

/// Everything the layout needs, borrowed from a table or a single receipt.
struct Grid<'a> {
    columns: &'a [ReceiptColumn],
    rows: &'a [Receipt],
    totals: Option<&'a Vec<Option<(ReceiptValue, bool)>>>,
    header: bool,
    label_header: &'a str,
    density: Option<ReceiptDensity>,
    words: &'a ReceiptWords,
    /// Stretch the label column to the area (a lone row); otherwise it is as
    /// wide as the longest label, so values stay near what they describe.
    fill: bool,
}

fn pad_start(s: &str, w: usize) -> String {
    format!("{}{s}", " ".repeat(w.saturating_sub(text::width(s))))
}

fn pad_end(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(text::width(s))))
}

/// What a laid-out grid needs to paint.
struct Layout {
    lines: Vec<(Line<'static>, Option<Role>)>,
    /// Rows of data (not header, totals or legend) in `lines`.
    legend: Option<String>,
}

impl Grid<'_> {
    fn density(&self, width: u16) -> ReceiptDensity {
        self.density
            .unwrap_or_else(|| ReceiptDensity::for_width(width))
    }

    /// Which value columns this density keeps, in order.
    fn visible(&self, density: ReceiptDensity) -> Vec<usize> {
        let all = 0..self.columns.len();
        match density {
            ReceiptDensity::Full => all.collect(),
            ReceiptDensity::Compact => all.filter(|i| self.columns[*i].essential).collect(),
            ReceiptDensity::Minimal => {
                let first = all
                    .clone()
                    .find(|i| self.columns[*i].essential)
                    .or_else(|| all.into_iter().next());
                first.into_iter().collect()
            }
        }
    }

    /// Lines, one per row, at `width`, plus the legend text when one applies.
    fn layout(&self, width: u16, theme: &Theme) -> Layout {
        let ascii = theme.ascii();
        let density = self.density(width);
        let mut cols = self.visible(density);
        let mut show_word = density != ReceiptDensity::Minimal;
        let show_header = self.header && density == ReceiptDensity::Full;
        let any_opens = density != ReceiptDensity::Minimal && self.rows.iter().any(|r| r.opens);
        let gap = if density == ReceiptDensity::Full {
            2
        } else {
            1
        };
        let width = usize::from(width);

        let mut grid: Vec<GridRow> = self
            .rows
            .iter()
            .map(|r| GridRow {
                mark: Some((glyphs::pick(r.state.glyph(), ascii), r.state.role())),
                label: text::display_safe(&r.label).into_owned(),
                label_role: Role::Foreground,
                bold: false,
                word: text::display_safe(&r.word).into_owned(),
                cells: (0..self.columns.len())
                    .map(|i| r.values.get(i).map(|v| split(v, ascii)))
                    .collect(),
                opens: r.opens,
            })
            .collect();
        if let Some(totals) = self.totals {
            grid.push(GridRow {
                mark: None,
                label: text::display_safe(&self.words.total).into_owned(),
                label_role: Role::Foreground,
                bold: true,
                word: String::new(),
                cells: (0..self.columns.len())
                    .map(|i| {
                        totals.get(i).cloned().flatten().map(|(v, p)| {
                            let mut s = split(&v, ascii);
                            if p {
                                let floor = if ascii { ">= " } else { "\u{2265} " };
                                s.lead.insert_str(0, floor);
                            }
                            s
                        })
                    })
                    .collect(),
                opens: false,
            });
        }

        let unknown = split(&ReceiptValue::Tokens(None), ascii);
        let slots = |cols: &[usize]| -> Vec<Slot> {
            cols.iter()
                .map(|i| {
                    let mut slot = Slot::default();
                    for row in &grid {
                        match row.cells.get(*i) {
                            Some(Some(s)) => slot.take(s),
                            _ => slot.take(&unknown),
                        }
                    }
                    let header = text::width(&self.columns[*i].header);
                    if show_header && header > slot.width() {
                        slot.lead += header - slot.width();
                    }
                    slot
                })
                .collect()
        };
        let word_w = |show: bool| -> usize {
            if !show {
                return 0;
            }
            let rows = grid.iter().map(|r| text::width(&r.word));
            let head = if show_header {
                text::width(&self.words.outcome)
            } else {
                0
            };
            rows.chain(Some(head)).max().unwrap_or(0)
        };
        let fixed = |slots: &[Slot], show_word: bool| -> usize {
            let values: usize = slots.iter().map(|s| gap + s.width()).sum();
            let word = if show_word { gap + word_w(true) } else { 0 };
            let arrow = if any_opens { gap + 1 } else { 0 };
            2 + values + word + arrow
        };
        let mut slots_now = slots(&cols);
        // The label keeps room before anything else is shown: narrower than
        // that, drop the last value column, then the state word.
        while width.saturating_sub(fixed(&slots_now, show_word)) < MIN_LABEL
            && (!cols.is_empty() || show_word)
        {
            if cols.len() > 1 {
                cols.pop();
            } else if show_word {
                show_word = false;
            } else {
                cols.pop();
            }
            slots_now = slots(&cols);
        }
        let word_cells = word_w(show_word);
        let natural = grid
            .iter()
            .map(|r| text::width(&r.label))
            .chain(show_header.then(|| text::width(self.label_header)))
            .max()
            .unwrap_or(1);
        let room = width.saturating_sub(fixed(&slots_now, show_word));
        let label_w = if self.fill { room } else { room.min(natural) }.max(1);

        let compose = |row: &GridRow, theme: &Theme| -> Line<'static> {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let weight = if row.bold {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            match row.mark {
                Some((glyph, role)) => {
                    spans.push(Span::styled(glyph, theme.fg(role)));
                    spans.push(Span::raw(" "));
                }
                None => spans.push(Span::raw("  ")),
            }
            let label = text::pad(&row.label, label_w, ascii);
            spans.push(Span::styled(
                label,
                theme.fg(row.label_role).add_modifier(weight),
            ));
            if show_word {
                spans.push(Span::raw(" ".repeat(gap)));
                spans.push(Span::styled(
                    pad_end(&row.word, word_cells),
                    theme.fg(Role::Muted),
                ));
            }
            for (slot, i) in slots_now.iter().zip(&cols) {
                spans.push(Span::raw(" ".repeat(gap)));
                let cell = match row.cells.get(*i) {
                    Some(Some(s)) => s,
                    _ => &unknown,
                };
                let cell_text = format!(
                    "{}{}",
                    pad_start(&cell.lead, slot.lead),
                    pad_end(&cell.tail, slot.tail)
                );
                spans.push(Span::styled(
                    cell_text,
                    theme.fg(cell.role).add_modifier(weight),
                ));
            }
            if any_opens {
                spans.push(Span::raw(" ".repeat(gap)));
                let arrow = if row.opens {
                    if ascii { ">" } else { "\u{2192}" }
                } else {
                    " "
                };
                spans.push(Span::styled(arrow, theme.fg(Role::Muted)));
            }
            Line::from(spans)
        };

        let mut lines = Vec::new();
        if show_header {
            let mut spans = vec![Span::raw("  ")];
            spans.push(Span::styled(
                text::pad(self.label_header, label_w, ascii),
                theme.fg(Role::Muted),
            ));
            if show_word {
                spans.push(Span::raw(" ".repeat(gap)));
                spans.push(Span::styled(
                    pad_end(&text::display_safe(&self.words.outcome), word_cells),
                    theme.fg(Role::Muted),
                ));
            }
            for (slot, i) in slots_now.iter().zip(&cols) {
                spans.push(Span::raw(" ".repeat(gap)));
                let header = text::display_safe(&self.columns[*i].header);
                spans.push(Span::styled(
                    pad_start(&header, slot.width()),
                    theme.fg(Role::Muted),
                ));
            }
            lines.push((Line::from(spans), None));
        }
        for row in &grid {
            lines.push((compose(row, theme), None));
        }

        // The legend names every mark the table used, so none is a puzzle.
        let used = |f: &dyn Fn(&ReceiptValue) -> bool| {
            self.rows
                .iter()
                .flat_map(|r| cols.iter().filter_map(|i| r.values.get(*i)))
                .any(f)
        };
        let partial = self.totals.is_some_and(|t| {
            cols.iter()
                .any(|i| matches!(t.get(*i), Some(Some((_, true)))))
        });
        let any_missing = self
            .rows
            .iter()
            .any(|r| cols.iter().any(|i| r.values.get(*i).is_none()));
        let mut legend = Vec::new();
        if used(&|v| v.is_unknown()) || any_missing {
            legend.push(format!(
                "{unknown} {}",
                text::display_safe(&self.words.unknown),
                unknown = unknown_value(ascii)
            ));
        }
        if used(&|v| v.is_estimate()) {
            legend.push(format!("~ {}", text::display_safe(&self.words.estimate)));
        }
        if partial {
            let floor = if ascii { ">=" } else { "\u{2265}" };
            legend.push(format!(
                "{floor} {}",
                text::display_safe(&self.words.partial)
            ));
        }
        // Not ` - `: that is the unknown mark's own ASCII form.
        let sep = if ascii { "  " } else { " \u{b7} " };
        let legend = (!legend.is_empty()).then(|| legend.join(sep));
        Layout { lines, legend }
    }
}

/// A label never gets less than this before columns are dropped.
const MIN_LABEL: usize = 10;

fn paint_grid(grid: &Grid, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    let layout = grid.layout(area.width, theme);
    let legend = layout.legend.map(|l| {
        let shown = text::truncate(&l, usize::from(area.width).saturating_sub(2), theme.ascii());
        Line::from(vec![
            Span::raw("  "),
            Span::styled(shown.into_owned(), theme.fg(Role::Hint)),
        ])
    });
    let total = layout.lines.len();
    let rows = usize::from(area.height);
    let totals_rows = usize::from(grid.totals.is_some());
    // Rows that do not fit end in `+n more` with the real n; the totals row
    // and the legend are the last things to go.
    let reserve = totals_rows + usize::from(legend.is_some());
    let header_rows = total - grid.rows.len() - totals_rows;
    let body_fit = rows.saturating_sub(header_rows + reserve);
    let mut y = area.y;
    let mut put = |line: &Line<'static>, y: &mut u16| {
        if *y < area.bottom() {
            line.clone()
                .render(Rect::new(area.x, *y, area.width, 1), buf);
            *y += 1;
        }
    };
    for line in layout.lines.iter().take(header_rows) {
        put(&line.0, &mut y);
    }
    let body: Vec<_> = layout
        .lines
        .iter()
        .skip(header_rows)
        .take(grid.rows.len())
        .collect();
    if body.len() <= body_fit {
        for line in &body {
            put(&line.0, &mut y);
        }
    } else if body_fit > 0 {
        let shown = body_fit - 1;
        for line in body.iter().take(shown) {
            put(&line.0, &mut y);
        }
        let more = format!(
            "+{} {}",
            body.len() - shown,
            text::display_safe(&grid.words.more)
        );
        let more = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                text::truncate(
                    &more,
                    usize::from(area.width).saturating_sub(2),
                    theme.ascii(),
                )
                .into_owned(),
                theme.fg(Role::Muted),
            ),
        ]);
        put(&more, &mut y);
    }
    if totals_rows == 1
        && let Some(line) = layout.lines.last()
    {
        put(&line.0, &mut y);
    }
    if let Some(legend) = &legend {
        put(legend, &mut y);
    }
}

fn grid_height(grid: &Grid, width: u16, theme: &Theme) -> u16 {
    let layout = grid.layout(width, theme);
    let rows = layout.lines.len() + usize::from(layout.legend.is_some());
    u16::try_from(rows).unwrap_or(u16::MAX)
}

impl Paint for Receipt {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let columns = vec![ReceiptColumn::new("").essential(); self.values.len()];
        let words = ReceiptWords::default();
        let grid = Grid {
            columns: &columns,
            rows: std::slice::from_ref(self),
            totals: None,
            header: false,
            label_header: "",
            density: None,
            words: &words,
            fill: true,
        };
        paint_grid(&grid, area, buf, theme);
    }
}

/// Many receipts in aligned columns, with an optional header and totals row.
///
/// Numbers are right-aligned on their unit or decimal point. The header
/// carries each unit once. Width picks the [`ReceiptDensity`]: the full table
/// down to a label and one number. Rows that do not fit end in `+n more`
/// with the real count; the totals row is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptTable {
    pub columns: Vec<ReceiptColumn>,
    pub rows: Vec<Receipt>,
    /// The header over the label column (`Action`).
    pub label_header: Cow<'static, str>,
    pub header: bool,
    pub totals: bool,
    /// `None` picks by width.
    pub density: Option<ReceiptDensity>,
    pub words: ReceiptWords,
}

impl ReceiptTable {
    #[must_use]
    pub fn new(columns: Vec<ReceiptColumn>, rows: Vec<Receipt>) -> Self {
        Self {
            columns,
            rows,
            label_header: Cow::Borrowed(""),
            header: true,
            totals: false,
            density: None,
            words: ReceiptWords::default(),
        }
    }

    #[must_use]
    pub fn label_header(mut self, header: impl Into<Cow<'static, str>>) -> Self {
        self.label_header = header.into();
        self
    }

    #[must_use]
    pub fn header(mut self, header: bool) -> Self {
        self.header = header;
        self
    }

    /// Add a totals row: each column's sum where its values are all one kind.
    /// A column where some rows did not report shows `≥` and the legend says
    /// so; a column where none did shows `—`.
    #[must_use]
    pub fn totals(mut self) -> Self {
        self.totals = true;
        self
    }

    #[must_use]
    pub fn density(mut self, density: ReceiptDensity) -> Self {
        self.density = Some(density);
        self
    }

    #[must_use]
    pub fn words(mut self, words: ReceiptWords) -> Self {
        self.words = words;
        self
    }

    /// The sums the totals row shows, per column: `(value, partial)`, or
    /// `None` where the column has no total. A missing cell counts as not
    /// reported.
    #[must_use]
    pub fn column_totals(&self) -> Vec<Option<(ReceiptValue, bool)>> {
        (0..self.columns.len())
            .map(|i| {
                let present: Vec<&ReceiptValue> =
                    self.rows.iter().filter_map(|r| r.values.get(i)).collect();
                let missing = self.rows.len() - present.len();
                let total = sum(&present)?;
                // A row with no cell at all did not report either.
                let partial = total.partial || (missing > 0 && !total.value.is_unknown());
                Some((total.value, partial))
            })
            .collect()
    }

    fn grid<'a>(&'a self, totals: Option<&'a Vec<Option<(ReceiptValue, bool)>>>) -> Grid<'a> {
        Grid {
            columns: &self.columns,
            rows: &self.rows,
            totals,
            header: self.header,
            label_header: &self.label_header,
            density: self.density,
            words: &self.words,
            fill: false,
        }
    }
}

impl Paint for ReceiptTable {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let totals = self.totals.then(|| self.column_totals());
        paint_grid(&self.grid(totals.as_ref()), area, buf, theme);
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        let totals = self.totals.then(|| self.column_totals());
        grid_height(&self.grid(totals.as_ref()), width, theme)
    }
}

/// Localized words for [`TurnSummary`]. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnSummaryWords {
    /// Lead phrase before the elapsed duration (for example `"Worked for"`).
    pub worked_for: Cow<'static, str>,
    /// Singular step word (`"1 step"`).
    pub step_one: Cow<'static, str>,
    /// Plural step word (`"6 steps"`).
    pub step_many: Cow<'static, str>,
    /// Suffix after a formatted token count (`"12.4k tokens"`).
    pub tokens: Cow<'static, str>,
}

impl Default for TurnSummaryWords {
    fn default() -> Self {
        Self {
            worked_for: Cow::Borrowed("Worked for"),
            step_one: Cow::Borrowed("step"),
            step_many: Cow::Borrowed("steps"),
            tokens: Cow::Borrowed("tokens"),
        }
    }
}

/// A compact one-line settled turn summary (`Worked for 2m 14s · 6 steps`),
/// with optional token and cost receipts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnSummary<'a> {
    pub duration: Duration,
    pub steps: usize,
    pub tokens: Option<u64>,
    pub cost: Option<Cost>,
    pub words: Cow<'a, TurnSummaryWords>,
}

impl<'a> TurnSummary<'a> {
    /// Construct a turn summary for a settled duration and step count.
    #[must_use]
    pub fn new(duration: Duration, steps: usize) -> Self {
        Self {
            duration,
            steps,
            tokens: None,
            cost: None,
            words: Cow::Owned(TurnSummaryWords::default()),
        }
    }

    /// Attach a reported token count.
    #[must_use]
    pub fn tokens(mut self, tokens: u64) -> Self {
        self.tokens = Some(tokens);
        self
    }

    /// Attach a reported cost.
    #[must_use]
    pub fn cost(mut self, cost: Cost) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Override the localized words.
    #[must_use]
    pub fn words(mut self, words: TurnSummaryWords) -> Self {
        self.words = Cow::Owned(words);
        self
    }

    /// Format the summary text for Unicode or ASCII-safe terminals.
    #[must_use]
    pub fn render_text(&self, ascii: bool) -> String {
        let sep = if ascii { " - " } else { " \u{00b7} " };
        let lead = text::display_safe(&self.words.worked_for);
        let mut parts = vec![format!("{lead} {}", format_duration(self.duration))];
        if self.steps > 0 {
            let word = if self.steps == 1 {
                text::display_safe(&self.words.step_one)
            } else {
                text::display_safe(&self.words.step_many)
            };
            parts.push(format!("{} {word}", format_count(self.steps as u64)));
        }
        if let Some(tokens) = self.tokens {
            let word = text::display_safe(&self.words.tokens);
            parts.push(format!("{} {word}", format_tokens(tokens)));
        }
        if let Some(cost) = &self.cost {
            parts.push(ReceiptValue::Cost(Some(cost.clone())).render(ascii));
        }
        parts.join(sep)
    }
}

impl Paint for TurnSummary<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let text = self.render_text(ascii);
        let clipped = text::truncate_words(&text, usize::from(area.width), ascii);
        super::workbench::row(
            Rect { height: 1, ..area },
            buf,
            &Line::from(Span::styled(clipped.into_owned(), theme.fg(Role::Muted))),
        );
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        u16::from(width > 0)
    }
}
