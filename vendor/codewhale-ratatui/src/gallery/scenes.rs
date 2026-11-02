//! Complete Codewhale compositions made only from the public kit. Every
//! value is illustrative host data, not a live Engine/provider measurement.
use super::{Entry, workbench};
use crate::{
    ApprovalCard, ApprovalChoice, ApprovalEffect, ApprovalKey, ApprovalKind, ApprovalScope,
    ApprovalState, ApprovalSubject, Artifact, ArtifactKind, ArtifactShelf, AttentionItem,
    AttentionQueue, ChoiceId, Composer, Cost, CountBar, Diff, DiffGutter, DiffWrap, Heading,
    KeyHint, KeyHints, Message, Paint, PaneHeader, Receipt, ReceiptColumn, ReceiptTable,
    ReceiptValue, ReviewKind, ReviewVerdict, Role, State, Theme, ToolCard, TreeNode, Whale,
    WhaleState, WorkflowTree, WorkspaceFrame, parse_unified,
};
use ratatui::{buffer::Buffer, layout::Rect};
use std::time::Duration;

fn band(area: Rect, top: u16, height: u16) -> Rect {
    let top = top.min(area.height);
    Rect::new(
        area.x,
        area.y.saturating_add(top),
        area.width,
        height.min(area.height.saturating_sub(top)),
    )
}
fn hints() -> KeyHints {
    KeyHints::new(vec![
        KeyHint::new("Enter", "send"),
        KeyHint::new("Tab", "details"),
        KeyHint::new("/", "commands"),
    ])
}
fn composer(main: Rect, buf: &mut Buffer, theme: &Theme, draft: &'static str) {
    let hints = hints();
    let composer = Composer::new(draft).hints(&hints).focused(true);
    let height = composer.height(main.width, theme).max(4).min(main.height);
    let footer = main.height.saturating_sub(height.saturating_add(1));
    workbench::context().paint(band(main, footer, 1), buf, theme);
    composer.paint(band(main, footer.saturating_add(1), height), buf, theme);
}
fn files() -> Vec<Artifact<'static>> {
    vec![
        Artifact::new("WorkspaceFrame", ArtifactKind::File, State::Done)
            .detail("src/components/workbench.rs")
            .action("Open"),
        Artifact::new("Habitat", ArtifactKind::File, State::Working)
            .detail("src/components/habitat.rs")
            .action("Open"),
        Artifact::new("Visual review", ArtifactKind::Review, State::NeedsYou)
            .detail("dark, light, narrow")
            .action("Review"),
        Artifact::new("Verification", ArtifactKind::Run, State::Done)
            .detail("local component checks")
            .receipt(
                "Time",
                ReceiptValue::Duration(Some(Duration::from_secs(12))),
            ),
    ]
}
fn workspace(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let frame = WorkspaceFrame::new("codewhale-ratatui").branch("workspace-polish").mode("Work").footer(if area.width < 96 { "Illustrative / Tab opens files & decisions" } else { "Illustrative host data / Conversation + Files / Ctrl+K commands / Workbar summoned below" });
    frame.paint(area, buf, theme);
    let regions = frame.areas(area);
    let main = regions.main;
    PaneHeader::new("Make the terminal feel like Codewhale")
        .meta("Today")
        .paint(band(main, 0, 2), buf, theme);
    let user = Message::new(
        "You",
        "Build a beautiful workspace. Keep the receipts legible.",
    )
    .role(Role::Accent);
    let user_height = user.height(main.width, theme);
    user.paint(band(main, 3, user_height), buf, theme);
    let reply_top = 4 + user_height;
    let reply = Message::new(
        "Codewhale",
        if regions.side.is_some() {
            "The conversation stays at the center. Files, decisions and agent work sit alongside it.\n\nYou can open a result, inspect a command, or compose a denser workspace when the job needs it."
        } else {
            "The conversation stays at the center. Files and decisions are one action away.\n\nI am keeping the narrow layout readable."
        },
    );
    let reply_height = reply.height(main.width, theme);
    reply.paint(
        band(
            main,
            reply_top,
            reply_height.min(main.height.saturating_sub(reply_top + 6)),
        ),
        buf,
        theme,
    );
    let tool_top = reply_top.saturating_add(reply_height + 1);
    if main.height >= tool_top.saturating_add(12) {
        ToolCard::new("Inspect", "workspace layout and rendered cells", State::Done).elapsed("12s").output("Wide: conversation + files\nNarrow: conversation, details on demand\nHost facts: preserved").paint(band(main, tool_top, 6), buf, theme);
        let workbar = super::workbar::sample(crate::WorkbarPanel::Tasks);
        let height = workbar
            .height(main.width, theme)
            .min(main.height.saturating_sub(tool_top + 12));
        workbar.paint(band(main, tool_top + 7, height), buf, theme);
    }
    composer(main, buf, theme, "Show me the review, then keep going.");
    if let Some(side) = regions.side {
        PaneHeader::new("Files & review")
            .meta("4 items")
            .paint(band(side, 0, 2), buf, theme);
        ArtifactShelf::new(files())
            .selected(2)
            .paint(band(side, 3, 9), buf, theme);
        let attention = AttentionItem::new(
            "Terminal",
            "Review",
            "Choose which preview to inspect first.",
            State::NeedsYou,
        )
        .action("Open visual review")
        .selected(true)
        .focused(true);
        attention.paint(band(side, 14, 7), buf, theme);
        Whale::new(WhaleState::Think)
            .words("Working alongside you")
            .paint(band(side, 23, 11), buf, theme);
    }
}

