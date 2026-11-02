//! Composer-owned state and behavior.
//!
//! Text editing, cursor movement, selection, paste/scrub handling, input
//! history navigation, and vim modal editing for the composer live here.
//! Methods that only touch composer fields are inherent methods on
//! [`ComposerState`]; methods that also need `App`-level state (redraw
//! flags, status messages, clipboard, workspace) stay on `App` in an
//! extension impl in this module. The `Deref<Target = ComposerState>`
//! bridge on `App` is unchanged.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerHistorySearch {
    pre_search_input: String,
    pre_search_cursor: usize,
    query: String,
    selected: usize,
}

impl ComposerHistorySearch {
    fn new(pre_search_input: String, pre_search_cursor: usize) -> Self {
        Self {
            pre_search_input,
            pre_search_cursor,
            query: String::new(),
            selected: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputHistoryDraft {
    input: String,
    cursor: usize,
}

pub(crate) fn char_count(text: &str) -> usize {
    text.chars().count()
}

pub(crate) fn byte_index_at_char(text: &str, char_index: usize) -> usize {
    if char_index == 0 {
        return 0;
    }
    text.char_indices()
        .nth(char_index)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| text.len())
}

/// Remove the chars in `[start_char, end_char)`. Returns true when anything
/// was removed. Range endpoints are clamped to the text length.
fn remove_char_range(text: &mut String, start_char: usize, end_char: usize) -> bool {
    if start_char >= end_char {
        return false;
    }
    let start = byte_index_at_char(text, start_char);
    let end = byte_index_at_char(text, end_char);
    if start >= end {
        return false;
    }
    text.replace_range(start..end, "");
    true
}

/// Char index of the grapheme-cluster boundary at or before `char_index - 1` —
/// i.e. where the cursor lands after one "left" step. Grapheme-aware so a
/// single step never splits a CJK char + combining mark, emoji ZWJ sequence,
/// or flag pair. Returns `0` when `char_index` is `0`.
pub(crate) fn prev_grapheme_boundary(text: &str, char_index: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut acc = 0usize;
    for g in text.graphemes(true) {
        let next = acc + g.chars().count();
        if next >= char_index {
            return acc;
        }
        acc = next;
    }
    acc
}

/// Char index of the grapheme-cluster boundary at or before `char_index`:
/// the start of the cluster that contains it. Vim column moves land here so a
/// line change never parks the cursor inside a ZWJ or combining sequence.
pub(crate) fn floor_grapheme_boundary(text: &str, char_index: usize) -> usize {
    prev_grapheme_boundary(text, char_index.saturating_add(1)).min(char_index)
}

/// Char index of the first grapheme-cluster boundary strictly after
/// `char_index` — i.e. where the cursor lands after one "right" step.
/// Returns the total char count when already at or past the end.
pub(crate) fn next_grapheme_boundary(text: &str, char_index: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut acc = 0usize;
    for g in text.graphemes(true) {
        let next = acc + g.chars().count();
        if next > char_index {
            return next;
        }
        acc = next;
    }
    acc
}

fn normalize_paste_text(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

fn strip_raw_mouse_report_runs(input: &str, cursor: usize) -> Option<(String, usize)> {
    // First pass: strip the well-defined control-sequence fragment
    // shapes that crossterm sometimes hands us as `Char(c)` keystrokes
    // when its event reader is interrupted mid-sequence during dense
    // streaming output (#1915). This covers OSC 8 hyperlink fragments
    // (`]8;;URL`, including the closing `]8;;`) and Kitty keyboard
    // protocol fragments (`[?…u`, `[>…u`, `[?u`).
    let (after_fragments, after_fragments_cursor, fragments_changed) =
        strip_control_sequence_fragments(input, cursor);

    // Second pass: the existing run-based filter handles SGR mouse
    // reports (`[<35;44;18M`) and the multi-terminator burst shape
    // (`5;46;18M;48;18M`) introduced in e63a4ba4a. It operates on a
    // narrow char set so it can't be confused with user-typed text.
    let chars: Vec<char> = after_fragments.chars().collect();
    let mut output = String::with_capacity(after_fragments.len());
    let mut new_cursor = 0usize;
    let mut changed = fragments_changed;
    let mut index = 0usize;

    while index < chars.len() {
        if is_raw_mouse_report_run_char(chars[index]) {
            let start = index;
            while index < chars.len() && is_raw_mouse_report_run_char(chars[index]) {
                index += 1;
            }
            let run = &chars[start..index];
            if let Some(keep) = raw_mouse_report_keep_mask(run) {
                changed = true;
                for (offset, ch) in run.iter().copied().enumerate() {
                    if !keep[offset] {
                        continue;
                    }
                    if start + offset < cursor {
                        new_cursor += 1;
                    }
                    output.push(ch);
                }
                continue;
            }
            for (offset, ch) in run.iter().copied().enumerate() {
                if start + offset < after_fragments_cursor {
                    new_cursor += 1;
                }
                output.push(ch);
            }
            continue;
        }

        if index < after_fragments_cursor {
            new_cursor += 1;
        }
        output.push(chars[index]);
        index += 1;
    }

    changed.then(|| {
        let cursor = new_cursor.min(char_count(&output));
        (output, cursor)
    })
}

fn is_raw_mouse_report_run_char(ch: char) -> bool {
    matches!(ch, '\x1b' | '[' | '<' | ';' | ':' | 'M' | 'm') || ch.is_ascii_digit()
}

fn looks_like_raw_mouse_report_run(run: &[char]) -> bool {
    if run.len() < 5 {
        return false;
    }
    let has_separator = run.iter().any(|ch| matches!(ch, ';' | ':'));
    let terminators = run.iter().filter(|ch| matches!(ch, 'M' | 'm')).count();
    if !has_separator || terminators == 0 {
        return false;
    }
    has_sgr_mouse_marker(run) || terminators >= 2
}

fn has_sgr_mouse_marker(run: &[char]) -> bool {
    run.windows(2).any(|window| window == ['[', '<'])
}

fn raw_mouse_report_keep_mask(run: &[char]) -> Option<Vec<bool>> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut index = 0usize;

    while index < run.len() {
        let (start, body_start) = if run[index] == '\x1b'
            && run.get(index + 1) == Some(&'[')
            && run.get(index + 2) == Some(&'<')
        {
            (index, index + 3)
        } else if run[index] == '[' && run.get(index + 1) == Some(&'<') {
            (index, index + 2)
        } else {
            index += 1;
            continue;
        };

        let mut end = body_start;
        let mut has_digit = false;
        let mut has_separator = false;
        let mut matched = false;
        while end < run.len() {
            match run[end] {
                '0'..='9' => {
                    has_digit = true;
                    end += 1;
                }
                ';' | ':' => {
                    has_separator = true;
                    end += 1;
                }
                'M' | 'm' if has_digit && has_separator => {
                    ranges.push((start, end + 1));
                    index = end + 1;
                    matched = true;
                    break;
                }
                _ => break,
            }
        }
        if !matched {
            index = index.saturating_add(1);
        }
    }

    if ranges.is_empty() {
        if looks_like_raw_mouse_report_run(run) {
            return Some(vec![false; run.len()]);
        }
        return None;
    }

    ranges.sort_unstable_by_key(|(start, _)| *start);
    let first_start = ranges[0].0;
    let mut prefix_start = first_start;
    while prefix_start > 0 && is_raw_mouse_report_fragment_char(run[prefix_start - 1]) {
        prefix_start -= 1;
    }
    if prefix_start < first_start
        && looks_like_raw_mouse_report_fragment(&run[prefix_start..first_start])
    {
        ranges.push((prefix_start, first_start));
    }

    let last_end = ranges.iter().map(|(_, end)| *end).max().unwrap_or_default();
    if last_end < run.len() && looks_like_raw_mouse_report_fragment(&run[last_end..]) {
        ranges.push((last_end, run.len()));
    }

    ranges.sort_unstable_by_key(|(start, _)| *start);
    let mut keep = vec![true; run.len()];
    for (start, end) in ranges {
        for slot in keep.iter_mut().take(end.min(run.len())).skip(start) {
            *slot = false;
        }
    }
    Some(keep)
}

fn is_raw_mouse_report_fragment_char(ch: char) -> bool {
    matches!(ch, ';' | ':' | 'M' | 'm') || ch.is_ascii_digit()
}

fn looks_like_raw_mouse_report_fragment(run: &[char]) -> bool {
    if run.len() < 4 {
        return false;
    }
    run.iter().any(|ch| ch.is_ascii_digit())
        && run.iter().any(|ch| matches!(ch, ';' | ':'))
        && run.iter().any(|ch| matches!(ch, 'M' | 'm'))
}

/// Scan `input` for control-sequence fragment shapes (#1915) — OSC 8
/// hyperlinks and Kitty keyboard protocol responses — and excise each
/// match. Returns `(output, new_cursor, changed)`. Cursor positions
/// inside an excised fragment are moved to the fragment's start.
///
/// The match shapes are deliberately narrow so legitimate text like
/// `[is this ok?]` or a typed URL survives untouched:
///
/// - **OSC 8**: `(\x1b?)] 8 ; ...` consuming everything up to the
///   first BEL (`\x07`), `\x1b\\`, lone `\\`, or the next `\x1b]8;`
///   block — terminator characters are optional because crossterm may
///   have already consumed them.
/// - **Kitty CSI**: `(\x1b?) [ (? | > | < | =) ... u` — the
///   private-parameter prefix is what distinguishes a Kitty response
///   from a user-typed `[…u` (which is exceedingly rare and would
///   need an explicit private-parameter byte to be a real CSI).
fn strip_control_sequence_fragments(input: &str, cursor: usize) -> (String, usize, bool) {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut new_cursor = 0usize;
    let mut changed = false;
    let mut index = 0usize;

    while index < chars.len() {
        if let Some(end) = match_osc8_fragment(&chars, index) {
            // The excised span contributes nothing to `output`, so
            // `new_cursor` simply doesn't tick for any of those
            // characters. A cursor that was inside the span ends up at
            // the fragment's start position in the rewritten input,
            // which matches the existing run-stripper's behavior.
            index = end;
            changed = true;
            continue;
        }

        if let Some(end) = match_kitty_csi_fragment(&chars, index) {
            index = end;
            changed = true;
            continue;
        }

        if index < cursor {
            new_cursor += 1;
        }
        output.push(chars[index]);
        index += 1;
    }

    let cursor = new_cursor.min(char_count(&output));
    (output, cursor, changed)
}

/// If an OSC 8 hyperlink fragment starts at `chars[start]`, return its
/// end index (exclusive). The leading `ESC` is optional because
/// crossterm's event parser often consumes it before reclassifying the
/// tail as keystrokes.
fn match_osc8_fragment(chars: &[char], start: usize) -> Option<usize> {
    let body_start = if chars.get(start) == Some(&'\x1b')
        && chars.get(start + 1) == Some(&']')
        && chars.get(start + 2) == Some(&'8')
        && chars.get(start + 3) == Some(&';')
    {
        start + 4
    } else if chars.get(start) == Some(&']')
        && chars.get(start + 1) == Some(&'8')
        && chars.get(start + 2) == Some(&';')
    {
        start + 3
    } else {
        return None;
    };

    // After `]8;` we expect the OSC 8 payload: an optional second `;`
    // (params separator), then the URL (or empty for the closing
    // wrapper), then a terminator. We deliberately stop at the first
    // ASCII whitespace so a typed `]8;` followed by real prose can't
    // swallow the user's words — real OSC 8 URLs don't contain spaces.
    let mut end = body_start;
    while end < chars.len() {
        let ch = chars[end];
        // BEL terminator.
        if ch == '\x07' {
            return Some(end + 1);
        }
        // `ESC \\` string terminator (ST).
        if ch == '\x1b' && chars.get(end + 1) == Some(&'\\') {
            return Some(end + 2);
        }
        // Lone `\\` — crossterm sometimes delivers ST with the leading
        // ESC already consumed, leaving just `\\` as a Char keystroke.
        if ch == '\\' {
            return Some(end + 1);
        }
        // Start of the next OSC 8 wrapper (closing `]8;;` glued to the
        // body) — close the current fragment here so the next iteration
        // matches that one separately.
        if ch == '\x1b' && chars.get(end + 1) == Some(&']') {
            return Some(end);
        }
        if ch == ']' && chars.get(end + 1) == Some(&'8') && chars.get(end + 2) == Some(&';') {
            return Some(end);
        }
        if ch.is_whitespace() {
            // We never crossed a terminator, so this isn't a real
            // fragment — give up rather than eat user prose.
            return None;
        }
        end += 1;
    }

    // Reached end of input without a terminator or whitespace. Treat as
    // a fragment in flight (its tail will arrive on a later keystroke
    // and get filtered then).
    Some(end)
}

/// If a private-parameter CSI fragment starts at `chars[start]`, return its
/// end index (exclusive). Shape: `(ESC)? [ (? | > | < | =) [0-9;:]* <final>`
/// where `<final>` is any ASCII letter. This covers the Kitty keyboard
/// protocol (`…u`) *and* the DEC private mode set/reset sequences a terminal
/// emits during a session — bracketed paste (`[?2004h`/`[?2004l`), mouse
/// capture (`[?1000h`), focus reporting (`[?1004h`), and synchronized output
/// (`[?2026h`). Those end in `h`/`l`, not `u`, so the old `u`-only terminator
/// let the leading `[` leak into the composer during dense streaming (#2592,
/// regression of #1915). The private-parameter byte (`?`, `>`, `<`, `=`) is
/// what keeps this distinct from text the user might plausibly type.
fn match_kitty_csi_fragment(chars: &[char], start: usize) -> Option<usize> {
    let after_csi = if chars.get(start) == Some(&'\x1b') && chars.get(start + 1) == Some(&'[') {
        start + 2
    } else if chars.get(start) == Some(&'[') {
        start + 1
    } else {
        return None;
    };

    let priv_byte = chars.get(after_csi)?;
    if !matches!(priv_byte, '?' | '>' | '<' | '=') {
        return None;
    }

    let mut end = after_csi + 1;
    let mut saw_param = false;
    while end < chars.len() {
        let ch = chars[end];
        if ch.is_ascii_digit() || ch == ';' || ch == ':' {
            saw_param = true;
            end += 1;
            continue;
        }
        // Final byte. The Kitty keyboard protocol ends in `u` and is valid
        // with no parameters (`[?u`). DEC private mode set/reset ends in
        // `h`/`l` and always carries a numeric mode — bracketed paste
        // (`[?2004h`/`l`), mouse capture (`[?1000h`), focus reporting
        // (`[?1004h`), synchronized output (`[?2026h`). Require a parameter
        // before `h`/`l` so ordinary text like `[?help]` is left untouched.
        return match ch {
            'u' => Some(end + 1),
            'h' | 'l' if saw_param => Some(end + 1),
            _ => None,
        };
    }
    None
}

pub(crate) const MAX_SUBMITTED_INPUT_CHARS: usize = 16_000;

/// Bounded preview shown inside the attachment card for a consolidated
/// paste. Small enough to stay a summary, large enough to recognize the
/// content.
const PASTE_ATTACHMENT_PREVIEW_CHARS: usize = 240;

/// Human-readable submission text for a paste-backed input: a size header,
/// the `@`-mention that attaches the file for the model, and a bounded
/// preview. The mention must survive verbatim — file-mention resolution
/// scans the submitted text for it.
fn paste_attachment_display(reference: &str, full: &str) -> String {
    let chars = full.chars().count();
    let preview_lines: Vec<&str> = full.lines().take(3).collect();
    let joined = preview_lines.join("\n");
    let preview: String = joined
        .chars()
        .take(PASTE_ATTACHMENT_PREVIEW_CHARS)
        .collect();
    let elided = if chars > PASTE_ATTACHMENT_PREVIEW_CHARS {
        "…"
    } else {
        ""
    };
    format!(
        "[Pasted content attached · {chars} chars]\n{reference}\n--- preview ---\n{preview}{elided}"
    )
}
/// Maximum characters displayed in the composer for oversized input.
/// Beyond this, the text is truncated for rendering but the full content
/// is preserved for model submission (#3263).
const MAX_COMPOSER_DISPLAY_CHARS: usize = 4_000;
const MAX_DRAFT_HISTORY: usize = 50;

impl ComposerState {
    /// Re-derive the "this line began as a command" claim from the current
    /// composer text (#5925).
    ///
    /// Called after every composer mutation. A user edit that removes the
    /// leading `/` releases the claim — removing it was intentional. Bytes
    /// lost between the terminal and the composer never pass through here,
    /// which is exactly the difference the submit guard needs: a claim that
    /// survives to Enter means the line still starts with `/` and must be
    /// dispatched as a command, never re-read as a prose prompt.
    pub(crate) fn resync_command_line_claim(&mut self) {
        self.line_began_with_slash &= self.input.trim_start().starts_with('/');
    }

    /// Whether this line is claimed as a command: it began with a typed `/`
    /// and still starts with one.
    pub(crate) fn command_line_claimed(&self) -> bool {
        self.line_began_with_slash && looks_like_slash_command_input(&self.input)
    }

    /// When the user starts editing a truncated oversized paste, restore the
    /// full text so they can see and edit the complete content (#3263).
    fn auto_expand_oversized_paste(&mut self) {
        if let Some(full) = self.oversized_paste_full_text.take() {
            self.input = full;
            self.resync_command_line_claim();
            // Clamp cursor to the new length instead of resetting to 0,
            // so the user's position in the truncated preview is preserved.
            self.cursor_position = self.cursor_position.min(char_count(&self.input));
        }
    }

    pub fn composer_attachment_count(&self) -> usize {
        codewhale_core::media_attachment_references(&self.input).len()
    }

    pub fn selected_composer_attachment_index(&self) -> Option<usize> {
        let count = self.composer_attachment_count();
        self.selected_attachment_index
            .filter(|index| *index < count)
    }

    fn strip_raw_mouse_reports_from_input(&mut self) {
        if let Some((input, cursor_position)) =
            strip_raw_mouse_report_runs(&self.input, self.cursor_position)
        {
            self.input = input;
            self.resync_command_line_claim();
            self.cursor_position = cursor_position;
        }
    }

    // === Selection helpers ===
    /// Return the (start, end) of the active selection, or `None`.
    /// `start` is inclusive, `end` is exclusive; both are char indices.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let total = char_count(&self.input);
        let anchor = self.selection_anchor?.min(total);
        let cursor = self.cursor_position.min(total);
        if anchor == cursor {
            return None;
        }
        Some(if anchor < cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        })
    }

