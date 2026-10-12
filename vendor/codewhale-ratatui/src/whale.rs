//! The whale, drawn in Braille dots.
//!
//! The C-shaped whale from the Codewhale mark, as the v2 pet
//! (`whale-character-v2`), is the one character on every surface. The desktop
//! app draws it with the v2 rig; the terminal draws the same contours as
//! Braille dots. Its states carry meaning: the kit's 17 actions, from resting
//! and listening through reading, editing and running to needs you and done,
//! and a pod, where calves swim with it while agents work in parallel. The
//! art has room for three calves; the words always carry the real count, and
//! a pod of none is drawn as plain work, never with an invented calf.
//!
//! `assets/whale-v2.scenes` holds the poster pose of each state as exact
//! cubic contours, exported from the v2 kit's Director by
//! `tools/export-whale.cjs`. [`rasterize`] is a port of the kit's pure
//! vector-to-dot renderer (`braille.js`): flatten each cubic into ten
//! segments, fill scanlines by even-odd crossings at dot centres (so the
//! throat and eye holes stay open), and pack each 2x4 block into one
//! `U+2800` cell. It reproduces the kit's 32x16 and 20x10 stills exactly
//! (`tests/whale.rs`).
//!
//! The art is decoration; the state lives in the words beside it. Screen
//! readers get the label, never a stream of dot names, and ASCII-safe
//! terminals get the words alone.
//!
//! [`Whale::paint_frame`] accepts a packed frame from the host's shared
//! Director. This renderer owns no clock, animation or state selection;
//! [`Paint::paint`] keeps drawing the existing poster pose.

use std::borrow::Cow;
use std::sync::OnceLock;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::Widget,
};

use crate::{
    Paint, Role, State, StatusMark, Theme,
    color::{ColorDepth, blend, rgb, rgb_to_ansi256},
    theme::{LOGO_BOTTOM, LOGO_TOP},
};

/// Dot bits per cell, row-major: `01 08`, `02 10`, `04 20`, `40 80`.
pub const BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// What the whale is doing: the v2 kit's 17 actions (`catalogue.js`).
///
/// The host classifies; the whale only draws. Pick the action from the
/// engine's own presence and activity (the kit's `PORTING.md`), never by
/// guessing from a command, URL or text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WhaleState {
    /// Idle: nothing to do.
    Rest,
    /// The person is typing.
    Listen,
    /// First output pending, or reasoning reported.
    Think,
    /// Working, with no finer activity reported.
    Busy,
    /// Reading files.
    Read,
    /// Searching.
    Search,
    /// Editing files.
    Write,
    /// Running a command.
    Run,
    /// Browsing the web.
    Browse,
    /// Writing the reply.
    Talk,
    /// Working with `calves` agents in parallel. The art draws at most three
    /// calves (zero draws the working pose); the words use the real count.
    Pod { calves: u8 },
    /// Waiting on the person.
    NeedsYou,
    /// The turn finished.
    Done,
    /// The turn genuinely failed moments ago. The kit marks this state a
    /// proposal: the host shows it only for a failed turn and expires it
    /// (the kit uses three seconds).
    Stuck,
    /// The engine is offline.
    Asleep,
    /// Using the computer (computer use).
    Computer,
    /// Calling a connected app. Says the call happened, not that it worked.
    Connect,
}

impl WhaleState {
    /// Every action once, in the kit's catalogue order (the pod with three
    /// calves).
    pub const ALL: [WhaleState; 17] = [
        WhaleState::Rest,
        WhaleState::Listen,
        WhaleState::Think,
        WhaleState::Busy,
        WhaleState::Read,
        WhaleState::Search,
        WhaleState::Write,
        WhaleState::Run,
        WhaleState::Browse,
        WhaleState::Talk,
        WhaleState::Pod { calves: 3 },
        WhaleState::NeedsYou,
        WhaleState::Done,
        WhaleState::Stuck,
        WhaleState::Asleep,
        WhaleState::Computer,
        WhaleState::Connect,
    ];

