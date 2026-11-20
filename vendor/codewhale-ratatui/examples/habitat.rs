//! A live marine specimen: `cargo run --example habitat`.
//! The example supplies the clock; the kit neither schedules nor simulates
//! Engine activity. `p` changes profile, `m` changes motion, `q` exits.
//! `--frames DIR` exports 80 deterministic actual-buffer SVG frames.
use codewhale_ratatui::{
    Habitat, HabitatDensity, KeyHint, KeyHints, Message, MotionMode, Paint, PaneHeader, Role,
    Theme, Whale, WhaleState, WorkspaceFrame,
    testing::{self, Profile},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, buffer::Buffer, layout::Rect};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

fn specimen(area: Rect, buf: &mut Buffer, theme: &Theme, elapsed: Duration, motion: MotionMode) {
    let area = area.intersection(buf.area);
    let frame = WorkspaceFrame::new("Habitat").mode("Marine specimen").side_width(0).footer("Demonstration / p profile / m motion / q close / host supplies the clock and session facts");
    frame.paint(area, buf, theme);
    let main = frame.areas(area).main;
    PaneHeader::new("Life in the water")
        .meta(if motion.animates() {
            "Full motion"
        } else {
            "Ambient motion off"
        })
        .paint(
            Rect {
                height: main.height.min(2),
                ..main
            },
            buf,
            theme,
        );
    let prose = Rect::new(
        main.x,
        main.y.saturating_add(3),
        main.width.min(58),
        main.height.saturating_sub(3).min(5),
    );
    Message::new("Codewhale", "A fish school, a far shoal, a passing jellyfish,\nsnow, bubbles and a distant whale. The water gives way to words.").paint(prose, buf, theme);
    if main.height >= 20 && main.width >= 30 {
        let width = main.width.min(30);
        let whale = Rect::new(
            main.x.saturating_add((main.width - width) / 2),
            main.y.saturating_add(9),
            width,
            11,
        );
        Whale::new(WhaleState::Rest)
            .words("Whale companion / demo")
            .paint(whale, buf, theme);
    }
    let hints = KeyHints::new(vec![
        KeyHint::new("p", "profile"),
        KeyHint::new("m", "motion"),
        KeyHint::new("q", "close"),
    ]);
    if main.height > 3 {
        hints.paint(
            Rect::new(main.x, main.bottom().saturating_sub(2), main.width, 2),
            buf,
            theme,
        );
    }
    // Paint after foreground content so open-water collision protects it.
    Habitat::new(elapsed, motion)
        .density(HabitatDensity::Rich)
        .hold_jellyfish_visit(true)
        .paint(main, buf, theme);
}

fn frames(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let theme = Profile::DarkTrue.theme();
    let mut manifest = Vec::new();
    for index in 0..80 {
        let elapsed_ms = 6_000 + index * 100;
        let buf = testing::render(112, 30, |area, buf| {
            buf.set_style(area, theme.bg(Role::Background));
            specimen(
                area,
                buf,
                &theme,
                Duration::from_millis(elapsed_ms),
                MotionMode::Full,
            );
        });
        let name = format!("frame-{index:03}.svg");
        std::fs::write(dir.join(&name), testing::svg(&buf, &theme))?;
        manifest.push(serde_json::json!({"file":name, "elapsed_ms":elapsed_ms, "profile":"dark-truecolor", "width":112, "height":30}));
    }
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--frames") {
        return frames(Path::new(
            args.get(1).map_or("target/habitat-frames", String::as_str),
        ));
    }
    if !args.is_empty() {
        return Err(io::Error::other("usage: habitat [--frames DIR]"));
    }
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let start = Instant::now();
    let mut profile = 0;
    let mut motion = MotionMode::Full;
    loop {
        let theme = Profile::ALL[profile].theme();
        terminal.draw(|frame| {
            let area = frame.area();
            specimen(
                area,
                frame.buffer_mut(),
                &theme,
                start.elapsed().saturating_add(Duration::from_secs(6)),
                motion,
            );
        })?;
        // The example's event loop owns frame scheduling. A quiet mode can
        // block on input instead of repeatedly painting identical frames.
        if motion.animates() && !event::poll(Duration::from_millis(50))? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('p') => profile = (profile + 1) % Profile::ALL.len(),
                KeyCode::Char('m') => {
                    motion = match motion {
                        MotionMode::Full => MotionMode::Reduced,
                        MotionMode::Reduced => MotionMode::Still,
                        MotionMode::Still => MotionMode::Full,
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}
