//! Full-screen pager overlay for long outputs.
//!
//! Vim-style key bindings (mirroring the codex pager_overlay):
//! - `j` / Down — scroll down one line
//! - `k` / Up — scroll up one line
//! - `g g` / Home — jump to top
//! - `G` / End — jump to bottom
//! - `Ctrl+D` — half-page down
//! - `Ctrl+U` — half-page up
//! - `Ctrl+F` / PageDown / Space — full page down
//! - `Ctrl+B` / PageUp / Shift+Space — full page up
//! - `/` — start search; `n` / `N` — next / previous match
//! - `c` / `y` — copy the pager's source text (or its attached copy payload)
//! - `a` — copy the attached final assistant answer (answer-carrying pagers)
//! - `e` — copy the attached turn handoff markdown (Turn Inspector only)
//! - `q` / Esc — close pager

use std::cell::{Cell, RefCell};
use std::ops::Range;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, action_footer_lines,
    render_modal_footer, render_panel_scroll_rail, render_underwater_surface,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_tui_kit::ui_text::grapheme_display_width;

#[derive(Debug, Clone)]
struct PagerDestructiveAction {
    key: char,
    label: String,
    confirm_label: String,
    event: ViewEvent,
    armed: bool,
}

const EVIDENCE_WINDOW_BYTES: usize = 128 * 1024;
const PAGER_RETAINED_ROWS: usize = 512;
const PAGER_ROW_BYTES: usize = 32 * 1024;

struct OwnedEvidence {
    workspace: std::path::PathBuf,
    session: String,
    reference: String,
    label: String,
    page: usize,
    body_start: usize,
    binary: bool,
    digest: Option<String>,
    total: u64,
    offset: u64,
    end: u64,
    previous: Vec<u64>,
    encoding: &'static str,
    ready: bool,
    pending: Option<(
        tokio_util::sync::CancellationToken,
        tokio::sync::oneshot::Receiver<Result<EvidenceWindow, ()>>,
    )>,
}

struct EvidenceWindow {
    binary: bool,
    text: String,
    copy: String,
    digest: String,
    total: u64,
    offset: u64,
    end: u64,
    encoding: &'static str,
}

impl Drop for OwnedEvidence {
    fn drop(&mut self) {
        if let Some((cancel, _)) = self.pending.take() {
            cancel.cancel();
        }
    }
}

impl OwnedEvidence {
    fn request(&mut self, offset: u64) {
        if let Some((cancel, _)) = self.pending.take() {
            cancel.cancel();
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let context = codewhale_engine::tools::ToolContext::new(self.workspace.clone())
            .with_state_namespace(self.session.clone())
            .with_cancel_token(cancel.clone());
        let mut input = serde_json::json!({
            "ref": self.reference,
            "mode": "bytes",
            "offset": offset,
            "length": EVIDENCE_WINDOW_BYTES,
            "max_bytes": EVIDENCE_WINDOW_BYTES,
        });
        if let Some(digest) = self.digest.as_ref() {
            input["expected_sha256"] = serde_json::json!(digest);
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        self.ready = false;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                use codewhale_engine::tools::spec::ToolSpec as _;
                let result = codewhale_engine::tools::tool_result_retrieval::RetrieveToolResultTool
                    .execute(input, &context)
                    .await
                    .map_err(|_| ())
                    .and_then(|result| evidence_window(&result.content, offset));
                let _ = send.send(result);
            });
        }
        self.pending = Some((cancel, receive));
    }
}

