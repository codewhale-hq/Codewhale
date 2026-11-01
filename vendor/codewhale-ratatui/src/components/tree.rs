//! WorkflowTree: nested work (a run, its phases, their agents) as one tree.
//!
//! ```text
//! ● Release notes   working · 4m 06s · $0.38
//! ├ ✓ Gather        done · 3 agents · 1m 12s
//! ├ ● Draft         working · 2 of 4 agents
//! │ ├ ✕ writer-2    failed · tests did not pass →
//! │ └ ● writer-1    working · Editing CHANGELOG.md · 38 s
//! └ ○ Review        ready · +3 hidden · 2 ready · 1 working
//!   1 failed · 2 working · 3 ready
//! ```
//!
//! Every row carries its state's mark and word. A collapsed parent reports
//! the real number of nodes it hides and how they stand, so folding a phase
//! never hides a failure. Guides are `├ └ │`, or `+ \ |` in ASCII-safe
//! terminals. The summary line counts the whole tree, not what fits.
//!
//! [`TreeState`] holds the selection, the scroll offset and which nodes the
//! person flipped from their default (`TreeNode::collapsed`), and
//! [`TreeState::handle_key`] turns keys into a [`TreeOutcome`] the host acts
//! on. Modelled on the engine's `widgets/workflow_panel.rs` and
//! `history/checklist.rs` (`Hmbown/CodeWhale` `58b1dd3dd`).

use std::{borrow::Cow, collections::BTreeSet};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{Paint, Role, State, StateWords, StatusMark, Theme, glyphs, text};

/// One node: a state, a label, and optionally words about it and children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub label: Cow<'static, str>,
    pub state: State,
    /// What it is doing, or why it failed.
    pub detail: Option<Cow<'static, str>>,
    /// Measured facts: `1m 12s · $0.38`.
    pub receipt: Option<Cow<'static, str>>,
    pub children: Vec<TreeNode>,
    /// Whether its children start folded. [`TreeState`] flips this per node.
    pub collapsed: bool,
    /// Draws `→`: there is a receipt or detail to open.
    pub opens: bool,
}

impl TreeNode {
    #[must_use]
    pub fn new(state: State, label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            label: label.into(),
            state,
            detail: None,
            receipt: None,
            children: Vec::new(),
            collapsed: false,
            opens: false,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<Cow<'static, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn receipt(mut self, receipt: impl Into<Cow<'static, str>>) -> Self {
        self.receipt = Some(receipt.into());
        self
    }

    #[must_use]
    pub fn child(mut self, child: TreeNode) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = TreeNode>) -> Self {
        self.children.extend(children);
        self
    }

    #[must_use]
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    #[must_use]
    pub fn opens(mut self) -> Self {
        self.opens = true;
        self
    }

    /// How many nodes sit below this one, at every depth.
    #[must_use]
    pub fn descendants(&self) -> usize {
        self.children.iter().map(|c| 1 + c.descendants()).sum()
    }
}

/// Counts by state, in [`State::ALL`] order.
type Counts = [usize; 7];

fn state_index(state: State) -> usize {
    State::ALL.iter().position(|s| *s == state).unwrap_or(0)
}

/// `node`'s descendants (not `node`) by state.
fn count_below(node: &TreeNode, counts: &mut Counts) {
    for child in &node.children {
        counts[state_index(child.state)] += 1;
        count_below(child, counts);
    }
}

/// Every node of `nodes` by state, folded or not.
#[must_use]
pub fn tree_counts(nodes: &[TreeNode]) -> Vec<(State, usize)> {
    let mut counts: Counts = [0; 7];
    for node in nodes {
        counts[state_index(node.state)] += 1;
        count_below(node, &mut counts);
    }
    ordered(&counts)
}

/// What a person scans for first: failures, then what needs them, then what
/// is moving.
const SUMMARY_ORDER: [State; 7] = [
    State::Failed,
    State::NeedsYou,
    State::Working,
    State::Ready,
    State::Stopped,
    State::Unknown,
    State::Done,
];

