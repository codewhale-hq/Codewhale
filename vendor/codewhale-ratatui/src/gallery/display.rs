//! Gallery entries for package Display: the receipt, diff, tree and
//! progress. The toast's entries live in `toast.rs`.

use std::ops::Range;

use ratatui::{buffer::Buffer, layout::Rect};

use super::Entry;
use crate::{
    CountBar, Diff, DiffGutter, DiffWrap, Paint, Receipt, ReceiptColumn, ReceiptDensity,
    ReceiptTable, ReceiptValue, Role, State, Theme, TreeNode, TreeState, WorkflowTree,
    parse_unified,
};
use std::time::Duration;

use crate::Cost;

// ---------------------------------------------------------------------------
// Receipts
// ---------------------------------------------------------------------------

fn columns() -> Vec<ReceiptColumn> {
    vec![
        ReceiptColumn::new("Time").essential(),
        ReceiptColumn::new("Tokens"),
        ReceiptColumn::new("Cost").essential(),
    ]
}

fn secs(s: u64) -> ReceiptValue {
    ReceiptValue::Duration(Some(Duration::from_secs(s)))
}

/// Language text is outside the ASCII charter, but an ASCII-safe frame must
/// stay ASCII, so the fixtures swap in a Latin line there.
fn words(ascii: bool, text: &'static str, latin: &'static str) -> &'static str {
    if ascii { latin } else { text }
}

fn rows(ascii: bool) -> Vec<Receipt> {
    vec![
        Receipt::new(State::Done, "Edited summary.md").values([
            secs(4),
            ReceiptValue::Tokens(Some(1_240)),
            ReceiptValue::Cost(Some(Cost::usd_cents(1))),
        ]),
        Receipt::new(State::Failed, "Ran cargo test")
            .values([
                secs(72),
                ReceiptValue::Tokens(Some(12_400)),
                ReceiptValue::Cost(None),
            ])
            .opens(),
        Receipt::new(State::Done, "Read 14 files").values([
            ReceiptValue::Duration(Some(Duration::from_millis(850))),
            ReceiptValue::Tokens(None),
            ReceiptValue::Cost(Some(Cost::usd_cents(0))),
        ]),
        Receipt::new(State::Working, "Writing CHANGELOG.md").values([
            secs(38),
            ReceiptValue::Tokens(Some(96_300)),
            ReceiptValue::Cost(Some(Cost::new(382_000, "$").estimate())),
        ]),
        Receipt::new(
            State::Stopped,
            words(
                ascii,
                "整理发布说明，翻译成中文",
                "Translate the release notes",
            ),
        )
        .values([
            secs(3_720),
            ReceiptValue::Tokens(Some(1_950_000)),
            ReceiptValue::Cost(Some(Cost::usd_cents(1_240))),
        ]),
    ]
}

fn receipt_row(area: Rect, buf: &mut Buffer, theme: &Theme) {
    Receipt::new(State::Done, "Edited summary.md")
        .values([
            secs(4),
            ReceiptValue::Tokens(Some(1_240)),
            ReceiptValue::Cost(Some(Cost::usd_cents(1))),
        ])
        .opens()
        .paint(area, buf, theme);
}

fn table(density: Option<ReceiptDensity>, ascii: bool) -> ReceiptTable {
    let table = ReceiptTable::new(columns(), rows(ascii))
        .label_header("Action")
        .totals();
    match density {
        Some(d) => table.density(d),
        None => table,
    }
}

fn receipt_table(area: Rect, buf: &mut Buffer, theme: &Theme) {
    table(None, theme.ascii()).paint(area, buf, theme);
}

fn receipt_table_compact(area: Rect, buf: &mut Buffer, theme: &Theme) {
    table(Some(ReceiptDensity::Compact), theme.ascii()).paint(area, buf, theme);
}

fn receipt_table_minimal(area: Rect, buf: &mut Buffer, theme: &Theme) {
    table(Some(ReceiptDensity::Minimal), theme.ascii()).paint(area, buf, theme);
}

