//! Shared list-selection navigation (#4755, #6290).
//!
//! Modal lists and config screens should wrap at the ends so Down on the last
//! row returns to the top and Up on the first row returns to the bottom.
//! Centralizing the arithmetic keeps that behavior consistent without each
//! picker inventing its own clamp.
//!
//! [`Motion`] extends that from the arithmetic to the *vocabulary*. `menu_style`
//! single-sources how a selected row looks; this single-sources what a key
//! means, which is the half that was still being reinvented per view: `h`/`l`
//! in the provider picker against `Left`/`Right` in the model picker one screen
//! later, `End` in exactly one of seven pickers, and no paging at all in
//! `fleet_detail` (#6290 has the tables).
//!
//! Two entry points, because the difference is load-bearing:
//! [`motion`] for a surface with no focused text input, and
//! [`motion_while_typing`] for one that is capturing characters. The letter
//! aliases exist only in the first. A picker with a live filter that routed
//! `j` to "move down" would eat the letter out of the user's query, so the
//! typing-safe set is arrow-and-page only. Surfaces choose by what they are
//! doing, not by what they are.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Move a 0-based selection by `delta`, wrapping at both ends.
///
/// Empty lists leave the selection at `0`. A zero `len` is treated as empty.
#[must_use]
pub fn wrap_index(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as isize + delta).rem_euclid(len as isize) as usize
}

/// One movement a selection surface can be asked to make.
///
/// Horizontal motions are named for what they do — move between panes,
/// columns or tab strips — rather than for a key, so a surface that has no
/// second axis simply never asks for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One row toward the start.
    Prev,
    /// One row toward the end.
    Next,
    /// One screenful toward the start.
    PagePrev,
    /// One screenful toward the end.
    PageNext,
    /// The first row.
    First,
    /// The last row.
    Last,
    /// The previous pane, column or tab.
    RegionPrev,
    /// The next pane, column or tab.
    RegionNext,
}

/// The motion a key asks for on a surface with **no focused text input**.
///
/// `j`/`k` and `h`/`l` are aliases here and nowhere else; see the module doc.
/// A key carrying CONTROL or ALT is never a motion — those belong to the
/// surface's own shortcuts.
#[must_use]
pub fn motion(key: &KeyEvent) -> Option<Motion> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char('k') => Some(Motion::Prev),
        KeyCode::Char('j') => Some(Motion::Next),
        KeyCode::Char('h') => Some(Motion::RegionPrev),
        KeyCode::Char('l') => Some(Motion::RegionNext),
        _ => motion_while_typing(key),
    }
}

/// The motion a key asks for while the surface is **capturing typed text**.
///
/// Arrows, paging and Home/End only, so no letter is ever taken out of a
/// query. Tab still moves between regions: it is not a character a filter
/// wants.
#[must_use]
pub fn motion_while_typing(key: &KeyEvent) -> Option<Motion> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(Motion::Prev),
        KeyCode::Down => Some(Motion::Next),
        KeyCode::PageUp => Some(Motion::PagePrev),
        KeyCode::PageDown => Some(Motion::PageNext),
        KeyCode::Home => Some(Motion::First),
        KeyCode::End => Some(Motion::Last),
        KeyCode::Left => Some(Motion::RegionPrev),
        KeyCode::Right => Some(Motion::RegionNext),
        KeyCode::BackTab => Some(Motion::RegionPrev),
        // Some terminals report shift+tab as Tab carrying SHIFT rather than
        // as BackTab; both are the same "previous region" motion.
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => Some(Motion::RegionPrev),
        KeyCode::Tab => Some(Motion::RegionNext),
        _ => None,
    }
}

