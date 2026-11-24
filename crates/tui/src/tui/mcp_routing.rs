//! MCP UI action helpers: where `/mcp` lands and how MCP receipts reach
//! the transcript.

use crate::tui::app::App;
use crate::tui::history::HistoryCell;

/// `/mcp` and every MCP action land on Plugins → Connectors, the one MCP
/// surface, rebuilt from the snapshot just stored on `app`. A pager full of
/// text was the 0.9.12 "terrible menu" (defects #17–#20). An Extensions view
/// already on top is replaced so it reads the fresh snapshot.
pub(super) fn open_mcp_extensions(app: &mut App) {
    use crate::tui::views::{
        ModalKind,
        extensions::{ExtensionsTab, ExtensionsView},
    };
    if app.view_stack.top_kind() == Some(ModalKind::Extensions) {
        app.view_stack.pop();
    }
    app.view_stack
        .push(ExtensionsView::new(app, ExtensionsTab::Mcp));
    app.needs_redraw = true;
}

pub(super) fn add_mcp_message(app: &mut App, content: String) {
    app.add_message(HistoryCell::System { content });
}