fn receipt_table_clipped(area: Rect, buf: &mut Buffer, theme: &Theme) {
    table(None, theme.ascii()).paint(area, buf, theme);
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

const PATCH: &str = concat!(
    "diff --git a/src/lib.rs b/src/lib.rs\n",
    "index 3f2a1c9..8b7d4e0 100644\n",
    "--- a/src/lib.rs\n",
    "+++ b/src/lib.rs\n",
    "@@ -10,5 +10,6 @@ fn main() {\n",
    " fn main() {\n",
    "     let name = \"whale\";\n",
    "-    let depth = 1;\n",
    "+    let depth = 2;\n",
    "+    let pods = 3;\n",
    "     println!(\"{name}\");\n",
    " }\n",
    "@@ -40,3 +41,3 @@ impl Pod {\n",
    " \tfn describe(&self) -> String {\n",
    "-\t\tformat!(\"a pod of {} whales swimming together in the deep blue ocean at night\", self.count)\n",
    "+\t\tformat!(\"a pod of {} whales swimming together in the deep blue ocean at dawn\", self.count)\n",
    " \t}\n",
);

/// A hunk of language text, for terminals that can show it.
const CJK_HUNK: &str = concat!(
    "@@ -70,2 +71,2 @@ impl Pod {\n",
    "-    let note = \"鲸鱼在深海里游动\";\n",
    "+    let note = \"鲸鱼在深蓝的海里游动\";\n",
    " }\n",
);

fn diff_lines<'a>(
    patch: &'a str,
    old: &'a [Range<usize>],
    new: &'a [Range<usize>],
) -> Vec<crate::DiffLine<'a>> {
    let mut lines = parse_unified(patch);
    for line in &mut lines {
        if line.text.contains("at night") {
            line.emphasis = old;
        } else if line.text.contains("at dawn") {
            line.emphasis = new;
        }
    }
    lines
}

/// Where `word` sits in the text of the patch line containing `marker`
/// (the text starts after the sign).
fn word_range(marker: &str, word: &str) -> Vec<Range<usize>> {
    let text = PATCH
        .lines()
        .find(|l| l.contains(marker))
        .map_or("", |l| &l[1..]);
    let at = text.find(word).unwrap_or(0);
    std::iter::once(at..at + word.len()).collect()
}

fn paint_diff(area: Rect, buf: &mut Buffer, theme: &Theme, build: impl Fn(Diff) -> Diff) {
    let old = word_range("at night", "night");
    let new = word_range("at dawn", "dawn");
    let patch = if theme.ascii() {
        PATCH.to_string()
    } else {
        format!("{PATCH}{CJK_HUNK}")
    };
    build(Diff::new(diff_lines(&patch, &old, &new))).paint(area, buf, theme);
}

fn diff_truncated(area: Rect, buf: &mut Buffer, theme: &Theme) {
    paint_diff(area, buf, theme, |d| d);
}

fn diff_wrapped(area: Rect, buf: &mut Buffer, theme: &Theme) {
    paint_diff(area, buf, theme, |d| d.wrap(DiffWrap::Wrap));
}

fn diff_no_numbers(area: Rect, buf: &mut Buffer, theme: &Theme) {
    paint_diff(area, buf, theme, |d| d.gutter(DiffGutter::Off).scroll(4));
}

/// A stand-in for a caller's highlighter: keywords in `Primary`, string
/// literals in `Muted`.
fn keywords(line: &str) -> Vec<(Range<usize>, Role)> {
    let mut out = Vec::new();
    for word in ["fn", "let", "impl", "format!", "println!"] {
        let mut from = 0;
        while let Some(at) = line[from..].find(word) {
            let start = from + at;
            out.push((start..start + word.len(), Role::Accent));
            from = start + word.len();
        }
    }
    if let (Some(open), Some(close)) = (line.find('"'), line.rfind('"'))
        && close > open
    {
        out.push((open..close + 1, Role::Muted));
    }
    out
}