/// Apply a vertical [`Motion`] to a 0-based selection.
///
/// `Prev`/`Next` wrap, matching [`wrap_index`]. Paging and `First`/`Last`
/// clamp: a user pressing PageDown is asking to travel, not to teleport to the
/// top. Returns `None` for a horizontal motion, which only the surface can
/// resolve.
#[must_use]
pub fn apply(selected: usize, len: usize, page: usize, motion: Motion) -> Option<usize> {
    if len == 0 {
        return Some(0);
    }
    let last = len - 1;
    let page = page.max(1) as isize;
    Some(match motion {
        Motion::Prev => wrap_index(selected, len, -1),
        Motion::Next => wrap_index(selected, len, 1),
        Motion::PagePrev => (selected as isize - page).max(0) as usize,
        Motion::PageNext => (selected as isize + page).min(last as isize) as usize,
        Motion::First => 0,
        Motion::Last => last,
        Motion::RegionPrev | Motion::RegionNext => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Motion, apply, motion, motion_while_typing, wrap_index};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The reason there are two entry points. A picker with a live filter that
    /// routed `j` to "move down" would silently eat the letter out of the
    /// user's query (#6290).
    #[test]
    fn typing_safe_motions_never_claim_a_letter() {
        for c in ['j', 'k', 'h', 'l', 'g', 'q'] {
            assert_eq!(
                motion_while_typing(&key(KeyCode::Char(c))),
                None,
                "`{c}` must stay available to a text filter"
            );
        }
        assert_eq!(motion_while_typing(&key(KeyCode::Down)), Some(Motion::Next));
        assert_eq!(motion_while_typing(&key(KeyCode::End)), Some(Motion::Last));
    }

    #[test]
    fn letter_aliases_exist_only_where_nothing_is_being_typed() {
        assert_eq!(motion(&key(KeyCode::Char('j'))), Some(Motion::Next));
        assert_eq!(motion(&key(KeyCode::Char('k'))), Some(Motion::Prev));
        assert_eq!(motion(&key(KeyCode::Char('h'))), Some(Motion::RegionPrev));
        assert_eq!(motion(&key(KeyCode::Char('l'))), Some(Motion::RegionNext));
    }

    /// `h`/`l` and `Left`/`Right` were two idioms for one motion in two pickers
    /// one screen apart. They resolve to the same `Motion` now.
    #[test]
    fn the_two_horizontal_idioms_agree() {
        assert_eq!(
            motion(&key(KeyCode::Char('l'))),
            motion(&key(KeyCode::Right))
        );
        assert_eq!(
            motion(&key(KeyCode::Char('h'))),
            motion(&key(KeyCode::Left))
        );
        assert_eq!(motion(&key(KeyCode::Tab)), Some(Motion::RegionNext));
    }

    /// A surface's own shortcuts keep their keys: Ctrl/Alt is never a motion.
    #[test]
    fn modified_keys_are_not_motions() {
        let ctrl_down = KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL);
        assert_eq!(motion(&ctrl_down), None);
        assert_eq!(motion_while_typing(&ctrl_down), None);
    }

    /// Rows wrap; pages clamp. PageDown is a request to travel, not to
    /// teleport back to the top.
    #[test]
    fn rows_wrap_and_pages_clamp() {
        assert_eq!(apply(2, 3, 10, Motion::Next), Some(0));
        assert_eq!(apply(0, 3, 10, Motion::Prev), Some(2));
        assert_eq!(apply(1, 40, 10, Motion::PageNext), Some(11));
        assert_eq!(apply(38, 40, 10, Motion::PageNext), Some(39));
        assert_eq!(apply(3, 40, 10, Motion::PagePrev), Some(0));
        assert_eq!(apply(7, 40, 10, Motion::Last), Some(39));
    }

    #[test]
    fn horizontal_motion_is_the_surfaces_to_resolve() {
        assert_eq!(apply(1, 5, 10, Motion::RegionNext), None);
        assert_eq!(apply(1, 5, 10, Motion::RegionPrev), None);
    }

    #[test]
    fn an_empty_list_has_no_selection_to_move() {
        assert_eq!(apply(0, 0, 10, Motion::Next), Some(0));
        assert_eq!(apply(0, 0, 10, Motion::Last), Some(0));
    }

    #[test]
    fn wraps_forward_and_backward() {
        assert_eq!(wrap_index(0, 3, -1), 2);
        assert_eq!(wrap_index(2, 3, 1), 0);
        assert_eq!(wrap_index(1, 3, 1), 2);
        assert_eq!(wrap_index(1, 3, -1), 0);
    }

    #[test]
    fn empty_list_stays_at_zero() {
        assert_eq!(wrap_index(5, 0, 1), 0);
        assert_eq!(wrap_index(0, 0, -1), 0);
    }
}
