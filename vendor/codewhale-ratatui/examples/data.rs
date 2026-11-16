//! Live data displays: `cargo run --example data`.
//!
//! A sparkline takes a new value every 400 ms, the trend draws in, and the
//! bars glide to new values every three seconds. r replays, m switches
//! between full and reduced motion, q or Escape exits.
//!
//! `--json FILE` shows one agent-sent [`DataDisplay`] instead.
//! `--text` prints the final frame and each display's plain description.
//! `--svg DIR` writes frames at fixed draw-in points.

use std::{
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use codewhale_ratatui::{
    DataBarItem, DataDisplay, DataWords, MotionEasing, MotionMode, MotionPolicy, MotionStep,
    MotionTiming, Paint, Role, Theme,
    testing::{self, Profile},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, buffer::Buffer, layout::Rect};

const TICK: Duration = Duration::from_millis(400);
const RESHUFFLE: Duration = Duration::from_secs(3);
const DRAW_IN: MotionTiming =
    MotionTiming::space(Duration::from_millis(1_400), MotionEasing::EaseOut);
const GLIDE: MotionTiming = MotionTiming::PANEL;

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (self.0 >> 33) as f64 / f64::from(1u32 << 31)
    }
}

struct Board {
    noise: Noise,
    requests: Vec<f64>,
    latency: Vec<f64>,
    bars: DataDisplay,
    bars_before: DataDisplay,
    draw: MotionStep,
    glide: MotionStep,
    agent: Option<DataDisplay>,
}

fn crate_bars(noise: &mut Noise) -> DataDisplay {
    DataDisplay::Bars {
        label: "Tests by crate".into(),
        unit: "tests".into(),
        items: ["engine", "tui", "runtime", "protocol", "config"]
            .into_iter()
            .map(|name| DataBarItem::new(name, (40.0 + noise.next() * 400.0).round()))
            .collect(),
    }
}

impl Board {
    fn new(agent: Option<DataDisplay>) -> Self {
        let mut noise = Noise(7);
        let latency = (0..96)
            .map(|i| 80.0 + 45.0 * (f64::from(i) / 9.0).sin() + noise.next() * 30.0)
            .collect();
        let requests = (0..40).map(|_| 20.0 + noise.next() * 60.0).collect();
        let bars = crate_bars(&mut noise);
        Self {
            noise,
            requests,
            latency,
            bars_before: DataDisplay::Bars {
                label: String::new(),
                unit: String::new(),
                items: Vec::new(),
            },
            bars,
            draw: MotionStep::at_rest(0.0),
            glide: MotionStep::at_rest(1.0),
            agent,
        }
    }

    fn replay(&mut self, now: Instant, policy: MotionPolicy) {
        self.draw = MotionStep::at_rest(0.0);
        self.draw.go(1.0, now, DRAW_IN, policy);
    }

    fn tick(&mut self) {
        let last = self.requests.last().copied().unwrap_or(50.0);
        let next = (last + (self.noise.next() - 0.5) * 30.0).clamp(5.0, 120.0);
        self.requests.push(next.round());
        if self.requests.len() > 200 {
            self.requests.remove(0);
        }
    }

    fn reshuffle(&mut self, now: Instant, policy: MotionPolicy) {
        self.bars_before = std::mem::replace(&mut self.bars, crate_bars(&mut self.noise));
        self.glide = MotionStep::at_rest(0.0);
        self.glide.go(1.0, now, GLIDE, policy);
    }

    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme, draw: f32, glide: f32) {
        buf.set_style(area, theme.bg(Role::Background));
        let inner = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(2),
        );
        if let Some(agent) = &self.agent {
            agent.view().drawn(draw).paint(inner, buf, theme);
            return;
        }
        let row =
            |y: u16, h: u16| Rect::new(inner.x, inner.y + y, inner.width, h).intersection(inner);
        let requests = DataDisplay::Sparkline {
            label: "Requests".into(),
            unit: "rpm".into(),
            values: self.requests.clone(),
        };
        let latency = DataDisplay::Trend {
            label: "Latency".into(),
            unit: "ms".into(),
            values: self.latency.clone(),
            rows: Some(9),
        };
        requests.view().drawn(draw).paint(row(0, 1), buf, theme);
        latency.view().drawn(draw).paint(row(2, 9), buf, theme);
        self.bars
            .view()
            .from(&self.bars_before)
            .drawn(glide.min(draw))
            .paint(row(12, 6), buf, theme);
    }

    fn displays(&self) -> Vec<DataDisplay> {
        if let Some(agent) = &self.agent {
            return vec![agent.clone()];
        }
        vec![
            DataDisplay::Sparkline {
                label: "Requests".into(),
                unit: "rpm".into(),
                values: self.requests.clone(),
            },
            DataDisplay::Trend {
                label: "Latency".into(),
                unit: "ms".into(),
                values: self.latency.clone(),
                rows: Some(9),
            },
            self.bars.clone(),
        ]
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let agent = match value("--json") {
        Some(path) => {
            let json = std::fs::read_to_string(&path)?;
            match DataDisplay::from_json(&json) {
                Ok(shown) => Some(shown),
                Err(e) => {
                    eprintln!("{path}: {e}");
                    std::process::exit(2);
                }
            }
        }
        None => None,
    };
    let board = Board::new(agent);

    if args.iter().any(|a| a == "--text") {
        let buf = testing::render(84, 20, |area, buf| {
            board.paint(area, buf, &Profile::DarkTrue.theme(), 1.0, 1.0);
        });
        println!("{}", testing::text(&buf));
        for shown in board.displays() {
            println!("{}", shown.describe(&DataWords::default()));
        }
        return Ok(());
    }
    if let Some(dir) = value("--svg") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir)?;
        let theme = Profile::DarkTrue.theme();
        for (i, at) in [0.2_f32, 0.5, 0.8, 1.0].into_iter().enumerate() {
            let buf = testing::render(84, 20, |area, buf| board.paint(area, buf, &theme, at, at));
            let path = dir.join(format!("data-{i}.svg"));
            std::fs::write(&path, testing::svg(&buf, &theme))?;
            println!("{}", path.display());
        }
        return Ok(());
    }
    run(board)
}

fn run(mut board: Board) -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let theme = Theme::detect().tui();
    let mut mode = MotionMode::Full;
    let mut policy = MotionPolicy::new(mode, &theme);
    let start = Instant::now();
    board.replay(start, policy);
    let (mut next_tick, mut next_shuffle) = (start + TICK, start + RESHUFFLE);

    let result = loop {
        let now = Instant::now();
        if now >= next_tick {
            board.tick();
            next_tick = now + TICK;
        }
        if now >= next_shuffle {
            board.reshuffle(now, policy);
            next_shuffle = now + RESHUFFLE;
        }
        let (draw, glide) = (
            board.draw.drawn(now, policy),
            board.glide.drawn(now, policy),
        );
        terminal.draw(|f| board.paint(f.area(), f.buffer_mut(), &theme, draw, glide))?;

        let due = [
            board.draw.next_frame_at(now, policy),
            board.glide.next_frame_at(now, policy),
        ]
        .into_iter()
        .flatten()
        .chain([next_tick, next_shuffle])
        .min()
        .unwrap_or(next_tick);
        if event::poll(due.saturating_duration_since(Instant::now()))?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                KeyCode::Char('r') => board.replay(Instant::now(), policy),
                KeyCode::Char('m') => {
                    mode = if mode == MotionMode::Full {
                        MotionMode::Reduced
                    } else {
                        MotionMode::Full
                    };
                    policy = MotionPolicy::new(mode, &theme);
                }
                _ => {}
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}
