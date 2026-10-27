//! A small native Codewhale app: edit, paste and echo a line with Enter.
//! The application owns input, data and exit; components only paint.
use std::{io, time::Duration};

use codewhale_ratatui::{
    Message, MotionMode, NativeComposer, OceanColumn, Paint, PostureBar, TerminalShell,
    TextInputOutcome, TextInputState, Theme, TuiGround, Workbar, WorkbarPanel, WorkbarRow,
};
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyModifiers},
    execute,
};

struct PasteMode;
impl Drop for PasteMode {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste);
    }
}

fn main() -> io::Result<()> {
    ratatui::run(|terminal| {
        execute!(io::stdout(), EnableBracketedPaste)?;
        let _paste_mode = PasteMode;
        let theme = Theme::detect().tui();
        let mut draft = TextInputState::new();
        let mut message = String::from("Type a line below. Enter echoes it here; Esc closes.");
        let workbar = Workbar::new(
            WorkbarPanel::Tasks,
            vec![WorkbarRow::new("echo", "Edit and submit your first message").mark("●")],
        );

        loop {
            terminal.draw(|frame| {
                let area = frame.area();
                let composer = NativeComposer::new(draft.buffer().text())
                    .focused(true)
                    .cursor(draft.buffer().cursor())
                    .can_submit(!draft.buffer().is_empty())
                    .placeholder("Write something…");
                let shell =
                    TerminalShell::new(composer.desired_height(area.width, area.height / 2))
                        .workbar_rows(workbar.height(area.width, &theme));
                let regions = shell.areas(area);
                let cursor = composer.cursor_position(regions.composer);
                let buf = frame.buffer_mut();
                shell.paint(area, buf, &theme);
                Message::native(&message).paint(regions.conversation, buf, &theme);
                composer.paint(regions.composer, buf, &theme);
                PostureBar::new("ask").paint(regions.posture, buf, &theme);
                workbar.paint(regions.workbar, buf, &theme);
                // A still column keeps the native depth without an idle redraw loop.
                let water = OceanColumn::new(Duration::ZERO, MotionMode::Still).viewport(area);
                water.apply(area, buf, &theme);
                if let Some(ground) = theme.tui_ground(TuiGround::Composer).bg {
                    water.apply_matching(regions.composer, buf, &theme, ground);
                }
                if let Some(cursor) = cursor {
                    frame.set_cursor_position(cursor);
                }
            })?;

            match event::read()? {
                Event::Key(key) => {
                    if key.is_press()
                        && key.code == KeyCode::Char('c')
                        && key.modifiers == KeyModifiers::CONTROL
                    {
                        break;
                    }
                    match draft.handle_key(key) {
                        TextInputOutcome::Submitted if !draft.buffer().is_empty() => {
                            message = draft.buffer().text().to_owned();
                            draft.clear();
                        }
                        TextInputOutcome::Cancelled => break,
                        _ => {}
                    }
                }
                Event::Paste(text) => {
                    draft.paste(&text);
                }
                Event::Resize(..) => {}
                _ => {}
            }
        }
        Ok(())
    })
}