fn diff_highlighted(area: Rect, buf: &mut Buffer, theme: &Theme) {
    paint_diff(area, buf, theme, |d| d.highlight(keywords).scroll(4));
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

fn run() -> Vec<TreeNode> {
    vec![
        TreeNode::new(State::Working, "Release notes")
            .detail("4m 06s")
            .receipt("$0.38")
            .children([
                TreeNode::new(State::Done, "Gather")
                    .detail("3 agents")
                    .receipt("1m 12s")
                    .collapsed(true)
                    .children([
                        TreeNode::new(State::Done, "reader-1").receipt("40 s"),
                        TreeNode::new(State::Done, "reader-2").receipt("52 s"),
                        TreeNode::new(State::Done, "reader-3").receipt("1m 12s"),
                    ]),
                TreeNode::new(State::Working, "Draft")
                    .detail("2 of 4 agents")
                    .children([
                        TreeNode::new(State::Working, "writer-1")
                            .detail("Editing CHANGELOG.md")
                            .receipt("38 s"),
                        TreeNode::new(State::Failed, "writer-2")
                            .detail("tests did not pass")
                            .opens(),
                        TreeNode::new(State::Ready, "writer-3"),
                    ]),
                TreeNode::new(State::Ready, "Review"),
            ]),
    ]
}

fn workflow_tree(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let nodes = run();
    WorkflowTree::new(&nodes).summary().paint(area, buf, theme);
}

fn workflow_tree_selected(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let nodes = run();
    let mut state = TreeState::new();
    state.selected = 4;
    WorkflowTree::new(&nodes)
        .state(&state)
        .summary()
        .paint(area, buf, theme);
}

fn workflow_tree_clipped(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let nodes = run();
    let mut state = TreeState::new();
    state.selected = 1;
    WorkflowTree::new(&nodes)
        .state(&state)
        .summary()
        .paint(area, buf, theme);
}

fn workflow_tree_long(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let ascii = theme.ascii();
    let nodes = vec![
        TreeNode::new(
            State::NeedsYou,
            words(
                ascii,
                "整理发布说明并翻译成中文，然后提交审阅",
                "Tidy the release notes, translate them, then send for review",
            ),
        )
        .detail(words(
            ascii,
            "等待你批准运行 cargo publish，这一步不能撤销",
            "Waiting for you to approve cargo publish, which cannot be undone",
        ))
        .child(TreeNode::new(
            State::Unknown,
            "review-with-a-very-long-agent-name",
        )),
    ];
    WorkflowTree::new(&nodes).paint(area, buf, theme);
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

fn count_bars(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let bars = [
        CountBar::new(3, 5).label("phases"),
        CountBar::new(0, 5).label("files"),
        CountBar::new(5, 5).label("phases").state(State::Done),
        CountBar::new(7, 5).label("checks").state(State::NeedsYou),
        CountBar::unknown(12).label("files"),
        CountBar::new(0, 0).label("tasks").state(State::Ready),
    ];
    for (row, bar) in bars.iter().enumerate() {
        let Some(y) = area
            .y
            .checked_add(row as u16)
            .filter(|y| *y < area.bottom())
        else {
            break;
        };
        bar.paint(Rect::new(area.x, y, area.width, 1), buf, theme);
    }
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "receipt-row",
            width: 60,
            height: 1,
            draw: receipt_row,
        },
        Entry {
            name: "receipt-table",
            width: 80,
            height: 8,
            draw: receipt_table,
        },
        Entry {
            name: "receipt-table-compact",
            width: 56,
            height: 7,
            draw: receipt_table_compact,
        },
        Entry {
            name: "receipt-table-minimal",
            width: 40,
            height: 7,
            draw: receipt_table_minimal,
        },
        Entry {
            name: "receipt-table-clipped",
            width: 80,
            height: 5,
            draw: receipt_table_clipped,
        },
        Entry {
            name: "diff",
            width: 80,
            height: 22,
            draw: diff_truncated,
        },
        Entry {
            name: "diff-wrapped",
            width: 48,
            height: 26,
            draw: diff_wrapped,
        },
        Entry {
            name: "diff-no-numbers",
            width: 60,
            height: 10,
            draw: diff_no_numbers,
        },
        Entry {
            name: "diff-highlighted",
            width: 80,
            height: 12,
            draw: diff_highlighted,
        },
        Entry {
            name: "workflow-tree",
            width: 64,
            height: 9,
            draw: workflow_tree,
        },
        Entry {
            name: "workflow-tree-selected",
            width: 64,
            height: 9,
            draw: workflow_tree_selected,
        },
        Entry {
            name: "workflow-tree-clipped",
            width: 64,
            height: 6,
            draw: workflow_tree_clipped,
        },
        Entry {
            name: "workflow-tree-long",
            width: 48,
            height: 3,
            draw: workflow_tree_long,
        },
        Entry {
            name: "count-bars",
            width: 60,
            height: 6,
            draw: count_bars,
        },
    ]
}