    /// The action's name in the v2 kit (`catalogue.js`, `braille/<key>-*.txt`).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            WhaleState::Rest => "rest",
            WhaleState::Listen => "listen",
            WhaleState::Think => "think",
            WhaleState::Busy => "busy",
            WhaleState::Read => "read",
            WhaleState::Search => "search",
            WhaleState::Write => "write",
            WhaleState::Run => "run",
            WhaleState::Browse => "browse",
            WhaleState::Talk => "talk",
            WhaleState::Pod { .. } => "pod",
            WhaleState::NeedsYou => "needs",
            WhaleState::Done => "done",
            WhaleState::Stuck => "hmm",
            WhaleState::Asleep => "sleep",
            WhaleState::Computer => "computer",
            WhaleState::Connect => "connect",
        }
    }

    /// The action named `key` in the v2 kit. The pod comes back with three
    /// calves; set the real count on it.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// The key and calf count in `assets/whale-v2.scenes`.
    fn scene_key(self) -> (&'static str, u8) {
        match self {
            WhaleState::Pod { calves: 0 } => ("busy", 0),
            WhaleState::Pod { calves } => ("pod", calves.min(3)),
            other => (other.key(), 0),
        }
    }

    /// The status this pose stands for: its mark and hue beside the words.
    #[must_use]
    pub const fn state(self) -> State {
        match self {
            WhaleState::Rest | WhaleState::Listen => State::Ready,
            WhaleState::Think
            | WhaleState::Busy
            | WhaleState::Read
            | WhaleState::Search
            | WhaleState::Write
            | WhaleState::Run
            | WhaleState::Browse
            | WhaleState::Talk
            | WhaleState::Pod { .. }
            | WhaleState::Computer
            | WhaleState::Connect => State::Working,
            WhaleState::NeedsYou => State::NeedsYou,
            WhaleState::Done => State::Done,
            WhaleState::Stuck => State::Failed,
            WhaleState::Asleep => State::Stopped,
        }
    }

    /// The English words beside the art. Hosts pass localized words with
    /// [`Whale::words`].
    #[must_use]
    pub fn words(self) -> Cow<'static, str> {
        match self {
            WhaleState::Rest => "Resting".into(),
            WhaleState::Listen => "Listening".into(),
            WhaleState::Think => "Thinking".into(),
            WhaleState::Busy | WhaleState::Pod { calves: 0 } => "Working".into(),
            WhaleState::Read => "Reading".into(),
            WhaleState::Search => "Searching".into(),
            WhaleState::Write => "Editing".into(),
            WhaleState::Run => "Running a command".into(),
            WhaleState::Browse => "Browsing".into(),
            WhaleState::Talk => "Replying".into(),
            WhaleState::Pod { calves: 1 } => "Working with 1 agent".into(),
            WhaleState::Pod { calves } => format!("Working with {calves} agents").into(),
            WhaleState::NeedsYou => "Needs you".into(),
            WhaleState::Done => "Done".into(),
            WhaleState::Stuck => "Stuck".into(),
            WhaleState::Asleep => "Asleep".into(),
            WhaleState::Computer => "Using the computer".into(),
            WhaleState::Connect => "Calling a connected app".into(),
        }
    }
}

/// A closed contour: a start point and cubic segments `(c1, c2, end)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Contour {
    pub start: [f64; 2],
    pub cubics: Vec<[f64; 6]>,
}

/// One filled shape and its compound holes, filled even-odd together.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub id: String,
    pub role: String,
    pub contours: Vec<Contour>,
}

/// A posed whale at one optical size.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub state: String,
    pub calves: u8,
    pub size: u32,
    pub shapes: Vec<Shape>,
}

const SCENES_SOURCE: &str = include_str!("../assets/whale-v2.scenes");