    /// Return the selected text, or empty string if no selection.
    pub fn selected_text(&self) -> String {
        self.selection_range()
            .map(|(s, e)| {
                let sb = byte_index_at_char(&self.input, s);
                let eb = byte_index_at_char(&self.input, e);
                self.input[sb..eb].to_string()
            })
            .unwrap_or_default()
    }

    /// Clear the selection without moving the cursor.
    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    /// Returns `true` when vim mode is active and the composer is in Normal
    /// mode, which means character keys should NOT be inserted as text.
    #[must_use]
    pub fn vim_is_normal_mode(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Normal
    }

    /// Returns `true` when vim mode is active and the composer is in Visual mode.
    #[must_use]
    pub fn vim_is_visual_mode(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Visual
    }

    pub fn stash_current_input_for_recovery(&mut self) {
        // Before stashing, expand any truncated paste so the saved draft
        // contains the full text, not the truncated preview (#3263).
        self.auto_expand_oversized_paste();
        let draft = self.input.clone();
        if draft.trim().is_empty() {
            self.clear_undo_buffer = None;
            return;
        }
        self.clear_undo_buffer = Some(draft.clone());
        self.remember_draft_for_recovery(draft);
    }

    fn remember_draft_for_recovery(&mut self, draft: String) {
        if draft.trim().is_empty() {
            return;
        }
        self.draft_history.retain(|existing| existing != &draft);
        self.draft_history.push_back(draft);
        while self.draft_history.len() > MAX_DRAFT_HISTORY {
            let _ = self.draft_history.pop_front();
        }
    }