const PATCH: &str = "--- a/workspace.rs\n+++ b/workspace.rs\n@@ -18,3 +18,7 @@\n fn draw_workspace(area: Rect) {\n-    draw_everything_in_one_column(area);\n+    let regions = frame.areas(area);\n+    conversation.paint(regions.main);\n+    if let Some(side) = regions.side {\n+        files.paint(side);\n+    }\n }\n";
fn review(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let frame = WorkspaceFrame::new("codewhale-ratatui")
        .branch("workspace-polish")
        .mode("Review")
        .footer("Review changes / Enter inspect / Esc close");
    frame.paint(area, buf, theme);
    let regions = frame.areas(area);
    let main = regions.main;
    PaneHeader::new("A workspace that adapts")
        .meta("+5 / -1")
        .paint(band(main, 0, 2), buf, theme);
    Diff::new(parse_unified(PATCH))
        .gutter(DiffGutter::Both)
        .wrap(DiffWrap::Wrap)
        .paint(band(main, 3, 12), buf, theme);
    Heading::new("Run results")
        .section()
        .paint(band(main, 17, 2), buf, theme);
    receipts().paint(band(main, 20, 7), buf, theme);
    composer(
        main,
        buf,
        theme,
        "Keep the changes local until I inspect them.",
    );
    if let Some(side) = regions.side {
        PaneHeader::new("A decision, in context").paint(band(side, 0, 2), buf, theme);
        let subject = ApprovalSubject::new(ApprovalKind::Command, "git diff --check")
            .cwd("codewhale-ratatui")
            .scope(ApprovalScope::Inside)
            .agent("Verifier", true)
            .risk_note("checks whitespace in the current changes");
        let state = ApprovalState::new(vec![
            ApprovalChoice::new(ChoiceId(1), "Allow once", ApprovalEffect::Grants).char_key('y'),
            ApprovalChoice::new(ChoiceId(2), "Deny", ApprovalEffect::Refuses).char_key('n'),
        ])
        .reveal(ApprovalKey::char('o'));
        ApprovalCard::new(&subject, &state).paint(band(side, 3, 18), buf, theme);
        let hints = KeyHints::new(vec![KeyHint::new("Enter", "inspect")]);
        ReviewVerdict::new(
            ReviewKind::Allowed,
            "Layout review",
            "The optional dock yields to conversation when space is limited.",
            &hints,
            None,
        )
        .expect("allowed fixture verdict")
        .paint(band(side, 23, 9), buf, theme);
    }
}
fn receipts() -> ReceiptTable {
    ReceiptTable::new(
        vec![
            ReceiptColumn::new("Time").essential(),
            ReceiptColumn::new("Tokens"),
            ReceiptColumn::new("Cost").essential(),
        ],
        vec![
            Receipt::new(State::Done, "Inspect files").values([
                ReceiptValue::Duration(Some(Duration::from_secs(12))),
                ReceiptValue::Tokens(Some(1240)),
                ReceiptValue::Cost(Some(Cost::usd_cents(2))),
            ]),
            Receipt::new(State::Done, "Render previews").values([
                ReceiptValue::Duration(Some(Duration::from_secs(4))),
                ReceiptValue::Tokens(None),
                ReceiptValue::Cost(None),
            ]),
            Receipt::new(State::Working, "Review narrow layout").values([
                ReceiptValue::Duration(Some(Duration::from_secs(38))),
                ReceiptValue::Tokens(Some(4800)),
                ReceiptValue::Cost(Some(Cost::usd_cents(12))),
            ]),
        ],
    )
    .label_header("Reported work")
    .totals()
}

