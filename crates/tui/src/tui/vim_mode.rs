//! Composer vim Normal-mode keybindings.

use crate::tui::app::App;

/// Handle a plain character key press when the composer is in vim Normal mode.
///
/// Implements the core set of normal-mode bindings:
/// - `h` / `l`  — left / right by character
/// - `j` / `k`  — down / up by logical line (falls back to prev/next history)
/// - `w` / `b`  — word forward / backward
/// - `0` / `$`  — line start / end
/// - `x`        — delete character under cursor
/// - `d` (×2)   — delete current line (`dd`)
/// - `i`        — enter Insert before cursor
/// - `a`        — enter Insert after cursor
/// - `o`        — open new line below and enter Insert
/// - `G`        — move to end of buffer
pub(super) fn handle_vim_normal_key(app: &mut App, c: char) {
    // Handle pending `d` (waiting for second `d` to complete `dd`).
    if app.composer.vim_pending_d {
        app.composer.vim_pending_d = false;
        if c == 'd' {
            app.vim_delete_line();
        }
        // Any other key cancels the pending operator.
        return;
    }

    match c {
        'h' => app.move_cursor_left(),
        'l' => app.move_cursor_right(),
        'j' => app.vim_move_down(),
        'k' => app.vim_move_up(),
        'w' => app.vim_move_word_forward(),
        'b' => app.vim_move_word_backward(),
        '0' => app.vim_move_line_start(),
        '$' => app.vim_move_line_end(),
        'x' => app.vim_delete_char_under_cursor(),
        'd' => {
            // Start the `dd` operator sequence.
            app.composer.vim_pending_d = true;
        }
        'i' => app.vim_enter_insert(),
        'a' => app.vim_enter_append(),
        'o' => app.vim_open_line_below(),
        'G' => app.move_cursor_end(),
        _ => {
            // Unknown normal-mode key — silently ignored in Normal mode.
        }
    }
}
