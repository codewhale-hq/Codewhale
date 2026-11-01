//! A flat shelf of caller-reported results, without a filesystem or run loop.
//!
//! Names lead, actions remain discoverable, and kind, state and receipt facts
//! share a compact second row. The caller owns selection and what an action
//! does. Rows also implement [`ListRow`] for existing selectable host lists.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    ListRow, ListRowState, ListState, Paint, ReceiptValue, Role, State, StatusMark, Theme, glyphs,
    text,
};

use super::workbench::row;

/// The kind reported by the host; no path, URL or command is inspected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    File,
    Review,
    Run,
    Link,
}

/// Localized shelf and kind words. State words come from [`StatusMark`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactWords {
    pub file: Cow<'static, str>,
    pub review: Cow<'static, str>,
    pub run: Cow<'static, str>,
    pub link: Cow<'static, str>,
    pub empty: Cow<'static, str>,
    pub omitted: Cow<'static, str>,
}

impl Default for ArtifactWords {
    fn default() -> Self {
        Self {
            file: Cow::Borrowed("File"),
            review: Cow::Borrowed("Review"),
            run: Cow::Borrowed("Run"),
            link: Cow::Borrowed("Link"),
            empty: Cow::Borrowed("No artifacts reported"),
            omitted: Cow::Borrowed("artifacts not fully shown"),
        }
    }
}

impl ArtifactWords {
    fn kind(&self, kind: ArtifactKind) -> &str {
        match kind {
            ArtifactKind::File => &self.file,
            ArtifactKind::Review => &self.review,
            ArtifactKind::Run => &self.run,
            ArtifactKind::Link => &self.link,
        }
    }
}

/// A resulting file, review, run or link over caller-owned facts.
///
/// `detail` may be a purpose, a relative path, or a review summary. It is
/// display text, never a destination this component opens. An unknown receipt
/// remains unknown through [`ReceiptValue::render`], rather than becoming zero.
#[derive(Clone, Debug)]
pub struct Artifact<'a> {
    pub name: Cow<'a, str>,
    pub kind: ArtifactKind,
    pub status: StatusMark,
    pub detail: Option<Cow<'a, str>>,
    pub action: Option<Cow<'a, str>>,
    pub receipt: Option<(Cow<'a, str>, ReceiptValue)>,
    pub words: Option<&'a ArtifactWords>,
}

impl<'a> Artifact<'a> {
    #[must_use]
    pub fn new(name: impl Into<Cow<'a, str>>, kind: ArtifactKind, state: State) -> Self {
        Self {
            name: name.into(),
            kind,
            status: StatusMark::new(state),
            detail: None,
            action: None,
            receipt: None,
            words: None,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<Cow<'a, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn action(mut self, action: impl Into<Cow<'a, str>>) -> Self {
        self.action = Some(action.into());
        self
    }

    #[must_use]
    pub fn receipt(mut self, label: impl Into<Cow<'a, str>>, value: ReceiptValue) -> Self {
        self.receipt = Some((label.into(), value));
        self
    }

    #[must_use]
    pub fn status_word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.status = self.status.word(word);
        self
    }

    #[must_use]
    pub fn words(mut self, words: &'a ArtifactWords) -> Self {
        self.words = Some(words);
        self
    }