fn ordered(counts: &Counts) -> Vec<(State, usize)> {
    SUMMARY_ORDER
        .iter()
        .map(|s| (*s, counts[state_index(*s)]))
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// Words the tree prints. `Default` is English, in lowercase because they
/// sit mid-sentence (`1 failed · 2 working`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeWords {
    /// The word beside each state mark, and in counts.
    pub states: StateWords,
    /// `+3 hidden`: nodes a folded parent covers.
    pub hidden: Cow<'static, str>,
    /// `+4 more`: rows below the window.
    pub more: Cow<'static, str>,
}

impl Default for TreeWords {
    fn default() -> Self {
        Self {
            states: StateWords {
                working: Cow::Borrowed("working"),
                done: Cow::Borrowed("done"),
                needs_you: Cow::Borrowed("needs you"),
                failed: Cow::Borrowed("failed"),
                stopped: Cow::Borrowed("stopped"),
                ready: Cow::Borrowed("ready"),
                unknown: Cow::Borrowed("unknown"),
            },
            hidden: Cow::Borrowed("hidden"),
            more: Cow::Borrowed("more"),
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A row on screen, with where it sits in the tree.
struct Visible<'a> {
    path: Vec<usize>,
    node: &'a TreeNode,
    /// For each ancestor below the roots: whether it was the last sibling.
    trail: Vec<bool>,
    is_last: bool,
    collapsed: bool,
}

/// Which row is selected, how far the tree has scrolled, and which nodes the
/// person folded or unfolded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeState {
    /// Index into the visible rows.
    pub selected: usize,
    pub offset: usize,
    /// Paths whose folded state is the opposite of their node's default.
    flipped: BTreeSet<Vec<usize>>,
}

/// What a key did, for the host to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeOutcome {
    Ignored,
    /// The selection moved.
    Moved,
    Expanded,
    Collapsed,
    /// Enter on this node (its path of child indices): open its receipt.
    Opened(Vec<usize>),
    Cancelled,
}