/// Every exported scene, parsed once.
///
/// # Panics
/// Only if the checked-in asset is malformed, which `tests/whale.rs` rules
/// out.
pub fn scenes() -> &'static [Scene] {
    static SCENES: OnceLock<Vec<Scene>> = OnceLock::new();
    SCENES.get_or_init(|| parse_scenes(SCENES_SOURCE).expect("whale-v2.scenes parses"))
}

fn parse_scenes(src: &str) -> Result<Vec<Scene>, String> {
    let mut scenes: Vec<Scene> = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let err = |what: &str| format!("line {}: {what}", n + 1);
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            None | Some("#") => {}
            Some(w) if w.starts_with('#') => {}
            Some("scene") => {
                let state = words.next().ok_or_else(|| err("scene state"))?.to_string();
                let calves = words
                    .next()
                    .and_then(|w| w.parse().ok())
                    .ok_or_else(|| err("calves"))?;
                let size = words
                    .next()
                    .and_then(|w| w.parse().ok())
                    .ok_or_else(|| err("size"))?;
                scenes.push(Scene {
                    state,
                    calves,
                    size,
                    shapes: Vec::new(),
                });
            }
            Some("shape") => {
                let scene = scenes.last_mut().ok_or_else(|| err("shape before scene"))?;
                scene.shapes.push(Shape {
                    id: words.next().ok_or_else(|| err("shape id"))?.to_string(),
                    role: words.next().ok_or_else(|| err("shape role"))?.to_string(),
                    contours: Vec::new(),
                });
            }
            Some("path") => {
                let shape = scenes
                    .last_mut()
                    .and_then(|s| s.shapes.last_mut())
                    .ok_or_else(|| err("path before shape"))?;
                let nums: Vec<f64> = words
                    .map(|w| w.parse::<f64>().map_err(|_| err("number")))
                    .collect::<Result<_, _>>()?;
                if nums.len() < 2 || !(nums.len() - 2).is_multiple_of(6) {
                    return Err(err("path length"));
                }
                shape.contours.push(Contour {
                    start: [nums[0], nums[1]],
                    cubics: nums[2..].as_chunks::<6>().0.to_vec(),
                });
            }
            Some(other) => return Err(err(&format!("unknown record {other}"))),
        }
    }
    Ok(scenes)
}

/// The scene for `state` whose optical size is nearest `size`.
#[must_use]
pub fn scene(state: WhaleState, size: u32) -> Option<&'static Scene> {
    let (key, calves) = state.scene_key();
    scenes()
        .iter()
        .filter(|s| s.state == key && s.calves == calves)
        .min_by_key(|s| s.size.abs_diff(size))
}

/// Packed Braille cells, row-major; `0` is an empty cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<u8>,
}

impl Grid {
    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> u8 {
        self.cells[usize::from(row) * usize::from(self.cols) + usize::from(col)]
    }