    fn draw(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        selected: bool,
        words: &ArtifactWords,
    ) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let words = self.words.unwrap_or(words);
        let width = usize::from(area.width);
        let name = safe(&self.name, theme.ascii());
        let action = self
            .action
            .as_deref()
            .map(|a| safe(a, theme.ascii()))
            .filter(|a| !a.is_empty());
        // The action gets at most a third of a useful row; the result's name
        // keeps priority. Both cuts remain visible through the shared ellipsis.
        let action = action
            .filter(|_| width >= 12)
            .map(|a| text::truncate(&a, width / 3, theme.ascii()).into_owned());
        let action_width = action.as_deref().map_or(0, |a| text::width(a) + 2);
        let name = text::truncate(&name, width.saturating_sub(action_width), theme.ascii());
        let ink = if selected {
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Role::Foreground)
        };
        row(
            Rect { height: 1, ..area },
            buf,
            &Line::from(Span::styled(name.into_owned(), ink)),
        );
        if let Some(action) = action {
            let width = u16::try_from(text::width(&action))
                .unwrap_or(area.width)
                .min(area.width);
            row(
                Rect {
                    x: area.right().saturating_sub(width),
                    width,
                    height: 1,
                    ..area
                },
                buf,
                &Line::from(Span::styled(action, theme.fg(Role::Accent))),
            );
        }
        if area.height < 2 {
            return;
        }
        let separator = format!(" {} ", glyphs::pick(glyphs::NEUTRAL, theme.ascii()));
        let mut facts: Vec<_> = self
            .status
            .spans(theme)
            .into_iter()
            .map(|s| Span::styled(safe(&s.content, theme.ascii()), s.style))
            .collect();
        facts.push(Span::styled(separator.clone(), theme.fg(Role::Dim)));
        facts.push(Span::styled(
            safe(words.kind(self.kind), theme.ascii()),
            theme.fg(Role::Muted),
        ));
        if let Some((label, value)) = &self.receipt {
            facts.push(Span::styled(separator.clone(), theme.fg(Role::Dim)));
            let label = safe(label, theme.ascii());
            if !label.is_empty() {
                facts.push(Span::styled(format!("{label} "), theme.fg(Role::Muted)));
            }
            let role = if value.is_unknown() {
                Role::Hint
            } else {
                Role::Foreground
            };
            facts.push(Span::styled(
                safe(&value.render(theme.ascii()), theme.ascii()),
                theme.fg(role),
            ));
        }
        if let Some(detail) = &self.detail {
            facts.push(Span::styled(separator, theme.fg(Role::Dim)));
            facts.push(Span::styled(
                safe(detail, theme.ascii()),
                theme.fg(Role::Muted),
            ));
        }
        row(
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
            buf,
            &Line::from(fit(facts, width, theme)),
        );
    }
}

impl Paint for Artifact<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.draw(area, buf, theme, false, &ArtifactWords::default());
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        if width == 0 { 0 } else { 2 }
    }
}

impl ListRow for Artifact<'_> {
    fn height(&self, _width: u16) -> u16 {
        2
    }

    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
        self.draw(area, buf, theme, state.selected, &ArtifactWords::default());
    }
}

/// A flat, selectable artifact shelf. Selection and scroll offset belong to
/// the caller; existing [`ListState`] supplies the viewport arithmetic.
///
/// The omission rail counts supplied rows not completely visible vertically
/// plus `omitted_artifacts` already withheld by the host. Horizontal cuts carry
/// their own ellipsis. At a one-row viewport an omission count takes priority.
#[derive(Clone, Debug)]
pub struct ArtifactShelf<'a> {
    pub artifacts: Vec<Artifact<'a>>,
    pub selected: Option<usize>,
    pub offset: usize,
    pub focused: bool,
    pub omitted_artifacts: usize,
    pub words: Option<&'a ArtifactWords>,
}

impl<'a> ArtifactShelf<'a> {
    #[must_use]
    pub fn new(artifacts: Vec<Artifact<'a>>) -> Self {
        Self {
            artifacts,
            selected: None,
            offset: 0,
            focused: false,
            omitted_artifacts: 0,
            words: None,
        }
    }

    #[must_use]
    pub fn selected(mut self, selected: usize) -> Self {
        self.selected = Some(selected);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn omitted(mut self, omitted: usize) -> Self {
        self.omitted_artifacts = omitted;
        self
    }

    #[must_use]
    pub fn words(mut self, words: &'a ArtifactWords) -> Self {
        self.words = Some(words);
        self
    }

    fn placement(&self, area: Rect) -> (usize, u16, bool) {
        let selected = self.selected.filter(|&i| i < self.artifacts.len());
        let offset_for = |height| {
            if let Some(selected) = selected {
                ListState {
                    selected,
                    offset: self.offset,
                }
                .visible_offset(self.artifacts.len(), height, |_| 2)
            } else {
                self.offset.min(self.artifacts.len().saturating_sub(1))
            }
        };
        let offset = offset_for(area.height);
        let overflow = self.omitted_artifacts > 0
            || offset > 0
            || self
                .artifacts
                .len()
                .saturating_sub(offset)
                .saturating_mul(2)
                > usize::from(area.height);
        let body = area.height.saturating_sub(u16::from(overflow));
        let offset = if overflow { offset_for(body) } else { offset };
        (offset, body, overflow)
    }

    /// The caller can route a pointer click to the same action as a key.
    /// Pass the actual visible viewport, intersected with the paint buffer.
    #[must_use]
    pub fn row_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        if area.is_empty() || !area.contains((column, row).into()) {
            return None;
        }
        let (offset, body, _) = self.placement(area);
        let y = row - area.y;
        if y >= body {
            return None;
        }
        let index = offset.saturating_add(usize::from(y / 2));
        (index < self.artifacts.len()).then_some(index)
    }
}

