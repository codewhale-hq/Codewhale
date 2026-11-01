//! Pure fixture facts for workspace composition and the summoned workbar.
use super::Entry;
use crate::{ContextItem, ContextRibbon, Paint, PaneHeader, Role, Theme, WorkspaceFrame};
use ratatui::{buffer::Buffer, layout::Rect};

pub(crate) fn context() -> ContextRibbon<'static> {
    ContextRibbon::new(vec![
        ContextItem::new("", "codewhale-ratatui").priority(3),
        ContextItem::new("", "workspace-polish").priority(2),
        ContextItem::new("", "Local / selected model")
            .priority(0)
            .role(Role::Accent),
        ContextItem::new("", "3 attachments").priority(1),
    ])
}
fn frame(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let frame = WorkspaceFrame::new("codewhale-ratatui")
        .branch("workspace-polish")
        .footer("Illustrative workspace / host owns sessions and actions");
    frame.paint(area, buf, theme);
    let areas = frame.areas(area);
    PaneHeader::new("Conversation")
        .meta("Today")
        .paint(areas.main, buf, theme);
    if let Some(side) = areas.side {
        PaneHeader::new("Files & review")
            .meta("4 changed")
            .paint(side, buf, theme);
    }
}
fn pane(area: Rect, buf: &mut Buffer, theme: &Theme) {
    PaneHeader::new("Files & review")
        .meta("4 changed")
        .focused(true)
        .paint(area, buf, theme);
}
fn ribbon(area: Rect, buf: &mut Buffer, theme: &Theme) {
    context().paint(area, buf, theme);
}
fn ribbon_narrow(area: Rect, buf: &mut Buffer, theme: &Theme) {
    context().paint(area, buf, theme);
}
pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "workbench-frame",
            width: 112,
            height: 12,
            draw: frame,
        },
        Entry {
            name: "pane-header",
            width: 48,
            height: 3,
            draw: pane,
        },
        Entry {
            name: "context-ribbon",
            width: 100,
            height: 2,
            draw: ribbon,
        },
        Entry {
            name: "context-ribbon-narrow",
            width: 40,
            height: 2,
            draw: ribbon_narrow,
        },
    ]
}