    /// The cell as text: `U+2800 + bits`, or a space.
    #[must_use]
    pub fn char_at(&self, col: u16, row: u16) -> char {
        match self.cell(col, row) {
            0 => ' ',
            bits => char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' '),
        }
    }

    /// Rows of text, joined by newlines, as the kit's stills are written.
    #[must_use]
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|r| {
                (0..self.cols)
                    .map(|c| self.char_at(c, r))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Flatten one contour into straight edges, exactly as `braille.js` does:
/// ten segments per cubic, then close back to the start.
fn edges(contour: &Contour, out: &mut Vec<[f64; 4]>) {
    let mut pen = contour.start;
    for c in &contour.cubics {
        let p = pen;
        for i in 1..=10 {
            let t = f64::from(i) / 10.0;
            let u = 1.0 - t;
            let q = [
                u * u * u * p[0]
                    + 3.0 * u * u * t * c[0]
                    + 3.0 * u * t * t * c[2]
                    + t * t * t * c[4],
                u * u * u * p[1]
                    + 3.0 * u * u * t * c[1]
                    + 3.0 * u * t * t * c[3]
                    + t * t * t * c[5],
            ];
            out.push([pen[0], pen[1], q[0], q[1]]);
            pen = q;
        }
    }
    out.push([pen[0], pen[1], contour.start[0], contour.start[1]]);
}

/// Rasterize a scene into `cols` x `rows` Braille cells. `cell_aspect` is a
/// cell's width over its height (0.5 for a typical monospace font).
#[must_use]
pub fn rasterize(scene: &Scene, cols: u16, rows: u16, cell_aspect: f64) -> Grid {
    let width = usize::from(cols) * 2;
    let height = usize::from(rows) * 4;
    let (w, h) = (width as f64, height as f64);
    let mut dots = vec![false; width * height];
    let scale = (w * cell_aspect * 2.0).min(h) / 124.0;
    let sx = scale / (cell_aspect * 2.0);
    let sy = scale;
    let mut shape_edges = Vec::new();
    let mut cross = Vec::new();
    for shape in &scene.shapes {
        shape_edges.clear();
        for contour in &shape.contours {
            edges(contour, &mut shape_edges);
        }
        for e in &mut shape_edges {
            *e = [
                w / 2.0 + e[0] * sx,
                h / 2.0 + e[1] * sy,
                w / 2.0 + e[2] * sx,
                h / 2.0 + e[3] * sy,
            ];
        }
        for y in 0..height {
            let cy = y as f64 + 0.5;
            cross.clear();
            for &[x1, y1, x2, y2] in &shape_edges {
                if (y1 > cy) != (y2 > cy) {
                    cross.push(x1 + (cy - y1) * (x2 - x1) / (y2 - y1));
                }
            }
            cross.sort_by(f64::total_cmp);
            for pair in cross.as_chunks::<2>().0 {
                let left = (pair[0] - 0.5).ceil().max(0.0);
                let right = (pair[1] - 0.5).ceil().min(w);
                if right <= left {
                    continue;
                }
                for x in left as usize..right as usize {
                    dots[y * width + x] = true;
                }
            }
        }
    }
    let mut cells = vec![0u8; usize::from(cols) * usize::from(rows)];
    for y in 0..height {
        for x in 0..width {
            if dots[y * width + x] {
                cells[(y / 4) * usize::from(cols) + x / 2] |= BITS[y % 4][x % 2];
            }
        }
    }
    Grid { cols, rows, cells }
}

/// The whale for `state` in a `cols` x `rows` viewport, as the kit's `frame`
/// sizes it, or `None` below 16x8 cells, where only the words fit.
#[must_use]
pub fn frame(state: WhaleState, cols: u16, rows: u16) -> Option<Grid> {
    if cols < 16 || rows < 8 {
        return None;
    }
    let size = (u32::from(cols) * 2).min(u32::from(rows) * 4);
    Some(rasterize(scene(state, size)?, cols, rows, 0.5))
}

/// The whale widget: the art, centred, with its words on the row below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whale {
    pub state: WhaleState,
    /// Localized words; `None` uses [`WhaleState::words`].
    pub words: Option<Cow<'static, str>>,
}

impl Whale {
    #[must_use]
    pub fn new(state: WhaleState) -> Self {
        Self { state, words: None }
    }

    #[must_use]
    pub fn words(mut self, words: impl Into<Cow<'static, str>>) -> Self {
        self.words = Some(words.into());
        self
    }

    /// Paint packed Braille cells evaluated by the host's shared Director.
    ///
    /// `grid.cells` must contain exactly `grid.cols * grid.rows` row-major
    /// bytes. The complete grid, at least 16×8 cells, must fit the intersection
    /// of `area` and the buffer with one additional row for the state words.
    /// Invalid or non-fitting grids and ASCII-safe terminals show only those
    /// words; a supplied frame is never resized or replaced with a poster.
    ///
    /// The host owns timing, reduced motion and hidden-surface scheduling.
    /// It can call [`Paint::paint`] for the static poster instead. Zero bits
    /// are transparent, preserving the existing ground: repaint the surface
    /// before each frame, as with other ratatui components. No grid is copied
    /// or allocated by this method.
    pub fn paint_frame(&self, area: Rect, buf: &mut Buffer, theme: &Theme, grid: &Grid) {
        self.paint_grid(area, buf, theme, Some(grid));
    }