fn evidence_window(content: &str, requested: u64) -> Result<EvidenceWindow, ()> {
    use base64::Engine as _;
    if content.len()
        > EVIDENCE_WINDOW_BYTES * 2
            + codewhale_engine::tools::tool_result_retrieval::MAX_REFERENCE_BYTES * 6
    {
        return Err(());
    }
    let value: serde_json::Value = serde_json::from_str(content).map_err(|_| ())?;
    let total = value
        .get("total_bytes")
        .and_then(serde_json::Value::as_u64)
        .ok_or(())?;
    let offset = value
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .ok_or(())?;
    let digest = value
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    if value.get("mode").and_then(serde_json::Value::as_str) != Some("bytes")
        || value.get("encoding").and_then(serde_json::Value::as_str) != Some("base64")
        || offset != requested
        || total > 512 * 1024 * 1024
        || offset > total
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| ())?;
    if bytes.len() > EVIDENCE_WINDOW_BYTES
        || bytes.len() as u64 > total - offset
        || value
            .get("returned_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(bytes.len() as u64)
    {
        return Err(());
    }
    if bytes.is_empty() && offset < total {
        return Err(());
    }
    let text_prefix = match std::str::from_utf8(&bytes) {
        Ok(text) => Some(text),
        Err(error)
            if error.error_len().is_none()
                && error.valid_up_to() > 0
                && offset + (bytes.len() as u64) < total =>
        {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    };
    let (text, copy, end, encoding, actual_offset, binary) = if let Some(text) = text_prefix {
        let mut sanitized = String::with_capacity(text.len());
        codewhale_tui_kit::osc8::strip_ansi_into(text, &mut sanitized);
        let (copy, encoding) = if sanitized == text {
            (text.to_string(), "UTF8")
        } else {
            (
                base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
                "base64",
            )
        };
        (
            sanitized,
            copy,
            offset + text.len() as u64,
            encoding,
            offset,
            false,
        )
    } else {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        (
            encoded.clone(),
            encoded,
            offset + bytes.len() as u64,
            "base64",
            offset,
            true,
        )
    };
    Ok(EvidenceWindow {
        binary,
        text,
        copy,
        digest: digest.to_ascii_lowercase(),
        total,
        offset: actual_offset,
        end,
        encoding,
    })
}

/// One independently copyable document in a pager. Most pagers contain a
/// single page; Turn Inspector uses several so each turn stays isolated while
/// retaining the pager's existing scroll/search/copy behavior.
#[derive(Debug, Clone)]
pub(crate) struct PagerPage {
    title: String,
    /// What the page shows. Its display rows are wrapped from this to the
    /// width the body really gets at render time, so one row is one scroll
    /// step whatever the terminal width.
    source: PagerSource,
    /// Width to lay the page out at before its first render (`0`: unwrapped).
    first_width: usize,
    rows: RefCell<PagerRows>,
    export_markdown: Option<String>,
    copy_text: Option<String>,
    answer_text: Option<String>,
}

#[derive(Debug, Clone)]
enum PagerSource {
    /// Sanitized text; `c` / `y` copy it verbatim when no explicit copy
    /// payload is attached.
    Text(String),
    /// Pre-styled lines, wrapped with their styles intact.
    Lines(Vec<Line<'static>>),
    Diff(String),
}

#[derive(Debug, Clone, Default)]
pub(super) struct PagerRows {
    width: Option<usize>,
    pub(super) start: usize,
    pub(super) total: usize,
    pub(super) escaped: bool,
    pub(super) continued: bool,
    clipped_rows: usize,
    clipped_bytes: usize,
    plain: Vec<String>,
    pub(super) styled: Vec<Line<'static>>,
    pub(super) origin: Vec<usize>,
}

impl PagerRows {
    fn push(
        &mut self,
        cells: &[(char, usize)],
        range: Range<usize>,
        origin: usize,
        style: Option<&Line<'static>>,
    ) {
        let index = self.total;
        self.total = self.total.saturating_add(1);
        if index < self.start || self.plain.len() >= PAGER_RETAINED_ROWS {
            return;
        }
        let text: String = cells[range.clone()].iter().map(|&(ch, _)| ch).collect();
        if let Some(line) = style {
            let mut spans = Vec::new();
            let mut run = String::new();
            let mut run_span: Option<usize> = None;
            for &(ch, span) in &cells[range] {
                if run_span.is_some_and(|current| current != span) {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        line.spans[run_span.unwrap()].style,
                    ));
                }
                run_span = Some(span);
                run.push(ch);
            }
            if let Some(span) = run_span {
                spans.push(Span::styled(run, line.spans[span].style));
            }
            let mut row = Line::from(spans).style(line.style);
            row.alignment = line.alignment;
            self.styled.push(row);
        }
        self.plain.push(text);
        self.origin.push(origin);
    }

    fn wrap_text(source: &str, width: usize, start: usize) -> Self {
        let mut rows = Self {
            width: Some(width),
            start,
            ..Self::default()
        };
        for (origin, raw) in source.lines().enumerate() {
            let (escaped, continued) =
                for_each_wrapped_range(std::iter::once((raw, 0)), width, |cells, range| {
                    rows.push(cells, range, origin, None);
                });
            rows.escaped |= escaped;
            rows.continued |= continued;
        }
        rows
    }

    #[cfg(test)]
    pub(super) fn wrap_lines(lines: &[Line<'static>], width: usize) -> Self {
        Self::wrap_lines_window(lines, width, 0)
    }

    pub(super) fn wrap_lines_window(lines: &[Line<'static>], width: usize, start: usize) -> Self {
        let mut rows = Self {
            width: Some(width),
            start,
            ..Self::default()
        };
        for (origin, line) in lines.iter().enumerate() {
            let (escaped, continued) = for_each_wrapped_range(
                line.spans
                    .iter()
                    .enumerate()
                    .map(|(i, span)| (span.content.as_ref(), i)),
                width,
                |cells, range| {
                    rows.push(cells, range, origin, Some(line));
                },
            );
            rows.escaped |= escaped;
            rows.continued |= continued;
        }
        rows
    }

    pub(super) fn line_range(
        lines: &[Line<'static>],
        width: usize,
        from: usize,
        to: usize,
    ) -> (usize, usize, usize, bool, bool) {
        let mut escaped = false;
        let mut continued = false;
        let mut total = 0usize;
        let mut first = None;
        let mut last = None;
        for (origin, line) in lines.iter().enumerate() {
            let flags = for_each_wrapped_range(
                line.spans
                    .iter()
                    .enumerate()
                    .map(|(i, span)| (span.content.as_ref(), i)),
                width,
                |_, _| {
                    if origin >= from && first.is_none() {
                        first = Some(total);
                    }
                    if origin <= to {
                        last = Some(total);
                    }
                    total = total.saturating_add(1);
                },
            );
            escaped |= flags.0;
            continued |= flags.1;
        }
        (
            first.unwrap_or(total),
            last.unwrap_or(0),
            total,
            escaped,
            continued,
        )
    }
    pub(super) fn display_notice(&self, locale: Locale, questions: bool) -> Option<String> {
        let mut notice = String::new();
        if self.escaped {
            notice.push_str(&tr(
                locale,
                if questions {
                    MessageId::UserInputUnicodeEscapes
                } else {
                    MessageId::PagerUnicodeEscapes
                },
            ));
        }
        if self.continued {
            if !notice.is_empty() {
                notice.push_str(" · ");
            }
            notice.push_str(&tr(locale, MessageId::PagerDisplayContinuation));
        }
        (!notice.is_empty()).then_some(notice)
    }
}

struct PageSearch {
    total: usize,
    index: usize,
    row: Option<usize>,
    origin: Option<usize>,
}

impl PagerPage {
    pub(crate) fn from_text(title: impl Into<String>, text: &str, width: u16) -> Self {
        // Pager bodies frequently carry tool output or worker transcripts.
        // Keep the same terminal-injection boundary as the single-page path.
        let mut sanitized = String::with_capacity(text.len());
        codewhale_tui_kit::osc8::strip_ansi_into(text, &mut sanitized);
        Self::new(title, PagerSource::Text(sanitized), usize::from(width))
    }

    fn from_lines(title: impl Into<String>, lines: Vec<Line<'static>>) -> Self {
        Self::new(title, PagerSource::Lines(lines), 0)
    }

    fn new(title: impl Into<String>, source: PagerSource, first_width: usize) -> Self {
        Self {
            title: title.into(),
            source,
            first_width,
            rows: RefCell::default(),
            export_markdown: None,
            copy_text: None,
            answer_text: None,
        }
    }

    fn layout(&self, width: usize, start: usize, needed: usize) -> bool {
        {
            let rows = self.rows.borrow();
            if rows.width == Some(width)
                && start >= rows.start
                && start.saturating_add(needed).min(rows.total)
                    <= rows.start.saturating_add(rows.plain.len())
            {
                return false;
            }
        }
        let scan = |start| match &self.source {
            PagerSource::Text(source) => PagerRows::wrap_text(source, width, start),
            PagerSource::Lines(lines) => PagerRows::wrap_lines_window(lines, width, start),
            PagerSource::Diff(diff) => {
                let rendered = codewhale_tui_kit::diff_render::render_diff_window(
                    diff,
                    u16::try_from(width).unwrap_or(u16::MAX),
                    start,
                    PAGER_RETAINED_ROWS,
                );
                let total = rendered.lines.len().saturating_add(rendered.omitted_rows);
                let plain = rendered.lines.iter().map(line_to_string).collect();
                PagerRows {
                    width: Some(width),
                    start,
                    total,
                    clipped_rows: rendered.clipped_rows,
                    clipped_bytes: rendered.clipped_bytes,
                    escaped: false,
                    continued: false,
                    plain,
                    styled: rendered.lines,
                    origin: rendered.origins,
                }
            }
        };
        let mut rows = scan(start);
        if start >= rows.total && rows.total > 0 {
            rows = scan(rows.total.saturating_sub(needed.max(1)));
        }
        *self.rows.borrow_mut() = rows;
        true
    }

    fn row_for_origin(&self, width: usize, origin: usize) -> usize {
        match &self.source {
            PagerSource::Text(source) => {
                let mut rows = 0usize;
                for raw in source.lines().take(origin) {
                    for_each_wrapped_range(std::iter::once((raw, 0)), width, |_, _| {
                        rows = rows.saturating_add(1);
                    });
                }
                rows
            }
            PagerSource::Lines(lines) => PagerRows::line_range(lines, width, origin, origin).0,
            PagerSource::Diff(diff) => codewhale_tui_kit::diff_render::diff_row_for_origin(
                diff,
                u16::try_from(width).unwrap_or(u16::MAX),
                origin,
            ),
        }
    }

    fn search(
        &self,
        width: usize,
        query: &str,
        selected: usize,
        anchor: Option<usize>,
    ) -> PageSearch {
        let query = query.trim().to_ascii_lowercase();
        let mut result = PageSearch {
            total: 0,
            index: 0,
            row: None,
            origin: None,
        };
        if query.is_empty() {
            return result;
        }
        if let PagerSource::Diff(diff) = &self.source {
            let found = if let Some(origin) = anchor {
                codewhale_tui_kit::diff_render::search_diff_from_origin(
                    diff,
                    u16::try_from(width).unwrap_or(u16::MAX),
                    &query,
                    origin,
                )
            } else {
                codewhale_tui_kit::diff_render::search_diff(
                    diff,
                    u16::try_from(width).unwrap_or(u16::MAX),
                    &query,
                    selected,
                )
            };
            result.total = found.total_matches;
            result.row = found.selected_row;
            result.origin = found.selected_origin;
            result.index = found.selected_index;
            return result;
        }
        let mut row_index = 0usize;
        let mut visit = |origin: usize, text: String| {
            if text.to_ascii_lowercase().contains(&query) {
                let choose = anchor.map_or(result.total == selected, |line| {
                    result.row.is_none() && origin >= line
                });
                if choose {
                    result.row = Some(row_index);
                    result.origin = Some(origin);
                    result.index = result.total;
                }
                result.total = result.total.saturating_add(1);
            }
            row_index = row_index.saturating_add(1);
        };
        match &self.source {
            PagerSource::Text(source) => {
                for (origin, raw) in source.lines().enumerate() {
                    for_each_wrapped_range(std::iter::once((raw, 0)), width, |cells, range| {
                        visit(origin, cells[range].iter().map(|&(ch, _)| ch).collect());
                    });
                }
            }
            PagerSource::Lines(lines) => {
                for (origin, line) in lines.iter().enumerate() {
                    for_each_wrapped_range(
                        line.spans
                            .iter()
                            .enumerate()
                            .map(|(i, span)| (span.content.as_ref(), i)),
                        width,
                        |cells, range| {
                            visit(origin, cells[range].iter().map(|&(ch, _)| ch).collect());
                        },
                    );
                }
            }
            PagerSource::Diff(_) => unreachable!(),
        }
        if result.row.is_none() && result.total > 0 {
            return self.search(width, &query, 0, None);
        }
        result
    }

    fn rows(&self) -> std::cell::Ref<'_, PagerRows> {
        if self.rows.borrow().width.is_none() {
            self.layout(self.first_width, 0, 1);
        }
        self.rows.borrow()
    }

    fn row_count(&self) -> usize {
        self.rows().total
    }

    /// What `c` / `y` copy without an explicit payload: the source, never the
    /// display wrapping.
    fn source_text(&self) -> String {
        match &self.source {
            PagerSource::Text(source) | PagerSource::Diff(source) => source.clone(),
            PagerSource::Lines(lines) => lines
                .iter()
                .map(line_to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    pub(crate) fn with_export_markdown(mut self, markdown: impl Into<String>) -> Self {
        self.export_markdown = Some(markdown.into());
        self
    }

    pub(crate) fn with_copy_text(mut self, text: impl Into<String>) -> Self {
        self.copy_text = Some(text.into());
        self
    }

    /// Attach the clean final assistant answer that the `a` key copies to
    /// the clipboard. Only surfaces that can produce an answer-only payload
    /// (Turn Inspector pages, assistant detail pagers) set this; the pager
    /// body itself stays free to render scaffolding around the answer.
    pub(crate) fn with_copy_answer(mut self, text: impl Into<String>) -> Self {
        self.answer_text = Some(text.into());
        self
    }
}

pub struct PagerView {
    locale: Locale,
    pages: Vec<PagerPage>,
    page_index: usize,
    /// First visible row. A cell because a resize re-wraps the rows during
    /// render, which remaps it to keep the same source line on top.
    scroll: Cell<usize>,
    search_input: String,
    search_index: Cell<usize>,
    search_total: Cell<usize>,
    search_row: Cell<Option<usize>>,
    search_origin: Cell<Option<usize>>,
    search_mode: bool,
    pending_g: bool,
    /// Cached visible content height from the last render. Used by paging
    /// keys (Ctrl+D/U, Ctrl+F/B, Space, etc.) to compute scroll deltas
    /// without access to the render area.
    last_visible_height: Cell<usize>,
    /// Optional inspector-owned destructive action. It requires two presses
    /// (or key then Enter); Esc disarms before it closes the pager.
    destructive_action: Option<PagerDestructiveAction>,
    action_area: Cell<Option<Rect>>,
    live_shell_job: Option<String>,
    follow: bool,
    evidence: Option<OwnedEvidence>,
}

impl PagerView {
    pub fn new(title: impl Into<String>, lines: Vec<Line<'static>>) -> Self {
        Self::from_pages(vec![PagerPage::from_lines(title, lines)], 0)
    }

    /// Build an opt-in multi-page pager and open the requested page. Page
    /// switching is deliberately unavailable to ordinary one-page pagers.
    pub(crate) fn from_pages(pages: Vec<PagerPage>, initial_page: usize) -> Self {
        assert!(!pages.is_empty(), "a pager needs at least one page");
        let page_index = initial_page.min(pages.len().saturating_sub(1));
        Self {
            pages,
            locale: Locale::En,
            page_index,
            scroll: Cell::new(0),
            search_input: String::new(),
            search_index: Cell::new(0),
            search_total: Cell::new(0),
            search_row: Cell::new(None),
            search_origin: Cell::new(None),
            search_mode: false,
            pending_g: false,
            last_visible_height: Cell::new(0),
            destructive_action: None,
            action_area: Cell::new(None),
            live_shell_job: None,
            follow: false,
            evidence: None,
        }
    }

    pub(crate) fn has_owned_evidence(&self) -> bool {
        self.evidence.is_some()
    }

    pub(crate) fn with_owned_evidence(
        mut self,
        workspace: std::path::PathBuf,
        session: String,
        reference: String,
    ) -> Self {
        let page = self.pages.len();
        let label = self.current_page().title.clone();
        self.pages.push(PagerPage::from_text("", "", 80));
        self.evidence = Some(OwnedEvidence {
            workspace,
            session,
            reference,
            label,
            page,
            body_start: 0,
            binary: false,
            digest: None,
            total: 0,
            offset: 0,
            end: 0,
            previous: Vec::new(),
            encoding: "UTF8",
            ready: false,
            pending: None,
        });
        self.evidence.as_mut().unwrap().request(0);
        self.localize_evidence_page();
        self.switch_page(page);
        self
    }

    fn evidence_request(&mut self, offset: u64) {
        if let Some(evidence) = self.evidence.as_mut() {
            evidence.request(offset);
            let page = &mut self.pages[evidence.page];
            page.copy_text = None;
            *page.rows.get_mut() = PagerRows::default();
            self.scroll.set(0);
            self.search_input.clear();
            self.search_index.set(0);
            self.search_total.set(0);
            self.search_row.set(None);
            self.search_origin.set(None);
            self.search_mode = false;
        }
        self.localize_evidence_page();
    }

    fn localize_evidence_page(&mut self) {
        let Some(evidence) = self.evidence.as_mut() else {
            return;
        };
        let page = &mut self.pages[evidence.page];
        if evidence.pending.is_some() {
            page.title = tr(self.locale, MessageId::PagerEvidenceLoadingTitle)
                .replace("{label}", &evidence.label);
            page.source =
                PagerSource::Text(tr(self.locale, MessageId::PagerEvidenceVerifying).into_owned());
        } else if evidence.ready {
            if let PagerSource::Text(source) = &mut page.source {
                let body = source.split_off(evidence.body_start);
                page.title = tr(self.locale, MessageId::PagerEvidenceRangeTitle)
                    .replace("{start}", &evidence.offset.to_string())
                    .replace("{end}", &evidence.end.to_string())
                    .replace("{total}", &evidence.total.to_string())
                    .replace("{encoding}", evidence.encoding)
                    .replace("{label}", &evidence.label);
                let mut header = tr(self.locale, MessageId::PagerEvidenceVerifiedRange)
                    .replace("{digest}", evidence.digest.as_deref().unwrap_or_default())
                    .replace("{start}", &evidence.offset.to_string())
                    .replace("{end}", &evidence.end.to_string())
                    .replace("{total}", &evidence.total.to_string())
                    .replace("{encoding}", evidence.encoding);
                header.push_str("\n\n");
                if evidence.binary {
                    header.push_str(&tr(self.locale, MessageId::PagerEvidenceBinary));
                    header.push_str("\n\n");
                }
                evidence.body_start = header.len();
                header.push_str(&body);
                *source = header;
            }
        } else {
            page.title = tr(self.locale, MessageId::PagerEvidenceUnavailableTitle)
                .replace("{label}", &evidence.label);
            page.source = PagerSource::Text(
                tr(self.locale, MessageId::PagerEvidenceUnavailable).into_owned(),
            );
        }
        *page.rows.get_mut() = PagerRows::default();
    }

    pub(crate) fn with_live_shell_job(mut self, job_id: impl Into<String>) -> Self {
        self.live_shell_job = Some(job_id.into());
        self
    }

    pub(crate) fn live_shell_job(&self) -> Option<&str> {
        self.live_shell_job.as_deref()
    }

    pub(crate) fn replace_live_text(&mut self, text: &str) -> bool {
        let page = self.current_page();
        if page.copy_text.as_deref() == Some(text) {
            return false;
        }
        if self.scroll.get() >= self.max_scroll() {
            self.follow = true;
        }
        let mut sanitized = String::with_capacity(text.len());
        codewhale_tui_kit::osc8::strip_ansi_into(text, &mut sanitized);
        let page = self.current_page_mut();
        page.source = PagerSource::Text(sanitized);
        page.copy_text = Some(text.to_string());
        *page.rows.get_mut() = PagerRows::default();
        if !self.search_input.trim().is_empty() {
            self.update_search_matches();
        }
        true
    }

    pub(crate) fn end_live_shell_job(&mut self) {
        self.live_shell_job = None;
        self.destructive_action = None;
        self.action_area.set(None);
    }

    /// Attach a compact Markdown export (e.g. the #4108 turn handoff) that the
    /// `e` key copies to the clipboard. Only the Turn Inspector pager sets this;
    /// other pagers leave `e` inert.
    #[cfg(test)]
    pub fn with_export_markdown(mut self, markdown: impl Into<String>) -> Self {
        self.current_page_mut().export_markdown = Some(markdown.into());
        self
    }

    /// Preserve a source-faithful payload for `c` / `y` while the rendered
    /// pager remains free to wrap content to its viewport.
    pub fn with_copy_text(mut self, text: impl Into<String>) -> Self {
        self.current_page_mut().copy_text = Some(text.into());
        self
    }

    /// Attach the clean final assistant answer that the `a` key copies
    /// (see [`PagerPage::with_copy_answer`]).
    pub fn with_copy_answer(mut self, text: impl Into<String>) -> Self {
        self.current_page_mut().answer_text = Some(text.into());
        self
    }

    /// Attach a two-step destructive action to this pager. Work Graph
    /// inspectors use this to keep Stop inside the detail surface while
    /// reusing the existing command/agent cancellation events.
    pub fn with_destructive_action(
        mut self,
        key: char,
        label: impl Into<String>,
        confirm_label: impl Into<String>,
        event: ViewEvent,
    ) -> Self {
        self.destructive_action = Some(PagerDestructiveAction {
            key,
            label: label.into(),
            confirm_label: confirm_label.into(),
            event,
            armed: false,
        });
        self
    }

    pub fn from_text(title: impl Into<String>, text: &str, width: u16) -> Self {
        Self::from_pages(vec![PagerPage::from_text(title, text, width)], 0)
    }

    pub(crate) fn from_diff(title: impl Into<String>, diff: String, width: u16) -> Self {
        let page = PagerPage::new(title, PagerSource::Diff(diff), usize::from(width));
        Self::from_pages(vec![page], 0)
    }

    /// Reuse the inspector confirmation control for token-bound commands.
    /// Copy exposes the exact command; confirmation dispatches it unchanged.
    pub(crate) fn command_review(
        title: impl Into<String>,
        text: &str,
        width: u16,
        command: String,
        locale: codewhale_localization::Locale,
    ) -> Self {
        let confirm = codewhale_localization::tr(
            locale,
            codewhale_localization::MessageId::PagerActionConfirm,
        )
        .into_owned();
        let mut view = Self::from_text(title, text, width)
            .with_copy_text(command.clone())
            .with_destructive_action(
                'y',
                confirm.clone(),
                confirm,
                ViewEvent::CommandPaletteSelected {
                    action: crate::tui::views::CommandPaletteAction::ExecuteCommand { command },
                },
            );
        view.locale = locale;
        view
    }

    fn activate_destructive_action(&mut self) -> ViewAction {
        self.pending_g = false;
        let Some(action) = self.destructive_action.as_mut() else {
            return ViewAction::None;
        };
        if action.armed {
            ViewAction::EmitAndClose(action.event.clone())
        } else {
            action.armed = true;
            ViewAction::None
        }
    }

    fn current_page(&self) -> &PagerPage {
        &self.pages[self.page_index]
    }

    fn current_page_mut(&mut self) -> &mut PagerPage {
        &mut self.pages[self.page_index]
    }

    fn switch_page(&mut self, page_index: usize) {
        let page_index = page_index.min(self.pages.len().saturating_sub(1));
        if page_index == self.page_index {
            return;
        }
        self.page_index = page_index;
        self.scroll.set(0);
        self.search_input.clear();
        self.search_index.set(0);
        self.search_total.set(0);
        self.search_row.set(None);
        self.search_origin.set(None);
        self.search_mode = false;
        self.pending_g = false;
        self.follow = false;
    }

    fn scroll_up(&mut self, amount: usize) {
        self.scroll.set(self.scroll.get().saturating_sub(amount));
        self.follow = false;
    }

    fn scroll_down(&mut self, amount: usize, max_scroll: usize) {
        self.scroll
            .set(self.scroll.get().saturating_add(amount).min(max_scroll));
        self.follow = self.live_shell_job.is_some() && self.scroll.get() >= max_scroll;
    }

    fn scroll_to_top(&mut self) {
        self.scroll.set(0);
        self.follow = false;
    }

    fn scroll_to_bottom(&mut self, max_scroll: usize) {
        self.scroll.set(max_scroll);
        self.follow = self.live_shell_job.is_some();
    }

    /// Plain-text rendered body of the pager joined with `\n`. This reflects
    /// width-based display wrapping, so it is not what the clipboard gets:
    /// an explicit copy payload wins, then the page's own source.
    #[cfg(test)]
    pub fn body_text(&self) -> String {
        self.current_page().rows().plain.join("\n")
    }

    fn clipboard_text(&self) -> String {
        let page = self.current_page();
        page.copy_text.clone().unwrap_or_else(|| page.source_text())
    }

    /// The pager's title bar text. Used by tests to assert the raw-detail
    /// pager is framed at leaf scope (#4105).
    #[cfg(test)]
    pub(crate) fn title(&self) -> &str {
        &self.current_page().title
    }

    /// Return the page height (in lines) used for paging keys.
    ///
    /// Falls back to a small constant (10) before the first render so the
    /// pager still responds to paging keys when invoked synthetically (e.g.
    /// in unit tests). After the first render, the cached value reflects
    /// the actual visible content area.
    fn page_height(&self) -> usize {
        let cached = self.last_visible_height.get();
        if cached == 0 { 10 } else { cached }
    }

    /// Half a page, rounded up so a single press always moves at least one line.
    fn half_page_height(&self) -> usize {
        let page = self.page_height();
        page.div_ceil(2).max(1)
    }

    fn max_scroll(&self) -> usize {
        // Match the render-side clamp so G/End land at the visible bottom and
        // k/Up immediately scroll back up by one line.
        self.current_page()
            .row_count()
            .saturating_sub(self.page_height())
    }

    fn start_search(&mut self) {
        self.search_mode = true;
        self.search_input.clear();
        self.search_index.set(0);
        self.search_total.set(0);
        self.search_row.set(None);
        self.search_origin.set(None);
    }

    fn install_search(&self, selected: usize, anchor: Option<usize>) {
        let page = self.current_page();
        let width = page.rows().width.unwrap_or(page.first_width);
        let found = page.search(width, &self.search_input, selected, anchor);
        self.search_total.set(found.total);
        self.search_index.set(found.index);
        self.search_row.set(found.row);
        self.search_origin.set(found.origin);
    }

    fn update_search_matches(&mut self) {
        self.install_search(0, None);
    }

    fn layout(&self, width: usize, needed: usize) {
        let page = self.current_page();
        let (old_width, top) = {
            let rows = page.rows.borrow();
            (
                rows.width,
                self.scroll
                    .get()
                    .checked_sub(rows.start)
                    .and_then(|i| rows.origin.get(i))
                    .copied(),
            )
        };
        if old_width != Some(width) {
            if let Some(top) = top {
                self.scroll.set(page.row_for_origin(width, top));
            }
            page.layout(width, self.scroll.get(), needed);
            if !self.search_input.trim().is_empty() {
                let found = page.search(
                    width,
                    &self.search_input,
                    self.search_index.get(),
                    self.search_origin.get(),
                );
                self.search_total.set(found.total);
                self.search_index.set(found.index);
                self.search_row.set(found.row);
                self.search_origin.set(found.origin);
            }
        } else {
            page.layout(width, self.scroll.get(), needed);
        }
    }

    fn jump_to_match(&mut self) {
        if let Some(row) = self.search_row.get() {
            self.scroll.set(row);
            self.follow = false;
        }
    }

    fn next_match(&mut self) {
        let total = self.search_total.get();
        if total == 0 {
            return;
        }
        self.install_search((self.search_index.get() + 1) % total, None);
        self.jump_to_match();
    }

    fn prev_match(&mut self) {
        let total = self.search_total.get();
        if total == 0 {
            return;
        }
        self.install_search(
            if self.search_index.get() == 0 {
                total - 1
            } else {
                self.search_index.get() - 1
            },
            None,
        );
        self.jump_to_match();
    }
}

impl ModalView for PagerView {
    fn set_locale(&mut self, locale: Locale) {
        if self.locale == locale {
            return;
        }
        self.locale = locale;
        self.localize_evidence_page();
        if !self.search_input.trim().is_empty() {
            self.update_search_matches();
        }
    }

    fn kind(&self) -> ModalKind {
        ModalKind::Pager
    }

    fn tick(&mut self) -> ViewAction {
        let completed = self.evidence.as_mut().and_then(|evidence| {
            evidence
                .pending
                .as_mut()
                .and_then(|(_, receive)| match receive.try_recv() {
                    Ok(result) => Some(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Err(())),
                })
        });
        let Some(result) = completed else {
            return ViewAction::None;
        };
        let evidence = self.evidence.as_mut().unwrap();
        evidence.pending.take();
        let page = &mut self.pages[evidence.page];
        match result {
            Ok(window)
                if evidence
                    .digest
                    .as_ref()
                    .is_none_or(|digest| digest == &window.digest) =>
            {
                page.source = PagerSource::Text(window.text);
                evidence.body_start = 0;
                evidence.binary = window.binary;
                page.copy_text = Some(window.copy);
                evidence.digest = Some(window.digest);
                evidence.total = window.total;
                evidence.offset = window.offset;
                evidence.end = window.end;
                evidence.encoding = window.encoding;
                evidence.ready = true;
            }
            _ => {
                page.copy_text = None;
                evidence.ready = false;
            }
        }
        self.localize_evidence_page();
        ViewAction::Redraw
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if self.search_mode {
            match key.code {
                KeyCode::Enter => {
                    self.search_mode = false;
                    self.update_search_matches();
                    self.jump_to_match();
                    return ViewAction::None;
                }
                KeyCode::Esc => {
                    // Bail out of search mode AND drop the current match list
                    // so the user gets back to the un-highlighted view —
                    // codex-style behavior. To resume from where they left
                    // off they re-enter `/` and re-type.
                    self.search_mode = false;
                    self.search_input.clear();
                    self.search_index.set(0);
                    self.search_total.set(0);
                    self.search_row.set(None);
                    self.search_origin.set(None);
                    return ViewAction::None;
                }
                KeyCode::Backspace => {
                    self.search_input.pop();
                    return ViewAction::None;
                }
                // Ctrl+H is the legacy ASCII backspace many terminals emit.
                KeyCode::Char('h')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.search_input.pop();
                    return ViewAction::None;
                }
                KeyCode::Char(c) => {
                    self.search_input.push(c);
                    return ViewAction::None;
                }
                // All other keys (Up/Down, PageUp/PageDown, etc.) are captured
                // in search mode so they don't fall through to the pager body.
                _ => return ViewAction::None,
            }
        }

        if self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.page == self.page_index)
        {
            let control = key.modifiers.contains(KeyModifiers::CONTROL);
            let target = match key.code {
                KeyCode::Home if control => Some(0),
                KeyCode::End if control => self
                    .evidence
                    .as_ref()
                    .filter(|evidence| evidence.ready)
                    .map(|evidence| evidence.total.saturating_sub(EVIDENCE_WINDOW_BYTES as u64)),
                KeyCode::Right | KeyCode::Char(']') => {
                    self.evidence.as_mut().and_then(|evidence| {
                        if !evidence.ready || evidence.end >= evidence.total {
                            return None;
                        }
                        if evidence.previous.len() < 8192 {
                            evidence.previous.push(evidence.offset);
                        }
                        Some(evidence.end)
                    })
                }
                KeyCode::Left | KeyCode::Char('[') => self.evidence.as_mut().and_then(|evidence| {
                    if !evidence.ready {
                        return None;
                    }
                    if evidence.offset == 0 {
                        return None;
                    }
                    Some(evidence.previous.pop().unwrap_or_else(|| {
                        evidence.offset.saturating_sub(EVIDENCE_WINDOW_BYTES as u64)
                    }))
                }),
                _ => None,
            };
            if let Some(offset) = target {
                self.evidence_request(offset);
                return ViewAction::Redraw;
            }
            if matches!(
                key.code,
                KeyCode::Right | KeyCode::Char(']') | KeyCode::Char('[')
            ) {
                return ViewAction::None;
            }
        }

        if let Some(action) = self.destructive_action.as_mut() {
            if key.code == KeyCode::Esc && action.armed {
                action.armed = false;
                self.pending_g = false;
                return ViewAction::None;
            }
            let matching_key =
                matches!(key.code, KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&action.key));
            if matching_key || (key.code == KeyCode::Enter && action.armed) {
                if key.kind != KeyEventKind::Press
                    || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
                {
                    return ViewAction::None;
                }
                return self.activate_destructive_action();
            }
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let max_scroll = self.max_scroll();

        // Ctrl+chord paging keys are matched first because their KeyCode
        // also matches the bare `KeyCode::Char(c)` arms below.
        if ctrl {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.scroll_down(self.half_page_height(), max_scroll);
                    self.pending_g = false;
                    return ViewAction::None;
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    self.scroll_up(self.half_page_height());
                    self.pending_g = false;
                    return ViewAction::None;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.scroll_down(self.page_height(), max_scroll);
                    self.pending_g = false;
                    return ViewAction::None;
                }
                KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.scroll_up(self.page_height());
                    self.pending_g = false;
                    return ViewAction::None;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Left if self.pages.len() > 1 => {
                self.switch_page(self.page_index.saturating_sub(1));
                ViewAction::None
            }
            KeyCode::Right if self.pages.len() > 1 => {
                self.switch_page(
                    self.page_index
                        .saturating_add(1)
                        .min(self.pages.len().saturating_sub(1)),
                );
                ViewAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_up(1);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_down(1, max_scroll);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::PageUp => {
                self.scroll_up(self.page_height());
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.scroll_down(self.page_height(), max_scroll);
                self.pending_g = false;
                ViewAction::None
            }
            // Vim convention: Space pages down, Shift+Space pages up. Match
            // Shift+Space first so it is not absorbed by the bare ' ' arm.
            KeyCode::Char(' ') if shift => {
                self.scroll_up(self.page_height());
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char(' ') => {
                self.scroll_down(self.page_height(), max_scroll);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Home => {
                self.scroll_to_top();
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::End => {
                self.scroll_to_bottom(max_scroll);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char('g') => {
                if self.pending_g {
                    self.scroll_to_top();
                    self.pending_g = false;
                } else {
                    self.pending_g = true;
                }
                ViewAction::None
            }
            KeyCode::Char('G') => {
                self.scroll_to_bottom(max_scroll);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char('/') => {
                self.start_search();
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char('n') => {
                self.next_match();
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char('N') => {
                self.prev_match();
                self.pending_g = false;
                ViewAction::None
            }
            // Copy the entire pager body to the clipboard. The pager
            // intercepts mouse capture so terminal-native selection is
            // disabled inside it; without this binding users with no
            // out-of-band copy path would have no way to extract content
            // they can see (#1354). Both `c` and `y` are wired so users
            // landing from either OS-clipboard or vim convention find a
            // working key.
            KeyCode::Char('c') | KeyCode::Char('y') => {
                if self
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.page == self.page_index && !evidence.ready)
                {
                    return ViewAction::None;
                }
                self.pending_g = false;
                ViewAction::Emit(ViewEvent::CopyToClipboard {
                    text: self.clipboard_text(),
                    label: self
                        .evidence
                        .as_ref()
                        .filter(|evidence| evidence.page == self.page_index)
                        .map_or_else(
                            || tr(self.locale, MessageId::PagerCopyContentLabel).into_owned(),
                            |evidence| {
                                tr(self.locale, MessageId::PagerEvidenceCopyLabel)
                                    .replace("{start}", &evidence.offset.to_string())
                                    .replace("{end}", &evidence.end.to_string())
                                    .replace("{encoding}", evidence.encoding)
                            },
                        ),
                })
            }
            // `e` exports the compact turn handoff (#4108) when this pager
            // carries one — the Turn Inspector. Elsewhere the guard fails and
            // `e` falls through to the inert arm below.
            KeyCode::Char('e') | KeyCode::Char('E')
                if self.current_page().export_markdown.is_some() =>
            {
                self.pending_g = false;
                let text = self
                    .current_page()
                    .export_markdown
                    .clone()
                    .unwrap_or_default();
                ViewAction::Emit(ViewEvent::CopyToClipboard {
                    text,
                    label: tr(self.locale, MessageId::PagerCopyHandoffLabel).into_owned(),
                })
            }
            // `a` copies ONLY the final assistant answer — the clean
            // answer-only payload attached by the Turn Inspector and the
            // assistant detail pagers. Unlike `c`/`y` (rendered body) or `e`
            // (whole-turn handoff markdown), this payload carries no
            // reasoning, tool calls/results, runtime status, or transcript
            // scaffolding. Elsewhere the guard fails and `a` is inert.
            KeyCode::Char('a') if self.current_page().answer_text.is_some() => {
                self.pending_g = false;
                let text = self.current_page().answer_text.clone().unwrap_or_default();
                ViewAction::Emit(ViewEvent::CopyToClipboard {
                    text,
                    label: tr(self.locale, MessageId::PagerCopyAnswerLabel).into_owned(),
                })
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && mouse.modifiers.is_empty()
            && self
                .action_area
                .get()
                .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
        {
            return self.activate_destructive_action();
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_up(3);
                self.pending_g = false;
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.scroll_down(3, self.max_scroll());
                self.pending_g = false;
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let page = self.current_page();
        let title = if self.pages.len() > 1 {
            format!(
                "{} · {}/{} · ←/→",
                page.title,
                self.page_index + 1,
                self.pages.len()
            )
        } else {
            page.title.clone()
        };
        let inner = render_underwater_surface(area, buf, title);

        // The wrapping action footer is anchored to the bottom of the inner
        // area; the body fills the rows above it.
        let mut hints = vec![
            ActionHint::new("q/Esc", tr(self.locale, MessageId::SessionsActionClose)),
            ActionHint::new("j/k", tr(self.locale, MessageId::PagerActionScroll)),
            ActionHint::new("Space", tr(self.locale, MessageId::PagerActionPage)),
            ActionHint::new("Ctrl+D/U", tr(self.locale, MessageId::PagerActionHalfPage)),
            ActionHint::new("g/G", tr(self.locale, MessageId::PagerActionTopBottom)),
            ActionHint::new("/", tr(self.locale, MessageId::SessionsActionSearch)),
            ActionHint::new("c", tr(self.locale, MessageId::PagerActionCopy)),
        ];
        if self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.page == self.page_index)
        {
            hints[5] = ActionHint::new(
                "/",
                tr(self.locale, MessageId::PagerActionSearchLoadedRange),
            );
            hints[6] = ActionHint::new("c", tr(self.locale, MessageId::PagerActionCopyLoadedRange));
            hints.push(ActionHint::new(
                "←/→",
                tr(self.locale, MessageId::PagerActionByteWindows),
            ));
            hints.push(ActionHint::new(
                "Ctrl+Home/End",
                tr(self.locale, MessageId::PagerActionFirstLastBytes),
            ));
        }
        if page.export_markdown.is_some() {
            hints.push(ActionHint::new(
                "e",
                tr(self.locale, MessageId::PagerActionCopyHandoff),
            ));
        }
        if page.answer_text.is_some() {
            hints.push(ActionHint::new(
                "a",
                tr(self.locale, MessageId::PagerActionCopyAnswer),
            ));
        }
        self.action_area.set(None);
        if let Some(action) = self.destructive_action.as_ref() {
            let key = if action.armed {
                format!("{}/Enter", action.key)
            } else {
                action.key.to_string()
            };
            let label = if action.armed {
                action.confirm_label.clone()
            } else {
                action.label.clone()
            };
            let action_width = key.width() + 2 + label.width();
            hints.push(ActionHint::new(key, label));
            let footer_lines = action_footer_lines(&hints, inner.width);
            if footer_lines.len() <= usize::from(inner.height)
                && inner.width > 0
                && let Some(last) = footer_lines.last()
            {
                let offset = last
                    .width()
                    .saturating_sub(action_width)
                    .min(usize::from(inner.width));
                self.action_area.set(Some(Rect::new(
                    inner.x.saturating_add(offset as u16),
                    inner.bottom().saturating_sub(1),
                    (action_width.min(usize::from(inner.width).saturating_sub(offset))) as u16,
                    1,
                )));
            }
        }
        let content = render_modal_footer(inner, buf, &hints);

        // Re-wrap before reserving the search-status row: resizing can make
        // the first match appear or the last match disappear. Hold a column
        // for the scroll rail so it cannot cause another paragraph wrap.
        let body_width = if content.width >= 2 {
            content.width - 1
        } else {
            content.width
        };
        self.layout(
            usize::from(body_width),
            usize::from(content.height).min(PAGER_RETAINED_ROWS),
        );

        // `content` already excludes the border, padding, and footer rows.
        let mut visible_height = usize::from(content.height).min(PAGER_RETAINED_ROWS);
        let diff_status = matches!(&page.source, PagerSource::Diff(_));
        let display_notice = page.rows.borrow().display_notice(self.locale, false);
        if diff_status || display_notice.is_some() {
            visible_height = visible_height.saturating_sub(1);
        }
        if self.search_mode {
            // Reserve a row for the search prompt that gets pushed below.
            visible_height = visible_height.saturating_sub(1);
        } else if self.search_total.get() > 0 {
            // Reserve a row for the "match X/Y (n/N)" status; without this
            // the status line gets clipped on small popup heights and the
            // user can't see how many matches there are.
            visible_height = visible_height.saturating_sub(1);
        }
        // Cache for paging keys; the value is treated as advisory and
        // clamped at use-time.
        self.last_visible_height.set(visible_height);
        let row_count = page.row_count();
        let max_scroll = row_count.saturating_sub(visible_height);
        let scroll = if self.follow {
            self.scroll.set(max_scroll);
            max_scroll
        } else {
            self.scroll.get().min(max_scroll)
        };
        self.scroll.set(scroll);
        page.layout(usize::from(body_width), scroll, visible_height);
        let rows = page.rows.borrow();
        let start = scroll.saturating_sub(rows.start).min(rows.plain.len());
        let end = start.saturating_add(visible_height).min(rows.plain.len());
        let mut visible_lines: Vec<Line<'static>> = if row_count == 0 {
            vec![Line::from("")]
        } else if rows.styled.is_empty() {
            rows.plain[start..end]
                .iter()
                .map(|row| Line::from(row.clone()))
                .collect()
        } else {
            rows.styled[start..end].to_vec()
        };

        // Highlight matched lines while the search prompt is closed and the
        // user is navigating with `n` / `N`. Other matches get a subtle
        // background; the current match gets a louder one. Per-substring
        // highlighting is deferred to a follow-up — preserving the pre-styled
        // spans (assistant / system colors) through a substring re-style is
        // a separate concern.
        if !self.search_mode && self.search_total.get() > 0 {
            let current_match_line = self.search_row.get();
            let query = self.search_input.trim().to_ascii_lowercase();
            for (visible_idx, line) in visible_lines.iter_mut().enumerate() {
                let absolute_idx = scroll + visible_idx;
                if absolute_idx >= row_count {
                    break;
                }
                if !rows.plain[start + visible_idx]
                    .to_ascii_lowercase()
                    .contains(&query)
                {
                    continue;
                }
                let is_current = current_match_line == Some(absolute_idx);
                let bg = if is_current {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                let fg = if is_current {
                    Color::Black
                } else {
                    Color::Yellow
                };
                let highlight = Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD);
                for span in line.spans.iter_mut() {
                    span.style = highlight;
                }
            }
        }

        if self.search_mode {
            let prompt = format!("/{}", self.search_input);
            visible_lines.push(Line::from(Span::styled(
                prompt,
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if self.search_total.get() > 0 {
            let status = tr(self.locale, MessageId::PagerSearchMatch)
                .replace("{current}", &(self.search_index.get() + 1).to_string())
                .replace("{total}", &self.search_total.get().to_string());
            visible_lines.push(Line::from(Span::styled(
                status,
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        if diff_status {
            let status = if rows.clipped_rows > 0 || rows.clipped_bytes > 0 {
                tr(self.locale, MessageId::PagerDiffRowsShortened)
                    .replace("{rows}", &rows.clipped_rows.to_string())
                    .replace("{bytes}", &rows.clipped_bytes.to_string())
                    .replace("{key}", "c")
            } else {
                tr(self.locale, MessageId::PagerDiffCopySource).replace("{key}", "c")
            };
            visible_lines.push(Line::from(Span::styled(
                codewhale_tui_kit::ui_text::truncate_line_to_width(
                    &status,
                    usize::from(body_width),
                ),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        if !diff_status && let Some(notice) = display_notice {
            visible_lines.push(Line::from(Span::styled(
                codewhale_tui_kit::ui_text::truncate_line_to_width(
                    &notice,
                    usize::from(body_width),
                ),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        let content =
            render_panel_scroll_rail(content, buf, row_count, scroll, visible_height, true);
        // Explicit base ink: the surface behind this body is always WHALE_BG,
        // so spans without their own fg must not inherit a dark terminal
        // default (light-profile terminals would render them as black-on-black).
        // Ratatui paints the base first; styled spans patch over it.
        let paragraph = Paragraph::new(visible_lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(palette::TEXT_PRIMARY));
        paragraph.render(content, buf);
    }
}

fn line_to_string(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.to_string())
        .collect::<String>()
}

fn for_each_wrapped_range<'a>(
    pieces: impl Iterator<Item = (&'a str, usize)>,
    width: usize,
    mut emit: impl FnMut(&[(char, usize)], Range<usize>),
) -> (bool, bool) {
    let mut escaped = false;
    let mut continued = false;
    let mut cells: Vec<(char, usize)> = Vec::new();
    let mut bytes = 0usize;
    let mut skip_space = false;
    codewhale_tui_kit::ui_text::display_graphemes_exact(pieces, 0, |grapheme| {
        escaped |= grapheme.escaped;
        if skip_space {
            if grapheme.text.chars().all(char::is_whitespace) {
                return;
            }
            skip_space = false;
        }
        if bytes.saturating_add(grapheme.text.len()) > PAGER_ROW_BYTES && !cells.is_empty() {
            let mut last = None;
            wrap_ranges_into(&cells, width, &mut |source, range| {
                if let Some(previous) = last.replace(range) {
                    emit(source, previous);
                }
            });
            let last = last.unwrap_or(0..0);
            skip_space = last.is_empty() && cells.last().is_some_and(|&(ch, _)| ch.is_whitespace());
            cells = cells[last].to_vec();
            bytes = cells.iter().map(|&(ch, _)| ch.len_utf8()).sum();
        }
        if skip_space {
            if grapheme.text.chars().all(char::is_whitespace) {
                return;
            }
            skip_space = false;
        }
        if bytes.saturating_add(grapheme.text.len()) > PAGER_ROW_BYTES && !cells.is_empty() {
            wrap_ranges_into(&cells, width, &mut emit);
            cells.clear();
            continued = true;
            let notice: Vec<_> = "[…]".chars().map(|ch| (ch, 0)).collect();
            wrap_ranges_into(&notice, width, &mut emit);
            bytes = 0;
        }
        bytes = bytes.saturating_add(grapheme.text.len());
        cells.extend_from_slice(grapheme.cells);
    });
    wrap_ranges_into(&cells, width, &mut emit);
    (escaped, continued)
}

#[cfg(test)]
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for_each_wrapped_range(std::iter::once((text, 0)), width, |cells, range| {
        rows.push(cells[range].iter().map(|&(ch, _)| ch).collect());
    });
    rows
}

fn wrap_ranges_into(
    cells: &[(char, usize)],
    width: usize,
    emit: &mut impl FnMut(&[(char, usize)], Range<usize>),
) {
    if width == 0 {
        emit(cells, 0..cells.len());
        return;
    }
    let text: String = cells.iter().map(|&(ch, _)| ch).collect();
    let mut character = 0;
    let graphemes: Vec<_> = text
        .graphemes(true)
        .map(|grapheme| {
            let start = character;
            character += grapheme.chars().count();
            (
                start,
                grapheme_display_width(grapheme),
                grapheme.chars().all(char::is_whitespace),
            )
        })
        .collect();
    let boundary = |index: usize| graphemes.get(index).map_or(cells.len(), |cell| cell.0);
    let mut current = 0..0;
    let mut current_width = 0usize;
    let mut has_content = false;
    let mut run_start = 0;
    while run_start < graphemes.len() {
        let is_space = graphemes[run_start].2;
        let run_end = graphemes[run_start..]
            .iter()
            .position(|&(_, _, space)| space != is_space)
            .map_or(graphemes.len(), |offset| run_start + offset);
        let run = run_start..run_end;
        run_start = run_end;
        let run_width: usize = run.clone().map(|i| graphemes[i].1).sum();
        if current_width + run_width <= width {
            current.end = run.end;
            current_width += run_width;
            has_content |= !is_space;
            continue;
        }
        if has_content {
            let mut kept = current.end;
            while kept > current.start && graphemes[kept - 1].2 {
                kept -= 1;
            }
            emit(cells, boundary(current.start)..boundary(kept));
            current_width = 0;
            has_content = false;
            current = run.end..run.end;
            if is_space {
                continue;
            }
            current = run.clone();
            if run_width <= width {
                current_width = run_width;
                has_content = true;
                continue;
            }
            current.end = run.start;
        }
        for index in run {
            let char_width = graphemes[index].1;
            if current_width + char_width > width && current_width > 0 {
                emit(cells, boundary(current.start)..boundary(current.end));
                current = index..index;
                current_width = 0;
            }
            current.end = index + 1;
            current_width += char_width;
        }
        has_content |= !is_space;
    }
    emit(cells, boundary(current.start)..boundary(current.end));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;

    fn make_pager(lines: usize) -> PagerView {
        let lines: Vec<Line<'static>> = (0..lines)
            .map(|i| Line::from(format!("line-{i:03}")))
            .collect();
        PagerView::new("T", lines)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn destructive_action_requires_two_steps_and_escape_only_disarms() {
        let mut pager = make_pager(2).with_destructive_action(
            's',
            "stop",
            "confirm stop · Esc cancels",
            ViewEvent::SidebarAgentCancel {
                agent_id: "agent_1".to_string(),
            },
        );

        assert!(matches!(
            pager.handle_key(key(KeyCode::Char('s'))),
            ViewAction::None
        ));
        assert!(matches!(
            pager.handle_key(key(KeyCode::Esc)),
            ViewAction::None
        ));
        assert!(matches!(
            pager.handle_key(key(KeyCode::Esc)),
            ViewAction::Close
        ));

        let _ = pager.handle_key(key(KeyCode::Char('s')));
        assert!(matches!(
            pager.handle_key(key(KeyCode::Enter)),
            ViewAction::EmitAndClose(ViewEvent::SidebarAgentCancel { agent_id })
                if agent_id == "agent_1"
        ));
    }

    #[test]
    fn command_review_confirms_the_pinned_command_with_keys_or_painted_mouse_control() {
        let command = format!(
            "/plugin trust fixture {}.{}",
            "a".repeat(64),
            "b".repeat(64)
        );
        for (width, height) in [(40, 12), (80, 24), (140, 40)] {
            let mut pager = PagerView::command_review(
                "Review fixture",
                "Exact reviewed capabilities",
                width - 2,
                command.clone(),
                codewhale_localization::Locale::En,
            );
            for modifiers in [
                KeyModifiers::CONTROL,
                KeyModifiers::ALT,
                KeyModifiers::SUPER,
            ] {
                assert!(matches!(
                    pager.handle_key(KeyEvent::new(KeyCode::Char('y'), modifiers)),
                    ViewAction::None
                ));
                assert!(!pager.destructive_action.as_ref().unwrap().armed);
            }
            assert!(matches!(
                pager.handle_key(KeyEvent::new_with_kind(
                    KeyCode::Char('y'),
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat
                )),
                ViewAction::None
            ));
            assert!(!pager.destructive_action.as_ref().unwrap().armed);
            let ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) =
                pager.handle_key(key(KeyCode::Char('c')))
            else {
                panic!("copy exposes the pinned command");
            };
            assert_eq!(text, command);
            let area = Rect::new(0, 0, width, height);
            let mut buffer = Buffer::empty(area);
            pager.render(area, &mut buffer);
            let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(rendered.contains("Confirm"), "{rendered}");
            assert!(
                !rendered.contains("disable"),
                "generic review must not describe an unrelated action"
            );
            let button = pager
                .action_area
                .get()
                .expect("visible confirmation control");
            assert!(button.width > 0 && button.bottom() <= height);
            let click = |button: Rect| MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.x,
                row: button.y,
                modifiers: KeyModifiers::NONE,
            };
            assert!(matches!(
                pager.handle_mouse(click(button)),
                ViewAction::None
            ));
            assert!(pager.destructive_action.as_ref().unwrap().armed);
            assert!(matches!(
                pager.handle_key(KeyEvent::new_with_kind(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat
                )),
                ViewAction::None
            ));
            pager.render(area, &mut buffer);
            let ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                action: crate::tui::views::CommandPaletteAction::ExecuteCommand { command: actual },
            }) = pager.handle_mouse(click(pager.action_area.get().unwrap()))
            else {
                panic!("second click confirms the reviewed command");
            };
            assert_eq!(actual, command);
        }
    }

    /// Drive a render once so `last_visible_height` is populated and paging
    /// keys use a deterministic page size.
    fn prime_layout(view: &mut PagerView, height: u16) {
        let area = Rect::new(0, 0, 40, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
    }

    #[test]
    fn j_scrolls_down_one_line() {
        let mut p = make_pager(50);
        let _ = p.handle_key(key(KeyCode::Char('j')));
        assert_eq!(p.scroll.get(), 1);
    }

    #[test]
    fn k_scrolls_up_one_line() {
        let mut p = make_pager(50);
        p.scroll.set(5);
        let _ = p.handle_key(key(KeyCode::Char('k')));
        assert_eq!(p.scroll.get(), 4);
    }

    #[test]
    fn gg_jumps_to_top() {
        let mut p = make_pager(50);
        p.scroll.set(30);
        let _ = p.handle_key(key(KeyCode::Char('g')));
        assert!(p.pending_g, "first 'g' should arm pending_g");
        assert_eq!(p.scroll.get(), 30, "first 'g' alone must not scroll");
        let _ = p.handle_key(key(KeyCode::Char('g')));
        assert_eq!(p.scroll.get(), 0);
        assert!(!p.pending_g);
    }

    #[test]
    fn home_jumps_to_top() {
        let mut p = make_pager(50);
        p.scroll.set(30);
        let _ = p.handle_key(key(KeyCode::Home));
        assert_eq!(p.scroll.get(), 0);
    }

    #[test]
    fn live_shell_job_pager_follows_new_output_until_scrolled_back() {
        let body = |rows: usize| {
            (0..rows)
                .map(|row| format!("out-{row:03}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut p = PagerView::from_text("Bash Job shell_1", &body(5), 40)
            .with_copy_text(body(5))
            .with_live_shell_job("shell_1");
        prime_layout(&mut p, 12);

        assert!(p.replace_live_text(&body(60)));
        prime_layout(&mut p, 12);
        assert!(p.max_scroll() > 0);
        assert_eq!(p.scroll.get(), p.max_scroll());

        let _ = p.handle_key(key(KeyCode::Char('k')));
        let held = p.scroll.get();
        assert!(p.replace_live_text(&body(80)));
        prime_layout(&mut p, 12);
        assert_eq!(p.scroll.get(), held);

        let _ = p.handle_key(key(KeyCode::Char('G')));
        assert!(p.replace_live_text(&body(90)));
        prime_layout(&mut p, 12);
        assert_eq!(p.scroll.get(), p.max_scroll());
        assert!(!p.replace_live_text(&body(90)));
    }

    #[test]
    fn ending_a_live_shell_job_drops_stop() {
        let mut p = make_pager(2)
            .with_live_shell_job("shell_1")
            .with_destructive_action(
                's',
                "stop",
                "confirm stop · Esc cancels",
                ViewEvent::SidebarAgentCancel {
                    agent_id: "agent_1".to_string(),
                },
            );
        p.end_live_shell_job();
        assert!(p.live_shell_job().is_none());
        for _ in 0..2 {
            assert!(matches!(
                p.handle_key(key(KeyCode::Char('s'))),
                ViewAction::None
            ));
        }
    }

    #[test]
    fn shift_g_jumps_to_bottom() {
        let mut p = make_pager(50);
        let _ = p.handle_key(key(KeyCode::Char('G')));
        assert_eq!(p.scroll.get(), p.max_scroll());
    }

    #[test]
    fn end_jumps_to_bottom() {
        let mut p = make_pager(50);
        let _ = p.handle_key(key(KeyCode::End));
        assert_eq!(p.scroll.get(), p.max_scroll());
    }

    #[test]
    fn up_immediately_scrolls_after_shift_g_to_bottom() {
        let mut p = make_pager(50);
        prime_layout(&mut p, 22);
        let bottom = p.max_scroll();

        let _ = p.handle_key(key(KeyCode::Char('G')));
        assert_eq!(p.scroll.get(), bottom);
        let _ = p.handle_key(key(KeyCode::Up));
        assert_eq!(p.scroll.get(), bottom - 1);
        let _ = p.handle_key(key(KeyCode::Char('k')));
        assert_eq!(p.scroll.get(), bottom - 2);
    }

    #[test]
    fn k_immediately_scrolls_after_end_to_bottom() {
        let mut p = make_pager(50);
        prime_layout(&mut p, 22);
        let bottom = p.max_scroll();

        let _ = p.handle_key(key(KeyCode::End));
        assert_eq!(p.scroll.get(), bottom);
        let _ = p.handle_key(key(KeyCode::Char('k')));
        assert_eq!(p.scroll.get(), bottom - 1);
    }

    #[test]
    fn ctrl_d_half_page_down() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        let half = p.half_page_height();
        assert!(half >= 1, "half-page must move at least one line");
        let _ = p.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(p.scroll.get(), half);
    }

    #[test]
    fn ctrl_u_half_page_up() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        p.scroll.set(50);
        let half = p.half_page_height();
        let _ = p.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(p.scroll.get(), 50 - half);
    }

    #[test]
    fn ctrl_f_full_page_down() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        let page = p.page_height();
        let _ = p.handle_key(ctrl(KeyCode::Char('f')));
        assert_eq!(p.scroll.get(), page);
    }

    #[test]
    fn ctrl_b_full_page_up() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        p.scroll.set(80);
        let page = p.page_height();
        let _ = p.handle_key(ctrl(KeyCode::Char('b')));
        assert_eq!(p.scroll.get(), 80 - page);
    }

    #[test]
    fn space_pages_down() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        let page = p.page_height();
        let _ = p.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(p.scroll.get(), page);
    }

    #[test]
    fn shift_space_pages_up() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        p.scroll.set(80);
        let page = p.page_height();
        let _ = p.handle_key(key_mod(KeyCode::Char(' '), KeyModifiers::SHIFT));
        assert_eq!(p.scroll.get(), 80 - page);
    }

    #[test]
    fn page_down_uses_cached_visible_height() {
        let mut p = make_pager(200);
        prime_layout(&mut p, 22);
        let page = p.page_height();
        let _ = p.handle_key(key(KeyCode::PageDown));
        assert_eq!(p.scroll.get(), page);
    }

    #[test]
    fn q_closes_pager() {
        let mut p = make_pager(10);
        let action = p.handle_key(key(KeyCode::Char('q')));
        assert!(matches!(action, ViewAction::Close));
    }

    #[test]
    fn esc_closes_pager() {
        let mut p = make_pager(10);
        let action = p.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, ViewAction::Close));
    }

    #[test]
    fn multi_page_switch_resets_view_state_and_keeps_copy_export_page_scoped() {
        let first =
            PagerPage::from_text("Turns", "first displayed", 80).with_copy_text("FIRST-SOURCE");
        let latest = PagerPage::from_text("Turns", "latest displayed", 80)
            .with_copy_text("LATEST-SOURCE")
            .with_export_markdown("LATEST-HANDOFF");
        let mut pager = PagerView::from_pages(vec![first, latest], 1);
        pager.scroll.set(4);
        pager.search_input = "latest".to_string();
        pager.search_total.set(1);
        pager.search_row.set(Some(0));
        pager.search_origin.set(Some(0));

        assert!(matches!(
            pager.handle_key(key(KeyCode::Left)),
            ViewAction::None
        ));
        assert_eq!(pager.body_text(), "first displayed");
        assert_eq!(pager.scroll.get(), 0);
        assert!(pager.search_input.is_empty());
        assert_eq!(pager.search_total.get(), 0);
        assert!(matches!(
            pager.handle_key(key(KeyCode::Char('c'))),
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) if text == "FIRST-SOURCE"
        ));
        assert!(matches!(
            pager.handle_key(key(KeyCode::Char('e'))),
            ViewAction::None
        ));

        let _ = pager.handle_key(key(KeyCode::Right));
        assert!(matches!(
            pager.handle_key(key(KeyCode::Char('e'))),
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) if text == "LATEST-HANDOFF"
        ));
    }

    #[test]
    fn g_does_not_consume_search_input() {
        // While in search mode, 'g' must be treated as a search character,
        // not as the half of a `gg` jump-to-top sequence.
        let mut p = make_pager(50);
        p.scroll.set(10);
        let _ = p.handle_key(key(KeyCode::Char('/')));
        assert!(p.search_mode);
        let _ = p.handle_key(key(KeyCode::Char('g')));
        assert_eq!(p.search_input, "g");
        assert_eq!(p.scroll.get(), 10);
    }

    #[test]
    fn footer_hint_includes_new_bindings() {
        // The rendered pager must surface the new vim-style bindings to the
        // user. The footer is now a wrapping ActionHint row inside the modal
        // body (not the bottom border), so assert against the rendered buffer.
        let p = make_pager(5);
        let area = Rect::new(0, 0, 100, 16);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        for needle in &[
            "j/k",
            "scroll",
            "g/G",
            "top/bottom",
            "Space",
            "page",
            "Ctrl+D/U",
            "half",
            "search",
            "copy",
            "q/Esc",
            "close",
        ] {
            assert!(text.contains(needle), "footer hint missing {needle:?}");
        }
    }

    #[test]
    fn body_cells_carry_explicit_ink_on_the_dark_surface() {
        // The pager paints WHALE_BG behind the body, so a body span
        // without its own fg inherits the terminal default, black ink on
        // light-profile terminals, i.e. black-on-black. The base paragraph
        // style must pin every text cell to the body ink.
        let p = make_pager(3);
        let area = Rect::new(0, 0, 100, 16);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut checked = 0;
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if !row.contains("line-") {
                continue;
            }
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.symbol().trim().is_empty() {
                    continue;
                }
                assert_eq!(
                    cell.style().fg,
                    Some(palette::TEXT_PRIMARY),
                    "body cell ({x}, {y}) must carry explicit body ink",
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "expected body rows in the rendered pager");
    }

    #[test]
    fn c_emits_copy_event_with_full_body() {
        // #1354: the pager intercepts mouse capture, so users have no way to
        // copy content out without an in-app key. Both `c` and `y` should
        // emit a CopyToClipboard event carrying the whole body so the host
        // dispatcher (in ui.rs) can write through `app.clipboard` and toast
        // a confirmation.
        let mut p = make_pager(3);
        let action = p.handle_key(key(KeyCode::Char('c')));
        match action {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, label }) => {
                assert_eq!(text, "line-000\nline-001\nline-002");
                assert_eq!(label, "Pager content");
            }
            other => panic!("expected CopyToClipboard emit, got {other:?}"),
        }
    }

    #[test]
    fn a_emits_copy_event_with_attached_answer_only() {
        // `a` copies the attached answer-only payload — never the rendered
        // body, which may carry scaffolding around the answer.
        let mut pager = PagerView::from_text("Turn Inspector", "[◆ · done] body", 40)
            .with_copy_answer("CLEAN-ANSWER");
        let action = pager.handle_key(key(KeyCode::Char('a')));
        match action {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, label }) => {
                assert_eq!(text, "CLEAN-ANSWER");
                assert_eq!(label, "Answer");
            }
            other => panic!("expected CopyToClipboard emit, got {other:?}"),
        }

        // Without an attached answer `a` stays inert; it must never fall
        // back to copying the rendered body.
        let mut plain = PagerView::from_text("T", "body", 40);
        assert!(matches!(
            plain.handle_key(key(KeyCode::Char('a'))),
            ViewAction::None
        ));
    }

    #[test]
    fn copy_override_preserves_indentation_tabs_and_blank_lines() {
        let source = "Result:\n    indented\n\twith-tab\n\nnext";
        let mut pager = PagerView::from_text("T", source, 12).with_copy_text(source);

        let action = pager.handle_key(key(KeyCode::Char('c')));
        match action {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) => {
                assert_eq!(text, source);
            }
            other => panic!("expected CopyToClipboard emit, got {other:?}"),
        }
    }

    #[test]
    fn from_text_keeps_one_display_row_per_blank_source_line() {
        let pager = PagerView::from_text("T", "first\n\nthird", 80);
        assert_eq!(pager.body_text(), "first\n\nthird");
    }

    #[test]
    fn from_text_strips_csi_mouse_and_osc_sequences() {
        // A worker transcript can carry captured terminal bytes (a child TUI's
        // mouse-tracking handshake, SGR color, OSC hyperlinks). Rendering them
        // raw emits the escapes to the user's terminal, which re-enables mouse
        // reporting after exit and leaves the shell executing fragments.
        let hostile = "── assistant ──\n\
                       enabling \u{1b}[?1003h\u{1b}[?1006h mouse tracking\n\
                       click bytes \u{1b}[<65;72;17M\u{1b}[<35;131;42M arrived\n\
                       \u{1b}[31mred text\u{1b}[0m and an \u{1b}]8;;https://example.com\u{7}OSC link\u{1b}]8;;\u{1b}\\\n\
                       trailing stray \u{1b}\u{7}bytes\u{1b}c done";
        let pager = PagerView::from_text("Agent transcript", hostile, 200);
        let body = pager.body_text();
        assert!(
            !body.contains('\u{1b}'),
            "no ESC byte may survive into the pager body: {body:?}"
        );
        assert!(
            !body.contains('\u{7}'),
            "no BEL byte may survive into the pager body: {body:?}"
        );
        for fragment in ["?1003h", "?1006h", "<65;72;17M", "<35;131;42M", "]8;;"] {
            assert!(
                !body.contains(fragment),
                "escape fragment {fragment:?} must be stripped, not painted: {body:?}"
            );
        }
        assert!(body.contains("red text"), "visible text survives: {body:?}");
        assert!(body.contains("OSC link"), "link label survives: {body:?}");
        assert!(body.contains("done"), "trailing text survives: {body:?}");
    }

    #[test]
    fn from_text_sanitizes_jsonl_transcript_shaped_content() {
        // Regression for the sub-agent transcript pager corrupting the parent
        // terminal: content shaped like the raw artifact lines, with embedded
        // mouse/CSI sequences inside a tool result, must render inert.
        let transcript_like = concat!(
            "── assistant ──\n",
            "I'll run the build now.\n",
            "← tool result (call-1)\n",
            // JSON-escaped text is literal backslash-u bytes — inert, kept as-is.
            "{\"line\":\"\\u{1b}[<0;9;4Mprogress done\"}\n",
            // Raw event bytes are the dangerous form and must be stripped.
            "raw \u{1b}[<0;10;5M event bytes\u{1b}[2K after\n",
        );
        let pager = PagerView::from_text("Agent transcript", transcript_like, 200);
        let body = pager.body_text();
        assert!(!body.contains('\u{1b}'), "inert body required: {body:?}");
        assert!(
            !body.contains("<0;10;5M"),
            "mouse event fragment must not render: {body:?}"
        );
        assert!(
            body.contains("progress") && body.contains("after"),
            "readable content survives: {body:?}"
        );
    }

    #[test]
    fn y_emits_copy_event_for_vim_users() {
        let mut p = make_pager(3);
        let action = p.handle_key(key(KeyCode::Char('y')));
        assert!(
            matches!(action, ViewAction::Emit(ViewEvent::CopyToClipboard { .. })),
            "y must emit a copy event for vim-yank parity"
        );
    }

    #[test]
    fn e_exports_turn_handoff_when_attached() {
        // #4108: the Turn Inspector pager carries a compact Markdown handoff;
        // `e` copies that artifact (not the visible inspector body) to the
        // clipboard via the host dispatcher.
        let mut p = make_pager(3).with_export_markdown("# Turn handoff\n\n## Intent\ndo the thing");
        let action = p.handle_key(key(KeyCode::Char('e')));
        match action {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, label }) => {
                assert!(text.contains("# Turn handoff"), "handoff text: {text}");
                assert_eq!(label, "Turn handoff");
            }
            other => panic!("expected CopyToClipboard emit, got {other:?}"),
        }
    }

    #[test]
    fn e_is_inert_without_an_attached_handoff() {
        // Every other pager leaves `e` unbound so it never surprises the user.
        let mut p = make_pager(3);
        assert!(matches!(
            p.handle_key(key(KeyCode::Char('e'))),
            ViewAction::None
        ));
    }

    #[test]
    fn copy_keys_inert_in_search_mode() {
        // Within `/`-search mode `c` and `y` must be treated as search
        // characters, not as a copy trigger — otherwise users typing a
        // query that contains either letter would lose their input.
        let mut p = make_pager(10);
        let _ = p.handle_key(key(KeyCode::Char('/')));
        assert!(p.search_mode);
        let action = p.handle_key(key(KeyCode::Char('c')));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(p.search_input, "c");
    }

    #[test]
    fn footer_hint_is_rendered_in_buffer() {
        let p = make_pager(5);
        let area = Rect::new(0, 0, 100, 10);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        // The footer is now anchored to the bottom of the modal body (above the
        // padding/border) rather than painted on the border, so scan the whole
        // frame for the action labels.
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("close") || text.contains("scroll"),
            "expected footer hint in rendered pager, got:\n{text}"
        );
    }

    /// `/` opens the search prompt; typing chars accumulates them; Enter
    /// commits and jumps to the first match. The matches index/count line
    /// must surface in the rendered buffer afterwards.
    #[test]
    fn search_finds_matches_and_renders_match_counter() {
        let mut p = make_pager(20);
        prime_layout(&mut p, 16);

        // Open search.
        let _ = p.handle_key(key(KeyCode::Char('/')));
        // Type "5" to match line-005, line-015 (any line whose number contains
        // a 5 — make_pager produced "line-NNN" with three-digit indices).
        for ch in "5".chars() {
            let _ = p.handle_key(key(KeyCode::Char(ch)));
        }
        // Commit.
        let _ = p.handle_key(key(KeyCode::Enter));

        // Render and look for the "match X/Y" status line.
        let area = Rect::new(0, 0, 60, 16);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut full = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                full.push_str(buf[(x, y)].symbol());
            }
            full.push('\n');
        }
        assert!(
            full.contains("match 1/2") || full.contains("match 1/3"),
            "expected match counter; got buffer:\n{full}"
        );
    }

    /// Esc while in search mode bails out AND clears the highlighted matches
    /// so the un-highlighted view returns. (Codex parity.)
    #[test]
    fn esc_in_search_mode_clears_matches() {
        let mut p = make_pager(20);
        prime_layout(&mut p, 16);

        let _ = p.handle_key(key(KeyCode::Char('/')));
        let _ = p.handle_key(key(KeyCode::Char('5')));
        let _ = p.handle_key(key(KeyCode::Enter));
        assert!(p.search_total.get() > 0);

        // Re-enter search mode and Esc out — matches must clear.
        let _ = p.handle_key(key(KeyCode::Char('/')));
        let _ = p.handle_key(key(KeyCode::Esc));
        assert_eq!(p.search_total.get(), 0);
        assert_eq!(p.search_input, "");
        assert!(!p.search_mode);
    }

    /// `n` and `N` cycle forward and backward through matches, wrapping at
    /// the ends without panicking on out-of-bounds index.
    #[test]
    fn n_and_capital_n_cycle_matches_with_wrap() {
        let mut p = make_pager(50);
        prime_layout(&mut p, 16);

        // Search "1" — matches every line whose printed index contains a 1.
        let _ = p.handle_key(key(KeyCode::Char('/')));
        let _ = p.handle_key(key(KeyCode::Char('1')));
        let _ = p.handle_key(key(KeyCode::Enter));
        let total = p.search_total.get();
        assert!(total > 1, "test needs multiple matches, got {total}");

        let start = p.search_index.get();
        let _ = p.handle_key(key(KeyCode::Char('n')));
        assert_eq!(p.search_index.get(), (start + 1) % total);
        let _ = p.handle_key(key(KeyCode::Char('N')));
        assert_eq!(p.search_index.get(), start);

        // Wrap backwards from 0 → last.
        let _ = p.handle_key(key(KeyCode::Char('N')));
        assert_eq!(p.search_index.get(), total - 1);
        let _ = p.handle_key(key(KeyCode::Char('n')));
        assert_eq!(p.search_index.get(), 0);
    }

    /// While search matches exist and the prompt is closed, the matched
    /// lines are visually distinguished in the rendered buffer by their
    /// background color. We sample directly across the matched-line text
    /// columns rather than the whole row width because Paragraph leaves
    /// the trailing-area cells at the default background.
    #[test]
    fn matched_lines_get_highlight_background() {
        let mut p = make_pager(20);
        prime_layout(&mut p, 16);

        let _ = p.handle_key(key(KeyCode::Char('/')));
        let _ = p.handle_key(key(KeyCode::Char('5')));
        let _ = p.handle_key(key(KeyCode::Enter));
        assert!(p.search_total.get() > 0);

        let area = Rect::new(0, 0, 40, 16);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);

        // Find the actual painted match: shared compact layout may move it.
        let row = (0..area.height)
            .find(|&y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains("line-005")
            })
            .expect("matched text must be visible");
        let highlighted = (0..area.width)
            .filter(|&x| buf[(x, row)].style().bg == Some(Color::Yellow))
            .collect::<Vec<_>>();
        assert_eq!(highlighted.len(), "line-005".len());
        for x in highlighted {
            assert_eq!(buf[(x, row)].style().fg, Some(Color::Black));
        }
    }

    #[test]
    fn mouse_scroll_up_scrolls_content() {
        let mut p = make_pager(50);
        p.scroll.set(10);
        let action = p.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(p.scroll.get(), 7);
        assert!(matches!(action, ViewAction::None));
    }

    #[test]
    fn mouse_scroll_down_scrolls_content() {
        let mut p = make_pager(50);
        prime_layout(&mut p, 20);
        p.scroll.set(10);
        let action = p.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(p.scroll.get(), 13);
        assert!(matches!(action, ViewAction::None));
    }

    #[test]
    fn mouse_scroll_down_clamps_to_pager_bottom() {
        let mut p = make_pager(50);
        prime_layout(&mut p, 20);
        let bottom = p.max_scroll();

        for _ in 0..100 {
            let _ = p.handle_mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
        }

        assert_eq!(p.scroll.get(), bottom);
    }

    #[test]
    fn pager_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;

        const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];
        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(make_pager(60));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
                .collect();
            let text = rows.join("\n");

            // Footer keeps every action.
            for label in [
                "close",
                "scroll",
                "page",
                "half",
                "top/bottom",
                "search",
                "copy",
            ] {
                assert!(text.contains(label), "{w}x{h}: footer missing '{label}'");
            }

            // Composited frame is fully opaque.
            assert!(!text.contains('X'), "{w}x{h}: background bleed-through");
            assert_eq!(
                buf[(w / 2, h / 2)].bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );

            // No horizontal overflow.
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_text_keeps_indentation_columns_and_repeated_spaces() {
        assert_eq!(wrap_text("    return x", 40), ["    return x"]);
        assert_eq!(wrap_text("\tif ok:", 40), ["    if ok:"]);
        assert_eq!(wrap_text("name    size", 40), ["name    size"]);
        assert_eq!(wrap_text("10%\r20%\u{b}x\u{2028}y", 40), ["10% 20% x y"]);
        // Only the whitespace at a soft break is consumed.
        assert_eq!(wrap_text("  alpha  beta", 9), ["  alpha", "beta"]);
        // An indent wider than the row still keeps every column it can.
        assert_eq!(wrap_text("      word", 4), ["    ", "  wo", "rd"]);
    }

    #[test]
    fn text_pager_displays_and_copies_code_with_its_indentation() {
        let source = "def f(x):\n    if x:\n\treturn x\n\n| a  | b |";
        let mut pager = PagerView::from_text("Selection", source, 80);

        assert_eq!(
            pager.body_text(),
            "def f(x):\n    if x:\n    return x\n\n| a  | b |"
        );
        match pager.handle_key(key(KeyCode::Char('y'))) {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) => {
                assert_eq!(text, source, "copy must carry the source, not the display");
            }
            other => panic!("expected CopyToClipboard emit, got {other:?}"),
        }
    }

    #[test]
    fn text_pager_rewraps_to_its_body_so_end_reaches_the_last_line() {
        // Callers wrap to the transcript width minus two; the pager body is
        // narrower (margins, padding, scroll rail). Rows that fit the caller's
        // width but not the body used to wrap again in the paragraph, which
        // the scroll arithmetic never counted.
        let mut text: Vec<String> = (0..39)
            .map(|i| format!("row {i:02} {}", "word ".repeat(14).trim_end()))
            .collect();
        text.push(format!(
            "row 39 {} END-MARKER",
            "word ".repeat(12).trim_end()
        ));
        let text = text.join("\n");
        let (width, height) = (80u16, 24u16);
        let mut pager = PagerView::from_text("Message", &text, width - 2);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        pager.render(area, &mut buf);

        let _ = pager.handle_key(key(KeyCode::End));
        let mut buf = Buffer::empty(area);
        pager.render(area, &mut buf);
        let screen: String = (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("END-MARKER"),
            "End must reveal the last line:\n{screen}"
        );
    }

    fn render_screen(pager: &PagerView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        pager.render(area, &mut buf);
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn styled_pager_wraps_to_its_body_so_end_reaches_the_last_line() {
        // A styled row wider than the body used to be wrapped by the
        // paragraph but counted as one row, so End stopped short of the tail.
        let mut lines: Vec<Line<'static>> = (0..39)
            .map(|i| {
                Line::from(vec![
                    Span::styled(format!("row {i:02} "), Style::default().fg(Color::Cyan)),
                    Span::raw("word ".repeat(20)),
                ])
            })
            .collect();
        lines.push(Line::from(format!(
            "row 39 {}END-MARKER",
            "word ".repeat(20)
        )));
        let mut pager = PagerView::new("Models", lines);
        let (width, height) = (80u16, 24u16);
        render_screen(&pager, width, height);

        let _ = pager.handle_key(key(KeyCode::End));
        let screen = render_screen(&pager, width, height);
        assert!(
            screen.contains("END-MARKER"),
            "End must reveal the last line:\n{screen}"
        );
        // Wrapping keeps each piece's style and copies the unwrapped source.
        let rows = pager.current_page().rows();
        assert!(
            rows.styled
                .iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.style.fg == Some(Color::Cyan))
        );
        assert!(rows.total > 40, "rows must be wrapped: {}", rows.total);
        drop(rows);
        match pager.handle_key(key(KeyCode::Char('c'))) {
            ViewAction::Emit(ViewEvent::CopyToClipboard { text, .. }) => {
                assert_eq!(text.lines().count(), 40);
            }
            other => panic!("expected CopyToClipboard emit, got {other:?}"),
        }
    }

    #[test]
    fn wrap_text_advances_tabs_to_stops_and_drops_bidi_format_chars() {
        assert_eq!(wrap_text("a\tb", 40), ["a   b"]);
        assert_eq!(wrap_text("abcd\tb", 40), ["abcd    b"]);
        assert_eq!(wrap_text("会\tb", 40), ["会  b"]);
        assert_eq!(wrap_text("left\u{202E}right\u{2066}", 40), ["leftright"]);
        assert_eq!(wrap_text("👩‍💻\tx", 40), ["👩‍💻  x"]);
    }

    #[test]
    fn pager_wraps_whole_graphemes_and_preserves_their_styles() {
        for grapheme in ["👩‍💻", "👍🏽", "🇨🇳", "1\u{fe0f}\u{20e3}"] {
            assert_eq!(wrap_text(grapheme, 2), [grapheme]);
            let source = grapheme.repeat(2);
            assert_eq!(wrap_text(&source, 2), [grapheme, grapheme]);
            let style = Style::default().fg(Color::Cyan);
            let rows = PagerRows::wrap_lines(&[Line::from(Span::styled(source, style))], 2);
            assert_eq!(rows.plain, [grapheme, grapheme]);
            assert!(
                rows.styled
                    .iter()
                    .all(|row| row.spans.len() == 1 && row.spans[0].style == style)
            );
        }
        assert_eq!(wrap_text("a\u{301}b", 1), ["a\u{301}", "b"]);
    }

    #[test]
    fn resize_recomputes_zero_matches_before_reserving_the_status_row() {
        let query = "needle".repeat(10);
        let text = format!("{query}\n{}", "hay\n".repeat(40));
        let mut pager = PagerView::from_text("Output", &text, 38);
        let no_search = PagerView::from_text("Output", &text, 38);
        render_screen(&pager, 40, 24);
        let _ = pager.handle_key(key(KeyCode::Char('/')));
        for ch in query.chars() {
            let _ = pager.handle_key(key(KeyCode::Char(ch)));
        }
        let _ = pager.handle_key(key(KeyCode::Enter));
        assert_eq!(pager.search_total.get(), 0);

        let wide = render_screen(&pager, 100, 24);
        render_screen(&no_search, 100, 24);
        assert!(wide.contains("match 1/1"), "{wide}");
        assert_eq!(pager.search_total.get(), 1);
        assert_eq!(pager.page_height() + 1, no_search.page_height());

        let narrow = render_screen(&pager, 40, 24);
        render_screen(&no_search, 40, 24);
        assert!(!narrow.contains("match 1/1"), "{narrow}");
        assert_eq!(pager.search_total.get(), 0);
        assert_eq!(pager.page_height(), no_search.page_height());
    }

    #[test]
    fn resize_keeps_the_top_line_and_the_current_match() {
        let text = (0..60)
            .map(|i| {
                let tag = if i == 40 { "needle" } else { "hay" };
                format!("row {i:02} {tag} {}", "word ".repeat(24).trim_end())
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut pager = PagerView::from_text("Output", &text, 98);
        render_screen(&pager, 100, 24);
        let _ = pager.handle_key(key(KeyCode::Char('/')));
        for ch in "needle".chars() {
            let _ = pager.handle_key(key(KeyCode::Char(ch)));
        }
        let _ = pager.handle_key(key(KeyCode::Enter));

        // Narrowing re-wraps every line into more rows.
        let screen = render_screen(&pager, 60, 24);
        let first_body_row = screen
            .lines()
            .find(|row| row.contains("row "))
            .unwrap_or_default();
        assert!(
            first_body_row.contains("row 40 needle"),
            "the line on top must stay on top after a resize:\n{screen}"
        );
        assert!(screen.contains("match 1/1"), "{screen}");
        let rows = pager.current_page().rows();
        assert_eq!(pager.search_total.get(), 1);
        assert!(
            rows.plain[pager.search_row.get().expect("selected match") - rows.start]
                .contains("needle")
        );
    }

    #[test]
    fn wrap_text_breaks_overlong_cjk_runs() {
        let text = "这是一个非常长的中文字符串".repeat(10);
        let lines = wrap_text(&text, 16);

        for line in &lines {
            assert!(line.width() <= 16, "line {line:?} exceeds width 16");
        }

        assert_eq!(lines.join(""), text);
    }
}