impl TreeState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `node` at `path` is folded now.
    #[must_use]
    pub fn is_collapsed(&self, path: &[usize], node: &TreeNode) -> bool {
        !node.children.is_empty() && (node.collapsed != self.flipped.contains(path))
    }

    fn flip(&mut self, path: &[usize]) {
        if !self.flipped.remove(path) {
            self.flipped.insert(path.to_vec());
        }
    }

    fn visible<'a>(&self, nodes: &'a [TreeNode]) -> Vec<Visible<'a>> {
        fn walk<'a>(
            nodes: &'a [TreeNode],
            state: &TreeState,
            path: &mut Vec<usize>,
            trail: &mut Vec<bool>,
            out: &mut Vec<Visible<'a>>,
        ) {
            for (i, node) in nodes.iter().enumerate() {
                path.push(i);
                let collapsed = state.is_collapsed(path, node);
                out.push(Visible {
                    path: path.clone(),
                    node,
                    trail: trail.clone(),
                    is_last: i + 1 == nodes.len(),
                    collapsed,
                });
                if !collapsed {
                    // Roots draw no guide, so they add nothing to the trail.
                    let guided = path.len() > 1;
                    if guided {
                        trail.push(i + 1 == nodes.len());
                    }
                    walk(&node.children, state, path, trail, out);
                    if guided {
                        trail.pop();
                    }
                }
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(nodes, self, &mut Vec::new(), &mut Vec::new(), &mut out);
        out
    }

    /// How many rows are on screen with the current folds.
    #[must_use]
    pub fn visible_len(&self, nodes: &[TreeNode]) -> usize {
        self.visible(nodes).len()
    }

    /// The selected node's path of child indices, if there is one.
    #[must_use]
    pub fn selected_path(&self, nodes: &[TreeNode]) -> Option<Vec<usize>> {
        let rows = self.visible(nodes);
        rows.get(self.selected.min(rows.len().saturating_sub(1)))
            .map(|r| r.path.clone())
    }

    /// The rows of a `len`-row tree to show in `rows` rows: `(offset,
    /// shown)`. When some rows are left below the window the last row is
    /// spent on `+n more` (`shown` is one less than `rows`), and the
    /// selection is always inside the shown rows.
    #[must_use]
    pub fn window(&self, len: usize, rows: usize) -> (usize, usize) {
        if rows == 0 || len == 0 {
            return (0, 0);
        }
        let selected = self.selected.min(len - 1);
        if len <= rows {
            return (0, len);
        }
        if rows == 1 {
            return (selected, 1);
        }
        let usable = rows - 1;
        let mut offset = self.offset.min(len - usable);
        if selected < offset {
            offset = selected;
        } else if selected >= offset + usable {
            offset = selected + 1 - usable;
        }
        if offset + rows >= len {
            (len - rows, rows)
        } else {
            (offset, usable)
        }
    }

    /// Store the offset the tree paints with, so scrolling is stable.
    pub fn scroll_into_view(&mut self, len: usize, rows: u16) {
        self.selected = self.selected.min(len.saturating_sub(1));
        self.offset = self.window(len, usize::from(rows)).0;
    }

    /// Apply one key. Up and Down, PageUp and PageDown, Home and End move;
    /// Right unfolds (or steps into the first child), Left folds (or steps
    /// out to the parent), Space toggles, Enter asks the host to open the
    /// node, Esc cancels. Releases are ignored. `rows` is the height the tree
    /// paints in.
    pub fn handle_key(&mut self, key: KeyEvent, nodes: &[TreeNode], rows: u16) -> TreeOutcome {
        if key.kind == KeyEventKind::Release {
            return TreeOutcome::Ignored;
        }
        let rows_now = self.visible(nodes);
        let len = rows_now.len();
        if len == 0 {
            return TreeOutcome::Ignored;
        }
        let at = self.selected.min(len - 1);
        let page = usize::from(rows.max(2)) - 1;
        let current = &rows_now[at];
        let has_children = !current.node.children.is_empty();
        let mut moved_to = None;
        let outcome = match key.code {
            KeyCode::Up => {
                moved_to = Some(at.saturating_sub(1));
                None
            }
            KeyCode::Down => {
                moved_to = Some((at + 1).min(len - 1));
                None
            }
            KeyCode::PageUp => {
                moved_to = Some(at.saturating_sub(page));
                None
            }
            KeyCode::PageDown => {
                moved_to = Some((at + page).min(len - 1));
                None
            }
            KeyCode::Home => {
                moved_to = Some(0);
                None
            }
            KeyCode::End => {
                moved_to = Some(len - 1);
                None
            }
            KeyCode::Right if has_children && current.collapsed => {
                let path = current.path.clone();
                self.flip(&path);
                Some(TreeOutcome::Expanded)
            }
            KeyCode::Right if has_children => {
                moved_to = Some(at + 1);
                None
            }
            KeyCode::Left if has_children && !current.collapsed => {
                let path = current.path.clone();
                self.flip(&path);
                Some(TreeOutcome::Collapsed)
            }
            KeyCode::Left if current.path.len() > 1 => {
                let parent = &current.path[..current.path.len() - 1];
                moved_to = rows_now.iter().position(|r| r.path == parent);
                None
            }
            KeyCode::Char(' ') if has_children => {
                let path = current.path.clone();
                let was = current.collapsed;
                self.flip(&path);
                Some(if was {
                    TreeOutcome::Expanded
                } else {
                    TreeOutcome::Collapsed
                })
            }
            KeyCode::Enter => Some(TreeOutcome::Opened(current.path.clone())),
            KeyCode::Esc => Some(TreeOutcome::Cancelled),
            _ => Some(TreeOutcome::Ignored),
        };
        let outcome = match (outcome, moved_to) {
            (Some(outcome), _) => outcome,
            (None, Some(to)) if to != at => {
                self.selected = to;
                TreeOutcome::Moved
            }
            _ => TreeOutcome::Ignored,
        };
        let len_after = self.visible_len(nodes);
        self.scroll_into_view(len_after, rows);
        outcome
    }
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

/// The nodes to paint, with optional selection state.
#[derive(Clone, Debug)]
pub struct WorkflowTree<'a> {
    pub nodes: &'a [TreeNode],
    pub state: Option<&'a TreeState>,
    /// A last line counting every node by state.
    pub summary: bool,
    pub words: TreeWords,
}