impl Paint for ArtifactShelf<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let default_words = ArtifactWords::default();
        let words = self.words.unwrap_or(&default_words);
        if self.artifacts.is_empty() && self.omitted_artifacts == 0 {
            let empty = safe(&words.empty, theme.ascii());
            row(
                Rect { height: 1, ..area },
                buf,
                &Line::from(Span::styled(
                    text::truncate_words(&empty, usize::from(area.width), theme.ascii())
                        .into_owned(),
                    theme.fg(Role::Hint),
                )),
            );
            return;
        }
        let (offset, body_height, overflow) = self.placement(area);
        let gutter = area.width.min(2);
        let mut painted = 0usize;
        let mut y = 0u16;
        for (index, artifact) in self.artifacts.iter().enumerate().skip(offset) {
            if y >= body_height {
                break;
            }
            let height = 2.min(body_height - y);
            let row_area = Rect {
                y: area.y + y,
                height,
                ..area
            };
            let selected = self.selected == Some(index);
            if selected {
                buf.set_style(row_area, theme.bg(Role::Selected));
                let marker = glyphs::pick(glyphs::SELECTION, theme.ascii());
                let role = if self.focused {
                    Role::Accent
                } else {
                    Role::Muted
                };
                row(
                    Rect {
                        width: gutter,
                        height: 1,
                        ..row_area
                    },
                    buf,
                    &Line::from(Span::styled(marker, theme.fg(role))),
                );
            }
            artifact.draw(
                Rect {
                    x: row_area.x + gutter,
                    width: row_area.width - gutter,
                    ..row_area
                },
                buf,
                theme,
                selected,
                words,
            );
            if height == 2 {
                painted += 1;
            }
            y += height;
        }
        if overflow {
            let hidden = self
                .artifacts
                .len()
                .saturating_sub(painted)
                .saturating_add(self.omitted_artifacts);
            let omitted = safe(&format!("{hidden} {}", words.omitted), theme.ascii());
            row(
                Rect {
                    y: area.y + body_height,
                    height: 1,
                    ..area
                },
                buf,
                &Line::from(Span::styled(
                    text::truncate_words(&omitted, usize::from(area.width), theme.ascii())
                        .into_owned(),
                    theme.fg(Role::Hint),
                )),
            );
        }
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        if width == 0 {
            return 0;
        }
        let rows = self
            .artifacts
            .len()
            .saturating_mul(2)
            .max(1)
            .saturating_add(usize::from(self.omitted_artifacts > 0));
        u16::try_from(rows).unwrap_or(u16::MAX)
    }
}

fn safe(value: &str, ascii: bool) -> String {
    let safe = text::display_safe(value);
    if !ascii {
        return safe.into_owned();
    }
    // ASCII-safe output never splits a combining sequence or a wide cluster.
    safe.graphemes(true)
        .map(|g| if g.is_ascii() { g } else { "?" })
        .collect()
}

/// Apply the shared grapheme-safe cut without throwing away semantic styles.
fn fit(spans: Vec<Span<'static>>, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    let source: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let cut = text::truncate(&source, width, theme.ascii());
    if cut == source {
        return spans;
    }
    let prefix = source
        .grapheme_indices(true)
        .zip(cut.graphemes(true))
        .take_while(|((_, a), b)| a == b)
        .map(|((at, g), _)| at + g.len())
        .last()
        .unwrap_or(0);
    let mut remaining = prefix;
    let mut fitted = Vec::new();
    for span in spans {
        let take = remaining.min(span.content.len());
        if take > 0 {
            fitted.push(Span::styled(span.content[..take].to_owned(), span.style));
        }
        remaining -= take;
        if remaining == 0 {
            break;
        }
    }
    if prefix < cut.len() {
        fitted.push(Span::styled(cut[prefix..].to_owned(), theme.fg(Role::Hint)));
    }
    fitted
}