    pub fn is_history_search_active(&self) -> bool {
        self.composer_history_search.is_some()
    }

    pub fn history_search_query(&self) -> Option<&str> {
        self.composer_history_search
            .as_ref()
            .map(|search| search.query.as_str())
    }

    pub fn history_search_selected_index(&self) -> usize {
        self.composer_history_search
            .as_ref()
            .map_or(0, |search| search.selected)
    }

    pub fn composer_display_input(&self) -> &str {
        self.history_search_query().unwrap_or(&self.input)
    }

    pub fn composer_display_cursor(&self) -> usize {
        self.composer_history_search
            .as_ref()
            .map_or(self.cursor_position, |search| char_count(&search.query))
    }

    pub fn history_search_matches(&self) -> Vec<String> {
        let Some(query) = self.history_search_query() else {
            return Vec::new();
        };
        self.history_search_matches_for_query(query)
    }

    fn history_search_matches_for_query(&self, query: &str) -> Vec<String> {
        let normalized_query = query.trim().to_lowercase();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut matches = Vec::new();

        for candidate in self
            .draft_history
            .iter()
            .rev()
            .chain(self.input_history.iter().rev())
        {
            if candidate.trim().is_empty() || !seen.insert(candidate.as_str()) {
                continue;
            }
            if normalized_query.is_empty() || candidate.to_lowercase().contains(&normalized_query) {
                matches.push(candidate.clone());
            }
        }

        matches
    }

    fn clamp_history_search_selection(&mut self) {
        let Some(search) = self.composer_history_search.as_ref() else {
            return;
        };
        let selected = search.selected;
        let query = search.query.clone();
        let match_count = self.history_search_matches_for_query(&query).len();
        if let Some(search) = self.composer_history_search.as_mut() {
            search.selected = if match_count == 0 {
                0
            } else {
                selected.min(match_count.saturating_sub(1))
            };
        }
    }

    pub fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            // Expand truncated paste first so the saved draft contains the
            // full text instead of the truncated preview (#3263).
            self.auto_expand_oversized_paste();
            self.history_navigation_draft = Some(InputHistoryDraft {
                input: self.input.clone(),
                cursor: self.cursor_position,
            });
        }
        let new_index = match self.history_index {
            None => self.input_history.len().saturating_sub(1),
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(new_index);
        self.input = self.input_history[new_index].clone();
        self.resync_command_line_claim();
        self.cursor_position = char_count(&self.input);
        self.selection_anchor = None;
        self.selected_attachment_index = None;
        self.slash_menu_hidden = false;
        self.paste_burst.clear_after_explicit_paste();
    }

    fn clear_input_history_navigation(&mut self) {
        self.history_index = None;
        self.history_navigation_draft = None;
    }
}

impl App {
    pub fn insert_str(&mut self, text: &str) {
        self.insert_str_with_raw_mouse_recovery(text, true);
    }

    fn insert_str_with_raw_mouse_recovery(&mut self, text: &str, recover_raw_mouse: bool) {
        if text.is_empty() {
            return;
        }
        // Any edit detaches a recalled history entry (mirrors insert_char):
        // without this a paste typed while navigating stays on a stale index
        // and the next Up/Down silently discards the pasted text.
        self.clear_input_history_navigation();
        self.auto_expand_oversized_paste();
        self.delete_selection();
        self.selected_attachment_index = None;
        let cursor = self.cursor_position.min(char_count(&self.input));
        let byte_index = byte_index_at_char(&self.input, cursor);
        self.input.insert_str(byte_index, text);
        self.resync_command_line_claim();
        self.cursor_position = cursor + char_count(text);
        if recover_raw_mouse {
            self.strip_raw_mouse_reports_from_input();
        }
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.needs_redraw = true;
    }

    pub fn insert_paste_text(&mut self, text: &str) {
        if let Some(pending) = self.paste_burst.flush_before_modified_input() {
            self.insert_str(&pending);
        }
        if self.attach_pasted_image_paths(text) {
            self.paste_burst.clear_after_explicit_paste();
            return;
        }
        let normalized = normalize_paste_text(text);
        if !normalized.is_empty() {
            self.insert_str_with_raw_mouse_recovery(&normalized, false);
        }
        self.paste_burst.clear_after_explicit_paste();
        // Large pasted input stays editable and visible until submit. The
        // submit-time safety net consolidates oversized composer content into
        // an @paste-...md mention before dispatch, so no path silently
        // truncates user input.
        // self.consolidate_large_input_if_oversized(); // deferred to submit time
    }

    /// A file dragged onto the terminal arrives as a paste of its path. When
    /// the whole paste names local image files, attach them on the same
    /// `[Attached image: …]` line a clipboard image gets, so each shows as a
    /// composer attachment and is sent to the model as an image part (the
    /// engine expands the line when it builds the user message, and the
    /// route's vision capability decides per request; a text-only route gets
    /// the omission notice there). Returns whether the paste was consumed.
    ///
    /// A command line (`/…`) or shell line (`!…`) keeps the literal path.
    pub(crate) fn attach_pasted_image_paths(&mut self, text: &str) -> bool {
        if self.input.trim_start().starts_with(['/', '!']) {
            return false;
        }
        let Some(paths) = codewhale_engine::image_attach::pasted_image_paths(text) else {
            return false;
        };
        for path in &paths {
            self.insert_media_attachment("image", path, None);
        }
        self.status_message = Some(match paths.as_slice() {
            [path] => format!("Attached image: {}", path.display()),
            paths => format!("Attached {} images", paths.len()),
        });
        true
    }

    pub fn insert_media_attachment(&mut self, kind: &str, path: &Path, description: Option<&str>) {
        let reference = media_attachment_reference(kind, path, description);
        let cursor = self.cursor_position.min(char_count(&self.input));
        let byte_index = byte_index_at_char(&self.input, cursor);
        let needs_prefix_newline = self.input[..byte_index]
            .chars()
            .last()
            .is_some_and(|ch| !ch.is_whitespace());
        let needs_suffix_newline = self.input[byte_index..]
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_whitespace());