impl<'a> WorkflowTree<'a> {
    #[must_use]
    pub fn new(nodes: &'a [TreeNode]) -> Self {
        Self {
            nodes,
            state: None,
            summary: false,
            words: TreeWords::default(),
        }
    }

    /// Paint with this selection and these folds.
    #[must_use]
    pub fn state(mut self, state: &'a TreeState) -> Self {
        self.state = Some(state);
        self
    }

    /// Add the summary line.
    #[must_use]
    pub fn summary(mut self) -> Self {
        self.summary = true;
        self
    }

    #[must_use]
    pub fn words(mut self, words: TreeWords) -> Self {
        self.words = words;
        self
    }

    fn summary_text(&self, ascii: bool) -> String {
        let sep = if ascii { " - " } else { " \u{b7} " };
        self.counts_text(&tree_counts(self.nodes), sep)
    }

    fn counts_text(&self, counts: &[(State, usize)], sep: &str) -> String {
        counts
            .iter()
            .map(|(s, n)| format!("{n} {}", text::display_safe(self.words.states.get(*s))))
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// The rows the tree shows for a state: the folds it holds, or its
    /// nodes' defaults.
    fn rows(&self) -> Vec<Visible<'a>> {
        match self.state {
            Some(state) => state.visible(self.nodes),
            None => TreeState::default().visible(self.nodes),
        }
    }
}

/// `├ `, `└ `, `│ ` and the blanks between.
fn guide_cells(ascii: bool, row: &Visible<'_>, skip: usize) -> String {
    let (tee, ell, bar) = if ascii {
        ("+ ", "\\ ", "| ")
    } else {
        ("\u{251c} ", "\u{2514} ", "\u{2502} ")
    };
    let mut out = String::new();
    for last in row.trail.iter().skip(skip) {
        out.push_str(if *last { "  " } else { bar });
    }
    if row.path.len() > 1 {
        out.push_str(if row.is_last { ell } else { tee });
    }
    out
}