fn fleet(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let frame = WorkspaceFrame::new("codewhale-ratatui").branch("workspace-polish").mode("Station").side_width(0).footer("Illustrative / settled work keeps its receipt / host controls order, selection and actions");
    frame.paint(area, buf, theme);
    let main = frame.areas(area).main;
    PaneHeader::new("One goal. Three agents. Every result visible.")
        .meta("Station")
        .paint(band(main, 0, 2), buf, theme);
    Message::new(
        "You",
        "Build the workspace, review the interaction, and document the kit.",
    )
    .role(Role::Accent)
    .paint(band(main, 3, 3), buf, theme);
    let nodes = vec![
        TreeNode::new(State::Working, "Finish the terminal workspace").children([
            TreeNode::new(State::Done, "Builder / workspace composition")
                .detail("Responsive regions, module headers, composer context")
                .receipt("12 files / 2m 10s")
                .opens(),
            TreeNode::new(State::NeedsYou, "Reviewer / interaction and accessibility")
                .detail("Choose the narrow preview to inspect")
                .receipt("1 decision waiting")
                .opens(),
            TreeNode::new(State::Working, "Documentation / gallery and usage")
                .detail("Actual cell buffers for every terminal profile")
                .receipt("9 profiles / 38s")
                .opens(),
        ]),
    ];
    WorkflowTree::new(&nodes)
        .summary()
        .paint(band(main, 7, 7), buf, theme);
    CountBar::new(3, 5)
        .label("Reported tasks")
        .paint(band(main, 15, 2), buf, theme);
    if main.width >= 80 {
        let left = Rect {
            width: main.width / 2 - 2,
            ..band(main, 20, 8)
        };
        receipts().paint(left, buf, theme);
        let right = Rect::new(
            main.x.saturating_add(main.width / 2 + 2),
            main.y.saturating_add(20),
            main.width.saturating_sub(main.width / 2 + 2),
            8,
        )
        .intersection(main);
        let items = [AttentionItem::new(
            "Terminal",
            "Reviewer",
            "Inspect the 40-column composition before landing the change.",
            State::NeedsYou,
        )
        .action("Open narrow preview")];
        AttentionQueue::new(&items)
            .selected(0)
            .focused(true)
            .paint(right, buf, theme);
    }
    super::workbar::sample(crate::WorkbarPanel::Fleet).paint(
        band(main, main.height.saturating_sub(5), 5),
        buf,
        theme,
    );
}
pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "workspace-scene",
            width: 112,
            height: 38,
            draw: workspace,
        },
        Entry {
            name: "review-scene",
            width: 112,
            height: 38,
            draw: review,
        },
        Entry {
            name: "fleet-scene",
            width: 112,
            height: 38,
            draw: fleet,
        },
        Entry {
            name: "workspace-scene-narrow",
            width: 40,
            height: 28,
            draw: workspace,
        },
    ]
}