        let mut inserted = String::new();
        if needs_prefix_newline {
            inserted.push('\n');
        }
        inserted.push_str(&reference);
        if needs_suffix_newline || self.input[byte_index..].is_empty() {
            inserted.push('\n');
        }
        self.insert_str(&inserted);
        self.paste_burst.clear_after_explicit_paste();
    }

    pub fn select_previous_composer_attachment(&mut self) -> bool {
        let count = self.composer_attachment_count();
        if count == 0 {
            self.selected_attachment_index = None;
            return false;
        }

        let next = self
            .selected_composer_attachment_index()
            .map_or(count.saturating_sub(1), |index| index.saturating_sub(1));
        self.selected_attachment_index = Some(next);
        self.cursor_position = 0;
        self.status_message = Some("Attachment selected - Backspace/Delete removes it".to_string());
        self.needs_redraw = true;
        true
    }

    pub fn select_next_composer_attachment(&mut self) -> bool {
        let count = self.composer_attachment_count();
        let Some(index) = self.selected_composer_attachment_index() else {
            return false;
        };
        if index + 1 < count {
            self.selected_attachment_index = Some(index + 1);
            self.status_message =
                Some("Attachment selected - Backspace/Delete removes it".to_string());
        } else {
            self.selected_attachment_index = None;
            self.status_message = Some("Composer focused".to_string());
        }
        self.needs_redraw = true;
        true
    }

    pub fn clear_composer_attachment_selection(&mut self) -> bool {
        if self.selected_attachment_index.take().is_some() {
            self.status_message = Some("Composer focused".to_string());
            self.needs_redraw = true;
            true
        } else {
            false
        }
    }

    pub fn remove_selected_composer_attachment(&mut self) -> bool {
        let references = codewhale_core::media_attachment_references(&self.input);
        let Some(index) = self
            .selected_composer_attachment_index()
            .filter(|index| *index < references.len())
        else {
            self.selected_attachment_index = None;
            return false;
        };
        self.clear_input_history_navigation();
        let reference = references[index].clone();
        let cursor_byte = byte_index_at_char(&self.input, self.cursor_position);
        let new_cursor_byte = if cursor_byte <= reference.start_byte {
            cursor_byte
        } else if cursor_byte >= reference.end_byte {
            cursor_byte.saturating_sub(reference.end_byte - reference.start_byte)
        } else {
            reference.start_byte
        };

        self.input
            .replace_range(reference.start_byte..reference.end_byte, "");
        self.cursor_position = self.input[..new_cursor_byte.min(self.input.len())]
            .chars()
            .count();
        let remaining = self.composer_attachment_count();
        self.selected_attachment_index = if remaining == 0 {
            None
        } else {
            Some(index.min(remaining.saturating_sub(1)))
        };
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.status_message = Some(format!("Removed attachment: {}", reference.path));
        self.needs_redraw = true;
        true
    }

    #[cfg(test)]
    pub fn flush_paste_burst_if_due(&mut self, now: Instant) -> bool {
        match self.paste_burst.flush_if_due(now) {
            FlushResult::Paste(text) => {
                self.insert_str(&text);
                true
            }
            FlushResult::Typed(ch) => {
                self.insert_char(ch);
                true
            }
            FlushResult::SuppressionExpired => {
                self.needs_redraw = true;
                true
            }
            FlushResult::None => false,
        }
    }

    pub(crate) fn take_paste_burst_flush_if_enabled(&mut self, now: Instant) -> FlushResult {
        if self.use_paste_burst_detection {
            self.paste_burst.flush_if_due(now)
        } else {
            FlushResult::None
        }
    }

    pub fn paste_burst_next_flush_delay_if_enabled(&self, now: Instant) -> Option<Duration> {
        if self.use_paste_burst_detection {
            self.paste_burst.next_flush_delay(now)
        } else {
            None
        }
    }

    pub fn flush_paste_burst_before_modified_input_if_enabled(&mut self) -> Option<String> {
        if self.use_paste_burst_detection {
            self.paste_burst.flush_before_modified_input()
        } else {
            None
        }
    }

    /// Paste from clipboard into input.
    ///
    /// Returns whether content was inserted. In SSH sessions without a
    /// forwarded graphical display, the terminal client owns paste, so direct
    /// clipboard shortcuts surface the local terminal-paste instruction while
    /// `Event::Paste` remains the data path.
    pub fn paste_from_clipboard(&mut self) -> bool {
        if self.clipboard.requires_terminal_paste() {
            self.status_message = Some(self.tr(MessageId::ClipboardSshPasteHint).into_owned());
            return false;
        }
        if let Some(content) = self.clipboard.read_markdown(self.workspace.as_path()) {
            self.apply_clipboard_content(content);
            return true;
        }
        // The read cannot tell an empty clipboard from an unreadable one;
        // either way the user asked to paste and nothing happened.
        self.status_message = Some(self.tr(MessageId::ClipboardNothingToPaste).into_owned());
        false
    }

    pub fn apply_clipboard_content(&mut self, content: ClipboardContent) {
        match content {
            ClipboardContent::Text(text) => {
                self.insert_paste_text(&text);
            }
            ClipboardContent::Image(pasted) => {
                let description = format!("{} ({})", pasted.short_label(), pasted.size_label());
                self.insert_media_attachment("image", &pasted.path, Some(&description));
                self.status_message = Some(format!("Attached image: {description}"));
            }
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.acknowledge_sticky_on_composer_activity();
        self.clear_input_history_navigation();
        self.auto_expand_oversized_paste();
        self.delete_selection();
        self.selected_attachment_index = None;
        // #5925: a line that starts with a typed `/` is a command from its
        // first byte, and stays one until Enter. Recorded here — the only
        // place a character reaches the composer by typing — so the submit
        // guard can tell a lost `/` from a deleted one.
        let starts_line = self.input.trim().is_empty();
        let cursor = self.cursor_position.min(char_count(&self.input));
        let byte_index = byte_index_at_char(&self.input, cursor);
        self.input.insert(byte_index, c);
        if starts_line {
            self.line_began_with_slash = c == '/';
        }
        self.resync_command_line_claim();
        self.cursor_position = cursor + 1;
        self.strip_raw_mouse_reports_from_input();
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.needs_redraw = true;
    }

    pub fn delete_char(&mut self) {
        self.clear_input_history_navigation();
        self.auto_expand_oversized_paste();
        if self.delete_selection() {
            return;
        }
        self.selected_attachment_index = None;
        if self.cursor_position == 0 {
            return;
        }
        // Grapheme-aware: Backspace removes the whole cluster before the
        // cursor (emoji ZWJ sequence, flag pair, CJK char + combining mark),
        // never a lone scalar out of the middle of one.
        let cursor = self.cursor_position.min(char_count(&self.input));
        let target = prev_grapheme_boundary(&self.input, cursor);
        let removed = remove_char_range(&mut self.input, target, cursor);
        self.resync_command_line_claim();
        if removed {
            self.cursor_position = target;
            self.slash_menu_hidden = false;
            self.mention_menu_hidden = false;
            self.mention_menu_selected = 0;
            self.needs_redraw = true;
        }
    }

    pub fn delete_char_forward(&mut self) {
        self.clear_input_history_navigation();
        self.auto_expand_oversized_paste();
        if self.delete_selection() {
            return;
        }
        self.selected_attachment_index = None;
        if self.input.is_empty() {
            return;
        }
        // Grapheme-aware: forward-delete removes the whole cluster at the
        // cursor rather than a single scalar from inside it.
        let target = self.cursor_position;
        let end = next_grapheme_boundary(&self.input, target);
        let removed = remove_char_range(&mut self.input, target, end);
        self.resync_command_line_claim();
        if !removed {
            self.cursor_position = char_count(&self.input);
        }
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.needs_redraw = true;
    }

    /// Delete the word before the cursor.
    pub fn delete_word_backward(&mut self) {
        self.clear_input_history_navigation();
        if self.delete_selection() {
            return;
        }
        self.selected_attachment_index = None;
        if self.cursor_position == 0 {
            return;
        }

        let cursor_byte = byte_index_at_char(&self.input, self.cursor_position);
        let mut word_start = cursor_byte;

        while word_start > 0 {
            let Some((prev, ch)) = self.input[..word_start].char_indices().next_back() else {
                break;
            };
            if !ch.is_whitespace() {
                break;
            }
            word_start = prev;
        }

        while word_start > 0 {
            let Some((prev, ch)) = self.input[..word_start].char_indices().next_back() else {
                break;
            };
            if ch.is_whitespace() {
                break;
            }
            word_start = prev;
        }

        if word_start < cursor_byte {
            self.input.replace_range(word_start..cursor_byte, "");
            self.resync_command_line_claim();
            self.cursor_position = char_count(&self.input[..word_start]);
            self.slash_menu_hidden = false;
            self.mention_menu_hidden = false;
            self.mention_menu_selected = 0;
            self.needs_redraw = true;
        }
    }

    /// Delete from the cursor to the start of the line.
    pub fn delete_to_start_of_line(&mut self) {
        self.clear_input_history_navigation();
        if self.delete_selection() {
            return;
        }
        self.selected_attachment_index = None;
        if self.cursor_position == 0 {
            return;
        }

        let cursor_byte = byte_index_at_char(&self.input, self.cursor_position);
        // Find the start of the current line (last newline or start of string)
        let line_start = self.input[..cursor_byte]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);

        if line_start < cursor_byte {
            self.input.replace_range(line_start..cursor_byte, "");
            self.resync_command_line_claim();
            self.cursor_position = char_count(&self.input[..line_start]);
            self.slash_menu_hidden = false;
            self.mention_menu_hidden = false;
            self.mention_menu_selected = 0;
            self.needs_redraw = true;
        }
    }

    /// Delete the word after the cursor.
    pub fn delete_word_forward(&mut self) {
        self.clear_input_history_navigation();
        if self.delete_selection() {
            return;
        }
        self.selected_attachment_index = None;
        let cursor_byte = byte_index_at_char(&self.input, self.cursor_position);
        if cursor_byte >= self.input.len() {
            return;
        }

        let mut word_end = cursor_byte;
        while word_end < self.input.len() {
            let Some(ch) = self.input[word_end..].chars().next() else {
                break;
            };
            if !ch.is_whitespace() {
                break;
            }
            word_end += ch.len_utf8();
        }

        while word_end < self.input.len() {
            let Some(ch) = self.input[word_end..].chars().next() else {
                break;
            };
            if ch.is_whitespace() {
                break;
            }
            word_end += ch.len_utf8();
        }

        if cursor_byte < word_end {
            self.input.replace_range(cursor_byte..word_end, "");
            self.resync_command_line_claim();
            self.slash_menu_hidden = false;
            self.mention_menu_hidden = false;
            self.mention_menu_selected = 0;
            self.needs_redraw = true;
        }
    }

    /// Cut from the cursor to the end of the current logical line into the
    /// kill buffer. If the cursor is already at end-of-line and a trailing
    /// newline exists, that newline is consumed so repeated invocations
    /// continue to make progress (matching emacs/codex semantics).
    ///
    /// Returns `true` when bytes were moved into the kill buffer.
    pub fn kill_to_end_of_line(&mut self) -> bool {
        self.clear_input_history_navigation();
        if let Some((start, end)) = self.selection_range() {
            let sb = byte_index_at_char(&self.input, start);
            let eb = byte_index_at_char(&self.input, end);
            self.kill_buffer = self.input[sb..eb].to_string();
            self.delete_selection();
            return true;
        }
        let total_chars = char_count(&self.input);
        let cursor = self.cursor_position.min(total_chars);
        let start_byte = byte_index_at_char(&self.input, cursor);

        // Find the byte offset of the next '\n' (relative to the whole string)
        // or the end of the buffer if no newline exists at/after the cursor.
        let eol_byte = self.input[start_byte..]
            .find('\n')
            .map(|rel| start_byte + rel)
            .unwrap_or_else(|| self.input.len());

        let end_byte = if start_byte == eol_byte {
            // Cursor is at EOL — consume the newline itself if one is there.
            if eol_byte < self.input.len() {
                eol_byte + 1
            } else {
                return false;
            }
        } else {
            eol_byte
        };

        let removed: String = self.input[start_byte..end_byte].to_string();
        if removed.is_empty() {
            return false;
        }

        self.kill_buffer = removed;
        self.input.replace_range(start_byte..end_byte, "");
        self.resync_command_line_claim();
        // Cursor stays at the same character index (start of removed range).
        self.cursor_position = cursor;
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.needs_redraw = true;
        true
    }

    /// Insert the contents of the kill buffer at the cursor, advancing it.
    /// The kill buffer is left intact so multiple yanks duplicate the text.
    /// Returns `true` if any text was inserted.
    pub fn yank(&mut self) -> bool {
        if self.kill_buffer.is_empty() {
            return false;
        }
        self.delete_selection();
        self.clear_input_history_navigation();
        let text = self.kill_buffer.clone();
        let cursor = self.cursor_position.min(char_count(&self.input));
        let byte_index = byte_index_at_char(&self.input, cursor);
        self.input.insert_str(byte_index, &text);
        self.resync_command_line_claim();
        self.cursor_position = cursor + char_count(&text);
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.needs_redraw = true;
        true
    }

    pub fn move_cursor_left(&mut self) {
        let cursor = self.cursor_position.min(char_count(&self.input));
        self.cursor_position = prev_grapheme_boundary(&self.input, cursor);
        self.needs_redraw = true;
    }

    pub fn move_cursor_right(&mut self) {
        let total = char_count(&self.input);
        if self.cursor_position < total {
            self.cursor_position = next_grapheme_boundary(&self.input, self.cursor_position);
            self.needs_redraw = true;
        }
    }

    pub fn move_cursor_start(&mut self) {
        self.cursor_position = 0;
        self.needs_redraw = true;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_position = char_count(&self.input);
        self.needs_redraw = true;
    }

    /// In a multiline composer, jump to the start of the current line.
    /// On single-line input this is equivalent to `move_cursor_start`.
    pub fn move_cursor_line_start(&mut self) {
        let byte_pos = byte_index_at_char(&self.input, self.cursor_position);
        let before = &self.input[..byte_pos];
        if let Some(last_nl_byte) = before.rfind('\n') {
            // Position after the '\n' (start of the current line).
            self.cursor_position = char_count(&self.input[..=last_nl_byte]);
        } else {
            self.cursor_position = 0;
        }
        self.needs_redraw = true;
    }

    /// In a multiline composer, jump to the end of the current line
    /// (just before the next `\n` or at the end of input).
    /// On single-line input this is equivalent to `move_cursor_end`.
    pub fn move_cursor_line_end(&mut self) {
        let search_start = byte_index_at_char(&self.input, self.cursor_position);
        if let Some(offset) = self.input[search_start..].find('\n') {
            self.cursor_position = char_count(&self.input[..search_start + offset]);
        } else {
            self.cursor_position = char_count(&self.input);
        }
        self.needs_redraw = true;
    }

    /// Move forward one word. Skips over the current word then any trailing
    /// whitespace to land on the first character of the next word.
    pub fn move_cursor_word_forward(&mut self) {
        let text = self.input.clone();
        let total = char_count(&text);
        let mut pos = self.cursor_position;
        if pos >= total {
            return;
        }
        // Skip non-whitespace (current word).
        while pos < total {
            let byte = byte_index_at_char(&text, pos);
            let ch = text[byte..].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            pos += 1;
        }
        // Skip whitespace.
        while pos < total {
            let byte = byte_index_at_char(&text, pos);
            let ch = text[byte..].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            pos += 1;
        }
        self.cursor_position = pos;
        self.needs_redraw = true;
    }

    /// Move backward one word. Skips leading whitespace then the preceding
    /// word to land on its first character.
    pub fn move_cursor_word_backward(&mut self) {
        let text = self.input.clone();
        let mut pos = self.cursor_position;
        if pos == 0 {
            return;
        }
        // Step back one so we're not already at the word start.
        pos -= 1;
        // Skip whitespace.
        while pos > 0 {
            let byte = byte_index_at_char(&text, pos);
            let ch = text[byte..].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            pos -= 1;
        }
        // Skip non-whitespace.
        while pos > 0 {
            let byte = byte_index_at_char(&text, pos - 1);
            let ch = text[byte..].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            pos -= 1;
        }
        self.cursor_position = pos;
        self.needs_redraw = true;
    }

    /// Select the entire composer contents: anchor at the start, cursor at
    /// the end. Expands an oversized-paste preview first so the selection
    /// covers the real draft, not a truncated placeholder (#3263).
    pub fn select_all(&mut self) {
        self.auto_expand_oversized_paste();
        if self.input.is_empty() {
            self.selection_anchor = None;
            return;
        }
        self.selection_anchor = Some(0);
        self.cursor_position = char_count(&self.input);
        self.needs_redraw = true;
    }

    /// Delete the selected text, place cursor at the start of the deleted range.
    /// Returns true if a selection was deleted.
    ///
    /// When the selection spans the whole draft (e.g. select-all then type or
    /// Backspace), the outgoing text is stashed exactly like `Ctrl+U` so the
    /// destruction is recoverable with `Ctrl+Z` / the draft history.
    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        if start == 0 && end == char_count(&self.input) {
            let draft = self.input.clone();
            if !draft.trim().is_empty() {
                self.clear_undo_buffer = Some(draft.clone());
                self.remember_draft_for_recovery(draft);
            }
        }
        let sb = byte_index_at_char(&self.input, start);
        let eb = byte_index_at_char(&self.input, end);
        self.input.replace_range(sb..eb, "");
        self.resync_command_line_claim();
        self.cursor_position = start;
        self.selection_anchor = None;
        self.clear_input_history_navigation();
        self.slash_menu_hidden = false;
        self.mention_menu_hidden = false;
        self.mention_menu_selected = 0;
        self.needs_redraw = true;
        true
    }

    // === Vim composer mode helpers ===
    /// Move the cursor to the start of the current logical line (vim `0`).
    pub fn vim_move_line_start(&mut self) {
        let text = self.input.clone();
        let cursor_byte = byte_index_at_char(&text, self.cursor_position);
        // Walk backward until we find a newline or the start of the string.
        let line_start_byte = text[..cursor_byte].rfind('\n').map_or(0, |idx| idx + 1);
        self.cursor_position = char_count(&text[..line_start_byte]);
        self.needs_redraw = true;
    }

    /// Move the cursor onto the last character of the current logical line
    /// (vim `$`).
    pub fn vim_move_line_end(&mut self) {
        self.move_cursor_line_end();
        self.vim_step_off_line_end();
    }

    fn vim_step_off_line_end(&mut self) {
        let cursor = self.cursor_position.min(char_count(&self.input));
        let cursor_byte = byte_index_at_char(&self.input, cursor);
        let line_start_byte = self.input[..cursor_byte]
            .rfind('\n')
            .map_or(0, |idx| idx + 1);
        let on_line_end = self.input[cursor_byte..]
            .chars()
            .next()
            .is_none_or(|ch| ch == '\n');
        if on_line_end && cursor_byte > line_start_byte {
            self.cursor_position = prev_grapheme_boundary(&self.input, cursor);
        }
    }

    /// Move forward one word (vim `w`).  Skips over the current word then any
    /// trailing whitespace to land on the first character of the next word.
    pub fn vim_move_word_forward(&mut self) {
        self.move_cursor_word_forward();
    }

    /// Move backward one word (vim `b`).  Skips leading whitespace then the
    /// preceding word to land on its first character.
    pub fn vim_move_word_backward(&mut self) {
        self.move_cursor_word_backward();
    }

    /// Delete the character under the cursor (vim `x`).
    pub fn vim_delete_char_under_cursor(&mut self) {
        self.clear_input_history_navigation();
        self.auto_expand_oversized_paste();
        let total = char_count(&self.input);
        if self.cursor_position >= total {
            return;
        }
        let pos = self.cursor_position;
        if self.input[byte_index_at_char(&self.input, pos)..].starts_with('\n') {
            return;
        }
        // Grapheme-aware: `x` deletes the whole cluster under the cursor.
        let end = next_grapheme_boundary(&self.input, pos);
        remove_char_range(&mut self.input, pos, end);
        self.resync_command_line_claim();
        // Keep cursor in bounds after deletion, on the line's last cluster's start.
        self.vim_step_off_line_end();
        self.needs_redraw = true;
    }

    /// Delete the entire current logical line (vim `dd`).
    pub fn vim_delete_line(&mut self) {
        self.clear_input_history_navigation();
        let text = self.input.clone();
        let cursor_byte = byte_index_at_char(&text, self.cursor_position);
        let line_start_byte = text[..cursor_byte].rfind('\n').map_or(0, |idx| idx + 1);
        let line_end_byte = text[cursor_byte..]
            .find('\n')
            .map_or(text.len(), |rel| cursor_byte + rel);

        // Include the trailing newline if present, or the leading newline for the
        // very last non-terminated line to avoid leaving a dangling newline.
        let (remove_start, remove_end) = if line_end_byte < text.len() {
            // There is a newline after the line — remove it too.
            (line_start_byte, line_end_byte + 1)
        } else if line_start_byte > 0 {
            // Last line without trailing newline — remove the preceding newline.
            (line_start_byte - 1, line_end_byte)
        } else {
            // Only line in the buffer.
            (line_start_byte, line_end_byte)
        };

        self.input.replace_range(remove_start..remove_end, "");
        self.resync_command_line_claim();
        self.cursor_position = char_count(&self.input[..remove_start]);
        self.needs_redraw = true;
    }

    /// Enter insert mode at the cursor (vim `i`).
    pub fn vim_enter_insert(&mut self) {
        self.vim_mode = VimMode::Insert;
        self.needs_redraw = true;
    }

    /// Enter insert mode after the cursor (vim `a`).
    pub fn vim_enter_append(&mut self) {
        let total = char_count(&self.input);
        if self.cursor_position < total
            && !self.input[byte_index_at_char(&self.input, self.cursor_position)..]
                .starts_with('\n')
        {
            // After the whole cluster under the cursor, not one scalar in.
            self.cursor_position = next_grapheme_boundary(&self.input, self.cursor_position);
        }
        self.vim_mode = VimMode::Insert;
        self.needs_redraw = true;
    }

    /// Open a new line below and enter insert mode (vim `o`).
    pub fn vim_open_line_below(&mut self) {
        // Move to end of line, then insert a newline.
        self.move_cursor_line_end();
        self.insert_char('\n');
        self.vim_mode = VimMode::Insert;
    }

    /// Return to Normal mode from Insert or Visual (vim `Esc`).
    pub fn vim_enter_normal(&mut self) {
        self.vim_mode = VimMode::Normal;
        self.vim_pending_d = false;
        // In Normal mode the cursor sits on a character, not after the last
        // one: on the last grapheme cluster's start (U01-m4).
        let total = char_count(&self.input);
        if self.cursor_position > 0 && self.cursor_position >= total {
            self.cursor_position = prev_grapheme_boundary(&self.input, total);
        }
        self.needs_redraw = true;
    }

    /// Move the cursor down one logical line within the buffer (vim `j`).
    /// Falls back to history-down when already on the last line.
    pub fn vim_move_down(&mut self) {
        let text = self.input.clone();
        let total = char_count(&text);
        if self.cursor_position >= total {
            self.history_down();
            return;
        }
        let cursor_byte = byte_index_at_char(&text, self.cursor_position);
        let rest = &text[cursor_byte..];
        if let Some(rel_nl) = rest.find('\n') {
            // Column offset on the current line.
            let line_start_byte = text[..cursor_byte].rfind('\n').map_or(0, |i| i + 1);
            let col = char_count(&text[line_start_byte..cursor_byte]);
            let next_line_start = cursor_byte + rel_nl + 1;
            let next_line = &text[next_line_start..];
            let next_line_len = next_line.find('\n').unwrap_or(next_line.len());
            let next_line_char_len =
                char_count(&text[next_line_start..next_line_start + next_line_len]);
            let target_col = col.min(next_line_char_len);
            self.cursor_position =
                floor_grapheme_boundary(&text, char_count(&text[..next_line_start]) + target_col);
            self.needs_redraw = true;
        } else {
            self.history_down();
        }
    }

    /// Move the cursor up one logical line within the buffer (vim `k`).
    /// Falls back to history-up when already on the first line.
    pub fn vim_move_up(&mut self) {
        let text = self.input.clone();
        let cursor_byte = byte_index_at_char(&text, self.cursor_position);
        if let Some(prev_nl) = text[..cursor_byte].rfind('\n') {
            // Column on the current line.
            let line_start_byte = prev_nl + 1;
            let col = char_count(&text[line_start_byte..cursor_byte]);
            // Find start of the previous line.
            let prev_line_end = prev_nl; // byte of the newline itself
            let prev_start = text[..prev_line_end].rfind('\n').map_or(0, |i| i + 1);
            let prev_line_len = char_count(&text[prev_start..prev_line_end]);
            let target_col = col.min(prev_line_len);
            self.cursor_position =
                floor_grapheme_boundary(&text, char_count(&text[..prev_start]) + target_col);
            self.needs_redraw = true;
        } else {
            self.history_up();
        }
    }

    pub fn clear_input(&mut self) {
        self.clear_input_history_navigation();
        self.input.clear();
        self.resync_command_line_claim();
        self.cursor_position = 0;
        // Prevent stale oversized-paste state from leaking when the user
        // clears the composer or navigates to a different input (#3263).
        self.pending_paste_reference = None;
        self.oversized_paste_full_text = None;
        self.selection_anchor = None;
        self.selected_attachment_index = None;
        self.slash_menu_selected = 0;
        self.slash_menu_hidden = false;
        self.paste_burst.clear_after_explicit_paste();
        self.needs_redraw = true;
    }

    pub fn clear_input_recoverable(&mut self) {
        self.stash_current_input_for_recovery();
        self.clear_input();
    }

    pub fn clear_composer_draft(&mut self) {
        self.backtrack.reset();
        self.disarm_quit();
        self.edit_in_progress = false;
        self.clear_input_recoverable();
        let _ = self.maybe_show_behavioral_tip(
            crate::tui::behavioral_tips::BehavioralTip::ClearedInputRestore,
        );
    }

    pub fn start_history_search(&mut self) {
        if self.composer_history_search.is_some() {
            return;
        }
        // Expand any truncated paste first so the history search seed
        // contains the full text, not the truncated preview (#3263).
        self.auto_expand_oversized_paste();
        self.composer_history_search = Some(ComposerHistorySearch::new(
            self.input.clone(),
            self.cursor_position,
        ));
        self.slash_menu_hidden = true;
        self.mention_menu_hidden = true;
        self.paste_burst.clear_after_explicit_paste();
        self.status_message = Some("History search: type to filter, Enter accepts".to_string());
        self.needs_redraw = true;
    }

    pub fn history_search_insert_char(&mut self, ch: char) {
        if let Some(search) = self.composer_history_search.as_mut() {
            search.query.push(ch);
            search.selected = 0;
            self.status_message = Some("History search: Enter accepts, Esc restores".to_string());
            self.needs_redraw = true;
        }
    }

    pub fn history_search_insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(search) = self.composer_history_search.as_mut() {
            search.query.push_str(&normalize_paste_text(text));
            search.selected = 0;
            self.status_message = Some("History search: Enter accepts, Esc restores".to_string());
            self.needs_redraw = true;
        }
    }

    pub fn history_search_backspace(&mut self) {
        if let Some(search) = self.composer_history_search.as_mut() {
            search.query.pop();
            search.selected = 0;
            self.needs_redraw = true;
        }
        self.clamp_history_search_selection();
    }

    pub fn history_search_select_previous(&mut self) {
        if let Some(search) = self.composer_history_search.as_mut() {
            search.selected = search.selected.saturating_sub(1);
            self.needs_redraw = true;
        }
    }

    pub fn history_search_select_next(&mut self) {
        let Some(search) = self.composer_history_search.as_ref() else {
            return;
        };
        let query = search.query.clone();
        let selected = search.selected;
        let match_count = self.history_search_matches_for_query(&query).len();
        if let Some(search) = self.composer_history_search.as_mut()
            && match_count > 0
        {
            search.selected = (selected + 1).min(match_count.saturating_sub(1));
            self.needs_redraw = true;
        }
    }

    pub fn accept_history_search(&mut self) -> bool {
        let Some(search) = self.composer_history_search.take() else {
            return false;
        };
        let matches = self.history_search_matches_for_query(&search.query);
        if let Some(selected) = matches
            .get(search.selected.min(matches.len().saturating_sub(1)))
            .cloned()
        {
            self.input = selected;
            self.resync_command_line_claim();
            self.cursor_position = char_count(&self.input);
            self.history_index = None;
            self.status_message = Some("History match inserted into composer".to_string());
            self.needs_redraw = true;
            true
        } else {
            self.composer_history_search = Some(search);
            self.status_message = Some("No history matches".to_string());
            self.needs_redraw = true;
            false
        }
    }

    pub fn cancel_history_search(&mut self) {
        let Some(search) = self.composer_history_search.take() else {
            return;
        };
        self.input = search.pre_search_input;
        self.resync_command_line_claim();
        self.cursor_position = search.pre_search_cursor.min(char_count(&self.input));
        self.status_message = Some("History search canceled".to_string());
        self.needs_redraw = true;
    }

    /// Refuse a submit the shell cannot vouch for, leaving the text exactly
    /// where the user can see it (#5925).
    ///
    /// Never destructive: the composer keeps its content and the next Enter
    /// sends it. The hold exists so a line whose first bytes may be missing
    /// is read by a human before it is read by a model.
    fn hold_unproven_submit(&mut self, reason: &str) {
        self.status_message = Some(reason.to_string());
        self.push_status_toast(reason, StatusToastLevel::Warning, Some(8_000));
        self.needs_redraw = true;
    }

    pub fn submit_input(&mut self) -> Option<String> {
        if self.input.trim().is_empty() {
            self.paste_burst.clear_after_explicit_paste();
            return None;
        }
        // #5925: startup consumed bytes it could not replay, so this shell
        // cannot prove it saw the whole line. Keep the text in the composer
        // and let the user look at it rather than sending a line that may be
        // missing its first characters — a truncated `/command` submitted as
        // prose runs with the session's full authority. Cleared here, so the
        // deliberate second Enter sends exactly what is on screen.
        if self.startup_input_unproven {
            self.startup_input_unproven = false;
            self.hold_unproven_submit(
                "Check the line, then press Enter again to send it. \
                 Startup may have missed some characters.",
            );
            return None;
        }
        // A line that began with `/` stays a command until Enter. If the
        // outgoing text is no longer a command — a submit-time rewrite, or
        // bytes lost after the composer accepted them — never fall through
        // to the prose-prompt branch; hold it instead.
        let claimed_command = self.command_line_claimed();
        // A dropped image path that was not converted at paste time (typed
        // keys, or the question written on the same line) still attaches.
        if !claimed_command
            && !self.input.trim_start().starts_with('!')
            && let Some((path, rest)) =
                codewhale_engine::image_attach::leading_dropped_image(&self.input)
        {
            let reference = media_attachment_reference("image", &path, None);
            self.input = if rest.is_empty() {
                reference
            } else {
                format!("{reference}\n{rest}")
            };
            self.cursor_position = char_count(&self.input);
        }
        // Safety net: if any earlier path filled the buffer above the
        // safety cap without going through `insert_paste_text`, fold it
        // into a workspace paste file now (#553). Bracketed pastes hit
        // the consolidation in `insert_paste_text` first, so the user
        // sees the @mention in the composer before submission.
        if !self.consolidate_large_input_if_oversized() {
            // The paste file could not be written. Never send a silently
            // truncated prompt: the full text stays in the composer and the
            // error explains why Enter did nothing.
            return None;
        }
        // If consolidation created a paste file, submit only the @-mention so
        // the model reads the full content from the paste file. Sending both
        // the inline text and the file mention duplicates the content in the
        // request and confuses the model.
        let mut input = self.input.clone();
        if let Some(reference) = self.pending_paste_reference.take() {
            // Drop the oversized inline copy; the paste file is now the
            // single source of truth for this content. The submitted text
            // keeps the @-mention (mention resolution attaches the file for
            // the model) but wraps it in a human-readable attachment card
            // with size and a bounded preview, so the transcript row can
            // never render as a mysterious bare filesystem path (#553
            // follow-up: "a path is not a message").
            let full = self.oversized_paste_full_text.take();
            input = match full {
                Some(full) => paste_attachment_display(&reference, &full),
                None => reference,
            };
        } else if let Some(full) = self.oversized_paste_full_text.take() {
            input = full;
        }
        if claimed_command && !looks_like_slash_command_input(&input) {
            // The line was a command when the composer accepted it and is
            // not one now. Something rewrote it between Enter and dispatch;
            // the one thing that must never happen is sending it to the
            // model as prose (#5925). Put the text back and say so.
            tracing::warn!(
                target: "startup_input",
                submitted = %input,
                "a command line stopped looking like a command before dispatch; holding it in the composer"
            );
            self.input = input;
            self.cursor_position = char_count(&self.input);
            self.line_began_with_slash = false;
            self.hold_unproven_submit(
                "That line started as a command but no longer reads as one. \
                 Check it and press Enter again to send it as shown.",
            );
            return None;
        }
        crate::composer_history::push_history_entry(&mut self.input_history, &input);
        if self.max_input_history == 0 {
            self.input_history.clear();
        } else if self.input_history.len() > self.max_input_history {
            let excess = self.input_history.len() - self.max_input_history;
            self.input_history.drain(0..excess);
        }
        // Mirror prompts and commands to the persisted cross-session history
        // so arrow-up recall works across restarts (#366, #6006). A history
        // limit of zero saves nothing (U01-m2).
        if self.max_input_history > 0 {
            crate::composer_history::append_history(&input);
        }
        self.history_index = None;
        self.history_navigation_draft = None;
        self.clear_input();
        // Collapse recent-only Work chrome on the next accepted turn (#4688).
        self.work_surface.note_user_turn_or_new_operation();
        Some(input)
    }

    /// Restore unsent user input to the composer; keep command-generated prompts
    /// as receipts that require invoking their original command again.
    pub fn restore_unsent_message(&mut self, message: QueuedMessage) {
        if let Some((command, _)) = message.extension_command.as_ref() {
            self.add_message(crate::tui::history::HistoryCell::System {
                content: format!(
                    "{} command prompt was not submitted. Run the command again to retry.\n{}",
                    command.origin, message.display
                ),
            });
            self.needs_redraw = true;
            return;
        }
        self.input = message.display;
        self.resync_command_line_claim();
        self.cursor_position = char_count(&self.input);
        self.history_index = None;
        self.history_navigation_draft = None;
        self.selected_attachment_index = None;
        self.active_skill = message.skill_instruction;
        self.active_skill_provenance = message.skill_provenance;
        self.needs_redraw = true;
    }

    pub fn restore_last_submitted_prompt_if_empty(&mut self) -> bool {
        if !self.input.is_empty() {
            return false;
        }
        let Some(prompt) = self
            .last_submitted_prompt
            .as_deref()
            .filter(|prompt| !prompt.is_empty())
        else {
            return false;
        };

        self.input = prompt.to_string();
        self.resync_command_line_claim();
        self.cursor_position = char_count(&self.input);
        self.history_index = None;
        self.history_navigation_draft = None;
        self.selected_attachment_index = None;
        self.needs_redraw = true;
        true
    }

    /// Replace the composer buffer with externally edited text. Recalled
    /// history, selection, and attachment positions belong to the old text:
    /// a stale `history_index` would let the next Up/Down silently discard
    /// the edited buffer.
    pub fn apply_external_edit(&mut self, new: String) {
        self.input = new;
        self.resync_command_line_claim();
        self.pending_paste_reference = None;
        self.oversized_paste_full_text = None;
        self.cursor_position = char_count(&self.input);
        self.history_index = None;
        self.history_navigation_draft = None;
        self.selection_anchor = None;
        self.selected_attachment_index = None;
        self.needs_redraw = true;
    }

    /// Restore the last cleared input if the composer is empty.
    /// Returns `true` if the input was restored.
    pub fn restore_last_cleared_input_if_empty(&mut self) -> bool {
        if !self.input.is_empty() {
            return false;
        }
        let Some(saved) = self.clear_undo_buffer.take().filter(|s| !s.is_empty()) else {
            return false;
        };

        self.input = saved;
        self.resync_command_line_claim();
        self.cursor_position = char_count(&self.input);
        self.history_index = None;
        self.history_navigation_draft = None;
        self.selected_attachment_index = None;
        self.slash_menu_selected = 0;
        self.slash_menu_hidden = false;
        self.needs_redraw = true;
        self.clear_undo_buffer = None;
        true
    }

    /// Composer dispatch with paste protection for bare Enter. Returns `Some(input)` when the press should
    /// fire a submit; `None` when Enter was absorbed (paste-burst Enter
    /// suppression — see #1073).
    ///
    /// Two suppression cases are handled here. Both are silent: nothing
    /// visible happens beyond the text gaining a newline.
    ///
    /// 1. **Burst active.** A paste burst is currently being assembled in
    ///    `paste_burst.buffer`. The Enter is part of the paste content;
    ///    append `\n` to the buffer so the next flush includes it, do not
    ///    submit, and extend the suppression window so a follow-on Enter
    ///    (i.e. the *next* line of a multi-line paste) is also absorbed.
    /// 2. **Window open after flush.** A burst just flushed into
    ///    `self.input`, but the suppression window is still alive. The
    ///    Enter is probably the trailing newline of that paste, not a submit
    ///    gesture by the user, so insert `\n` directly into the composer
    ///    text. The window is deliberately *not* re-armed here: no burst is
    ///    being assembled, so this Enter is only a guess, and re-arming on a
    ///    guess meant every absorbed Enter bought another 120ms — a user
    ///    pressing Enter to send just kept adding newlines and never
    ///    submitted. Suppression now always ends 120ms after the last real
    ///    keystroke.
    ///
    /// Outside both cases the call falls through to [`Self::submit_input`]
    /// unchanged so normal Enter-to-send behaviour is preserved.
    pub fn handle_composer_enter(&mut self, protect_paste: bool) -> Option<String> {
        if protect_paste && self.use_paste_burst_detection {
            let now = Instant::now();
            if self
                .paste_burst
                .newline_should_insert_instead_of_submit(now)
            {
                if !self.paste_burst.append_newline_if_active(now) {
                    self.insert_char('\n');
                }
                self.needs_redraw = true;
                return None;
            }
        }
        self.submit_input()
    }

    /// Non-destructive twin of [`Self::handle_composer_enter`] for callers
    /// that must commit other state before the composer may be consumed:
    /// the startup composer begins the launch session before it consumes
    /// its draft, so it probes first and never begins a session for an
    /// Enter the paste-burst window is about to absorb. Reads the exact
    /// same two predicates `handle_composer_enter` acts on — the burst
    /// window and the trimmed-empty buffer — without mutating anything.
    #[must_use]
    pub fn composer_enter_would_submit(&self, protect_paste: bool) -> bool {
        if protect_paste
            && self.use_paste_burst_detection
            && self
                .paste_burst
                .newline_should_insert_instead_of_submit(Instant::now())
        {
            return false;
        }
        !self.input.trim().is_empty()
    }

    /// Whether the *draft* is in a submittable state, for display only.
    ///
    /// Deliberately time-independent. [`Self::composer_enter_would_submit`]
    /// additionally consults the paste-burst heuristic, whose suppression
    /// window is re-extended on every fast keystroke -- correct for deciding
    /// what a newline does mid-paste, wrong for a persistent affordance.
    /// Driving the `[↵]` chip from it made the chip strobe `[↵]`/`[·]` for as
    /// long as the user kept typing, because the window kept reopening
    /// (#6397). Enter routing, mouse submit and hover registration stay on
    /// the timing predicate.
    #[must_use]
    pub fn composer_draft_is_submittable(&self) -> bool {
        !self.input.trim().is_empty()
    }

    /// Public wrapper around [`Self::consolidate_large_input`] that no-ops
    /// when the current input fits inside the safety cap. Both the paste-
    /// insert path (visible-before-submit) and the submit-time safety net
    /// route through here, so the cap is enforced exactly once even when
    /// both paths fire on the same buffer.
    ///
    /// Returns `false` when the input is oversized and could not be backed
    /// up to a paste file; the composer then still holds the full text.
    pub(crate) fn consolidate_large_input_if_oversized(&mut self) -> bool {
        if char_count(&self.input) > MAX_SUBMITTED_INPUT_CHARS {
            return self.consolidate_large_input();
        }
        true
    }

    /// When the composer input exceeds [`MAX_SUBMITTED_INPUT_CHARS`], write
    /// the full content to a timestamped paste file under
    /// `.codewhale/pastes/` and replace `self.input` with an `@`-mention
    /// pointing at it so the model can read the full content via the
    /// normal file-mention resolution path (#553).
    ///
    /// Returns `false` without touching the composer when the paste file
    /// cannot be written, so the caller holds the submit instead of sending
    /// a truncated prompt.
    fn consolidate_large_input(&mut self) -> bool {
        let now = chrono::Local::now();
        let suffix = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let filename = format!("paste-{}-{}.md", now.format("%Y-%m-%d-%H%M%S"), suffix);
        let rel_path = format!(".codewhale/pastes/{filename}");

        // Confined to the workspace: a linked `.codewhale` or `pastes`
        // directory must not send the pasted text somewhere else.
        let file_path = self.workspace.join(&rel_path);
        let written =
            codewhale_util::fs_confined::write(&self.workspace, &file_path, self.input.as_bytes());
        if let Err(error) = written {
            let limit = MAX_SUBMITTED_INPUT_CHARS.to_string();
            // The toast row sheds clauses to fit and left only "Not sent", so
            // the toast carries a short form and the full reason, including
            // the write error, goes to the transcript where nothing is clipped.
            let short = self
                .tr(MessageId::ComposerOversizedSubmitHeldShort)
                .replace("{limit}", &limit);
            let reason = self
                .tr(MessageId::ComposerOversizedSubmitHeld)
                .replace("{limit}", &limit)
                .replace("{error}", &error.to_string());
            self.push_status_toast(short, StatusToastLevel::Error, Some(8_000));
            // Enter and paste both retry this; do not stack identical cells.
            let already_noted = matches!(
                self.history.last(),
                Some(HistoryCell::System { content }) if *content == reason
            );
            if !already_noted {
                self.add_message(HistoryCell::System { content: reason });
            }
            self.needs_redraw = true;
            return false;
        }
        let full_input = std::mem::take(&mut self.input);

        // Keep a truncated preview in the composer so the user can still
        // select, copy, and edit it. The full text is written to the paste
        // file; at submit time the inline text is replaced by the @mention
        // so the model reads the file instead of receiving the content twice.
        self.pending_paste_reference = Some(format!("@{rel_path}"));
        self.oversized_paste_full_text = Some(full_input.clone());
        let display_chars = char_count(&full_input).min(MAX_COMPOSER_DISPLAY_CHARS);
        let mut truncated: String = full_input.chars().take(display_chars).collect();
        if char_count(&full_input) > MAX_COMPOSER_DISPLAY_CHARS {
            truncated.push_str("\n\n---\n(content truncated for display — start typing to expand; full text sent to model)");
        }
        self.input = truncated;
        self.resync_command_line_claim();
        self.cursor_position = 0;
        self.push_status_toast(
            "Large paste backed up to file — the model will receive the full content.",
            StatusToastLevel::Info,
            Some(5_000),
        );
        true
    }

    pub fn history_down(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        match self.history_index {
            None => {}
            Some(i) => {
                if i + 1 < self.input_history.len() {
                    self.history_index = Some(i + 1);
                    self.input = self.input_history[i + 1].clone();
                    self.resync_command_line_claim();
                    self.cursor_position = char_count(&self.input);
                    self.selection_anchor = None;
                    self.selected_attachment_index = None;
                    self.slash_menu_hidden = false;
                    self.paste_burst.clear_after_explicit_paste();
                } else {
                    self.history_index = None;
                    if let Some(draft) = self.history_navigation_draft.take() {
                        self.input = draft.input;
                        self.resync_command_line_claim();
                        self.cursor_position = draft.cursor.min(char_count(&self.input));
                        self.selection_anchor = None;
                        self.selected_attachment_index = None;
                        self.slash_menu_hidden = false;
                        self.paste_burst.clear_after_explicit_paste();
                        self.needs_redraw = true;
                    } else {
                        self.clear_input();
                    }
                }
            }
        }
    }
}