impl Paint for WorkflowTree<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let rows = self.rows();
        let summary = self.summary && area.height >= 2 && !rows.is_empty();
        let body_rows = usize::from(area.height) - usize::from(summary);
        let default = TreeState::default();
        let state = self.state.unwrap_or(&default);
        let (offset, shown) = state.window(rows.len(), body_rows);
        let selected = self
            .state
            .map(|s| s.selected.min(rows.len().saturating_sub(1)));
        let width = usize::from(area.width);
        let select_w = if self.state.is_some() { 2 } else { 0 };
        let window = &rows[offset..offset + shown];

        // Guides deeper than a third of the width would crowd the words out:
        // drop the outermost levels, never the row.
        let skip_of = |row: &Visible<'_>| -> usize {
            let max_levels = (width / 3).saturating_sub(select_w) / 2;
            (row.trail.len() + 1).saturating_sub(max_levels.max(1))
        };
        let lefts: Vec<usize> = window
            .iter()
            .map(|r| select_w + text::width(&guide_cells(ascii, r, skip_of(r))) + 2)
            .collect();
        let want = window
            .iter()
            .zip(&lefts)
            .map(|(r, left)| left + text::width(&text::display_safe(&r.node.label)))
            .max()
            .unwrap_or(0)
            + 2;
        let tail_col = want.min(width * 3 / 5).max(1);

        let sep = if ascii { " - " } else { " \u{b7} " };
        for (n, (row, left)) in window.iter().zip(&lefts).enumerate() {
            let y = area.y + n as u16;
            let rect = Rect::new(area.x, y, area.width, 1);
            let is_selected = selected == Some(offset + n);
            let node = row.node;
            let mut spans: Vec<Span<'static>> = Vec::new();
            if self.state.is_some() {
                let marker = glyphs::pick(glyphs::selection_marker(is_selected), ascii);
                spans.push(Span::styled(format!("{marker} "), theme.fg(Role::Accent)));
            }
            let guides = guide_cells(ascii, row, skip_of(row));
            if !guides.is_empty() {
                spans.push(Span::styled(guides, theme.fg(Role::Dim)));
            }
            let mark = StatusMark::new(node.state);
            spans.push(Span::styled(mark.glyph(theme), theme.fg(node.state.role())));
            spans.push(Span::raw(" "));
            let label_room = tail_col.saturating_sub(*left + 2).max(1);
            let label_room = label_room.min(width.saturating_sub(*left).max(1));
            let label = text::display_safe(&node.label);
            let label_style = if is_selected {
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
            } else {
                theme.fg(Role::Foreground)
            };
            spans.push(Span::styled(
                text::pad(&label, label_room, ascii),
                label_style,
            ));

            // The tail: word, detail, receipt, then what a fold hides.
            let mut room = width.saturating_sub(left + label_room);
            let loud = matches!(node.state, State::Failed | State::NeedsYou);
            let mut parts: Vec<(String, Role)> = vec![(
                text::display_safe(self.words.states.get(node.state)).into_owned(),
                Role::Muted,
            )];
            if let Some(detail) = &node.detail {
                let role = if loud { Role::Foreground } else { Role::Muted };
                parts.push((text::display_safe(detail).into_owned(), role));
            }
            if let Some(receipt) = &node.receipt {
                parts.push((text::display_safe(receipt).into_owned(), Role::Muted));
            }
            if row.collapsed {
                let hidden = node.descendants();
                let mut counts: Counts = [0; 7];
                count_below(node, &mut counts);
                let mut summary = format!("+{hidden} {}", text::display_safe(&self.words.hidden));
                let by_state = self.counts_text(&ordered(&counts), sep);
                if !by_state.is_empty() {
                    summary.push_str(sep);
                    summary.push_str(&by_state);
                }
                parts.push((summary, Role::Muted));
            }
            let arrow = if ascii { " >" } else { " \u{2192}" };
            let arrow_w = if node.opens { text::width(arrow) } else { 0 };
            room = room.saturating_sub(2 + arrow_w);
            let mut first = true;
            for (part, role) in parts {
                let lead = if first { "  " } else { sep };
                let lead_w = text::width(lead);
                if room <= lead_w {
                    break;
                }
                let fitted = text::truncate(&part, room - lead_w, ascii).into_owned();
                let cut = text::width(&fitted) < text::width(&part);
                room -= lead_w + text::width(&fitted);
                spans.push(Span::styled(
                    lead.to_string(),
                    theme.fg(if first { Role::Foreground } else { Role::Dim }),
                ));
                spans.push(Span::styled(fitted, theme.fg(role)));
                first = false;
                if cut {
                    break;
                }
            }
            if node.opens {
                spans.push(Span::styled(arrow, theme.fg(Role::Muted)));
            }
            if is_selected {
                buf.set_style(rect, theme.bg(Role::Selected));
            }
            Line::from(spans).render(rect, buf);
        }

        let mut y = area.y + shown as u16;
        if shown < rows.len() && shown == body_rows.saturating_sub(1) && offset + shown < rows.len()
        {
            let below = rows.len() - offset - shown;
            let more = format!("+{below} {}", text::display_safe(&self.words.more));
            let more = text::truncate(&more, width.saturating_sub(select_w), ascii);
            let rect = Rect::new(area.x, y, area.width, 1);
            Line::from(vec![
                Span::raw(" ".repeat(select_w)),
                Span::styled(more.into_owned(), theme.fg(Role::Muted)),
            ])
            .render(rect, buf);
            y += 1;
        }
        if summary && y < area.bottom() {
            let counts = self.summary_text(ascii);
            let line = text::truncate(&counts, width.saturating_sub(select_w), ascii);
            let rect = Rect::new(area.x, area.bottom() - 1, area.width, 1);
            Line::from(vec![
                Span::raw(" ".repeat(select_w)),
                Span::styled(line.into_owned(), theme.fg(Role::Muted)),
            ])
            .render(rect, buf);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        let rows = self.rows().len() + usize::from(self.summary && !self.nodes.is_empty());
        u16::try_from(rows).unwrap_or(u16::MAX)
    }
}