    /// The largest standard viewport (32x16, 20x10, 16x8) that fits.
    fn viewport(area: Rect) -> Option<(u16, u16)> {
        let rows = area.height.saturating_sub(1);
        [(32, 16), (20, 10), (16, 8)]
            .into_iter()
            .find(|&(c, r)| area.width >= c && rows >= r)
    }

    /// Ink for one row of art: the logo ombre in truecolor, `Primary`
    /// elsewhere, the terminal's own ink without color.
    fn ink(theme: &Theme, row: u16, rows: u16) -> Style {
        if !theme.paints_grounds() {
            return theme.fg(Role::Primary);
        }
        let (top, bottom) = match theme.caps().appearance {
            crate::detect::Appearance::Light => (rgb(LOGO_TOP), rgb(LOGO_BOTTOM)),
            _ => (theme.token(Role::Primary), rgb(LOGO_TOP)),
        };
        let t = f32::from(row) / f32::from(rows.saturating_sub(1).max(1));
        let c = blend(bottom, top, t);
        match (theme.depth(), c) {
            (ColorDepth::TrueColor, c) => Style::default().fg(c),
            (_, Color::Rgb(r, g, b)) => {
                Style::default().fg(Color::Indexed(rgb_to_ansi256(r, g, b)))
            }
            _ => theme.fg(Role::Primary),
        }
    }

    fn paint_label(label: Line<'_>, area: Rect, buf: &mut Buffer) {
        if area.right() != u16::MAX || label.width() <= usize::from(area.width) {
            label.render(area, buf);
            return;
        }
        // ratatui 0.30 saturates a truncated Span's cursor at u16::MAX,
        // then tries to write that exclusive edge. Local coordinates preserve
        // its ordinary centered clipping without reaching the invalid index.
        let mut row = Buffer::empty(Rect::new(0, 0, area.width, 1));
        for x in 0..area.width {
            row[(x, 0)] = buf[(area.x + x, area.y)].clone();
        }
        label.render(row.area, &mut row);
        for x in 0..area.width {
            buf[(area.x + x, area.y)] = row[(x, 0)].clone();
        }
    }

    fn paint_grid(&self, area: Rect, buf: &mut Buffer, theme: &Theme, grid: Option<&Grid>) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let mark = StatusMark::new(self.state.state())
            .word(self.words.clone().unwrap_or_else(|| self.state.words()));
        let label = if self.words.as_deref() == Some("") {
            Line::default()
        } else {
            Line::from(mark.spans(theme)).centered()
        };
        // Braille has no honest ASCII form: dot-count shading turns the whale
        // into noise. ASCII-safe terminals get the words alone.
        let grid = grid.filter(|grid| {
            !theme.ascii()
                && grid.cols >= 16
                && grid.rows >= 8
                && grid.cols <= area.width
                && grid.rows < area.height
                && usize::from(grid.cols).checked_mul(usize::from(grid.rows))
                    == Some(grid.cells.len())
        });
        let Some(grid) = grid else {
            Self::paint_label(label, Rect { height: 1, ..area }, buf);
            return;
        };
        let (cols, rows) = (grid.cols, grid.rows);
        let x0 = area.x + (area.width - cols) / 2;
        let y0 = area.y + (area.height - rows - 1) / 2;
        crate::components::paint_braille_cells(
            Rect::new(x0, y0, cols, rows),
            buf,
            &grid.cells,
            |row| Self::ink(theme, row, rows),
        );
        Self::paint_label(
            label,
            Rect {
                y: y0 + rows,
                height: 1,
                ..area
            },
            buf,
        );
    }
}

impl Paint for Whale {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        let grid = if theme.ascii() {
            None
        } else {
            Self::viewport(area).and_then(|(cols, rows)| frame(self.state, cols, rows))
        };
        self.paint_grid(area, buf, theme, grid.as_ref());
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        17
    }
}
