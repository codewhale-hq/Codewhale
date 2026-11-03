//! LaTeX math expression rendering for the TUI transcript.
//! Renders `$...$` (inline) and `$$...$$` (display) math expressions using
//! Unicode approximations for terminal display.
use std::collections::HashMap;
use std::sync::OnceLock;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn is_escaped(bytes: &[u8], idx: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = idx;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}
/// Private: find the start of math delimiter ($, $$, \(, \[) in text.
fn find_math_start(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    for (idx, &byte) in b.iter().enumerate() {
        if byte == b'$' && !is_escaped(b, idx) {
            return Some(idx);
        }
        if byte == b'\\'
            && !is_escaped(b, idx)
            && idx + 1 < b.len()
            && (b[idx + 1] == b'(' || b[idx + 1] == b'[')
        {
            return Some(idx);
        }
    }
    None
}
/// Private: if text starts with a math delimiter, find closing delimiter and return (end_pos, is_display).
fn find_math_end(text: &str) -> Option<(usize, bool)> {
    let b = text.as_bytes();
    if b.starts_with(b"$$") {
        for i in 2..b.len().saturating_sub(1) {
            if b[i] == b'{' {
                continue;
            }
            if b[i..].starts_with(b"$$") && !is_escaped(b, i) {
                return Some((i, true));
            }
        }
    } else if b.starts_with(b"$") && !b.starts_with(b"$$") {
        if b.get(1).is_some_and(u8::is_ascii_whitespace) {
            return None;
        }
        for (j, &byte) in b[1..].iter().enumerate() {
            if byte == b'{' {
                continue;
            }
            if byte == b'\n' {
                return None;
            }
            let i = j + 1;
            if byte != b'$' || is_escaped(b, i) {
                continue;
            }
            let prev = text[..i].chars().next_back().unwrap_or(' ');
            let next = b.get(i + 1).copied().unwrap_or(b' ');
            if next == b'{'
                || (next.is_ascii_alphanumeric()
                    && !prev.is_ascii_alphanumeric()
                    && !matches!(prev, ')' | ']' | '}')
                    && (prev.is_ascii() || prev.width() == Some(2)))
            {
                return None;
            }
            if !prev.is_ascii_whitespace() && !next.is_ascii_digit() {
                return Some((i, false));
            }
        }
    } else if b.starts_with(b"\\[") {
        for i in 2..b.len().saturating_sub(1) {
            if b[i] == b'{' {
                continue;
            }
            if b[i..].starts_with(b"\\]") && !is_escaped(b, i) {
                return Some((i, true));
            }
        }
    } else if b.starts_with(b"\\(") {
        for i in 2..b.len().saturating_sub(1) {
            if b[i] == b'{' {
                continue;
            }
            if b[i..].starts_with(b"\\)") && !is_escaped(b, i) {
                return Some((i, false));
            }
        }
    }
    None
}
fn math_delim_offset(text: &str) -> usize {
    let b = text.as_bytes();
    if b.starts_with(b"$$") || b.starts_with(b"\\[") || b.starts_with(b"\\(") {
        2
    } else {
        1
    }
}
fn render_math_segment(text: &str) -> String {
    let mut result = String::new();
    let mut i = 0;
    while i < text.len() {
        let remaining = &text[i..];
        if let Some((end, _is_display)) = find_math_end(remaining) {
            let offset = math_delim_offset(remaining);
            let inner = &remaining[offset..end];
            result.push_str(&render_latex_to_string(inner));
            let close_len: usize = if remaining[end..].starts_with("\\]")
                || remaining[end..].starts_with("$$")
                || remaining[end..].starts_with("\\)")
            {
                2
            } else if remaining.as_bytes().get(end..end + 1) == Some(b"$") {
                1
            } else {
                0
            };
            i += end + close_len;
        } else {
            let skip = find_math_start(remaining).unwrap_or(remaining.len());
            if skip == 0 {
                // Unmatched opening delimiter ($, $$, \(, \[) during streaming:
                // push it as plain text so the loop can advance.
                result.push(remaining.chars().next().unwrap_or('$'));
                i += remaining.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            } else {
                result.push_str(&remaining[..skip]);
                i += skip;
                if skip == remaining.len() {
                    break;
                }
            }
        }
    }
    result
}

/// Replace math delimiters with plain Unicode while preserving Markdown code.
///
/// Fast path (#perf-r5): the overwhelming majority of streamed content
/// contains no math delimiters at all. A single byte scan for the three
/// opening delimiters (`$`, `\(`, `\[`) decides between borrowing the input
/// untouched and running the full transform, so the per-chunk streaming
/// render avoids allocating a full-content copy on every update when no
/// math is present.
pub fn render_latex_in_text(text: &str) -> std::borrow::Cow<'_, str> {
    // Math can only start at '$' (incl. '$$') or the two-byte '\(' and '\['.
    // Scanning bytes directly avoids a regex; any hit falls back to the
    // full transform below, which re-verifies delimiters precisely.
    let has_delim = text
        .as_bytes()
        .iter()
        .enumerate()
        .any(|(idx, &byte)| match byte {
            b'$' => true,
            b'\\' => matches!(text.as_bytes().get(idx + 1), Some(b'(') | Some(b'[')),
            _ => false,
        });
    if !has_delim {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;

    while cursor < text.len() {
        let Some(tick_offset) = text[cursor..].find('`') else {
            result.push_str(&render_math_segment(&text[cursor..]));
            break;
        };
        let tick_start = cursor + tick_offset;
        result.push_str(&render_math_segment(&text[cursor..tick_start]));

        let tick_count = text[tick_start..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        let delimiter = "`".repeat(tick_count);
        let content_start = tick_start + tick_count;
        if let Some(close_offset) = text[content_start..].find(&delimiter) {
            let code_end = content_start + close_offset + tick_count;
            result.push_str(&text[tick_start..code_end]);
            cursor = code_end;
        } else {
            result.push_str(&text[tick_start..]);
            break;
        }
    }

    std::borrow::Cow::Owned(result)
}

// --- Environment rendering ---

/// Render a `\begin{name}...\end{name}` block.
/// `content` is everything between the braces.
fn render_environment(env_name: &str, content: &str) -> String {
    match env_name {
        "aligned" | "align" | "gather" | "eqnarray" | "split" => render_aligned(content),
        "pmatrix" | "bmatrix" | "vmatrix" | "Bmatrix" | "matrix" | "smallmatrix" => {
            render_matrix(env_name, content)
        }
        "array" => render_array(content),
        "cases" | "dcases" => render_cases(content, false),
        "rcases" | "drcases" => render_cases(content, true),
        _ => {
            // Unknown environment: pass through raw
            format!("\\begin{{{env_name}}}{content}\\end{{{env_name}}}")
        }
    }
}

/// Split a multi-row env content into rows (`\\` separator), each rendered.
/// `row_fn` is called for each parsed row (list of cell strings).
fn parse_rows<F>(content: &str, mut row_fn: F)
where
    F: FnMut(Vec<String>),
{
    // Split by \\ (but be careful: \\\\ is an escaped backslash, not a line break)
    let mut current = String::new();
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if chars.peek() == Some(&'\\') {
                // Line break marker
                chars.next(); // consume second \
                // If followed by optional whitespace and an optional * (\\*)
                while matches!(chars.peek(), Some(&' ') | Some(&'\t')) {
                    chars.next();
                }
                if chars.peek() == Some(&'*') {
                    chars.next();
                }
                row_fn(parse_row_cells(&current));
                current.clear();
            } else if chars.peek() == Some(&'[') || chars.peek() == Some(&'{') {
                // --- Spacing ---
                // --- Spacing ---
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        if c == ']' {
                            chars.next();
                            break;
                        }
                        chars.next();
                    }
                } else if chars.peek() == Some(&'{') {
                    let _ = read_braced_chars(&mut chars);
                }
                row_fn(parse_row_cells(&current));
                current.clear();
            } else {
                current.push('\\');
            }
        } else if ch == '\n' {
            // Newlines in environments often act as row separators
            // but not inside braces
            // Simple approach: treat bare \n as space
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
        } else {
            current.push(ch);
        }
    }
    // Last row
    row_fn(parse_row_cells(&current));
}

/// Split a single row into cells by `&`.
fn parse_row_cells(row: &str) -> Vec<String> {
    // Split by & but skip escaped \&
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = row.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            cells.push(current.trim().to_string());
            current.clear();
        } else if ch == '\\' && chars.peek() == Some(&'&') {
            // Escaped ampersand
            current.push('&');
            chars.next();
        } else {
            current.push(ch);
        }
    }
    cells.push(current.trim().to_string());
    cells
}

/// Aligned equations: align at `&` markers.
fn render_aligned(content: &str) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    parse_rows(content, |cells| rows.push(cells));

    if rows.is_empty() {
        return String::new();
    }

    // Determine max columns
    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    // Double-pass: render each cell and measure widths
    let mut rendered: Vec<Vec<String>> = Vec::new();
    let mut col_widths: Vec<usize> = vec![0; max_cols];

    for row in &rows {
        let mut rendered_row = Vec::new();
        for (ci, cell) in row.iter().enumerate() {
            let rendered_cell = render_latex_to_string(cell);
            let w = UnicodeWidthStr::width(rendered_cell.as_str());
            if ci < max_cols && w > col_widths[ci] {
                col_widths[ci] = w;
            }
            rendered_row.push(rendered_cell);
        }
        // Pad missing cells
        while rendered_row.len() < max_cols {
            rendered_row.push(String::new());
        }
        rendered.push(rendered_row);
    }

    // Second pass: assemble with padding
    let mut result = String::new();
    for (ri, row) in rendered.iter().enumerate() {
        if ri > 0 {
            result.push('\n');
        }
        for ci in 0..max_cols {
            if ci > 0 {
                let pad =
                    col_widths[ci - 1].saturating_sub(UnicodeWidthStr::width(row[ci - 1].as_str()));
                for _ in 0..pad {
                    result.push(' ');
                }
                result.push_str("  ");
            }
            result.push_str(&row[ci]);
        }
    }

    result
}

/// --- Brackets ---
fn render_matrix(env_name: &str, content: &str) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    parse_rows(content, |cells| rows.push(cells));

    if rows.is_empty() {
        return String::new();
    }

    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    // Two-pass: render + measure
    let mut rendered: Vec<Vec<String>> = Vec::new();
    let mut col_widths: Vec<usize> = vec![0; max_cols];

    for row in &rows {
        let mut rendered_row = Vec::new();
        for (ci, cell) in row.iter().enumerate() {
            let rendered_cell = render_latex_to_string(cell);
            let w = UnicodeWidthStr::width(rendered_cell.as_str());
            if ci < max_cols && w > col_widths[ci] {
                col_widths[ci] = w;
            }
            rendered_row.push(rendered_cell);
        }
        while rendered_row.len() < max_cols {
            rendered_row.push(String::new());
        }
        rendered.push(rendered_row);
    }

    // Build each row with proper padding
    let mut cell_strings: Vec<String> = Vec::new();
    for row in &rendered {
        let mut line = String::new();
        for ci in 0..max_cols {
            if ci > 0 {
                line.push(' ');
            }
            let cell = &row[ci];
            line.push_str(cell);
            let pad = col_widths[ci].saturating_sub(UnicodeWidthStr::width(cell.as_str()));
            for _ in 0..pad {
                line.push(' ');
            }
        }
        cell_strings.push(line);
    }

    match env_name {
        "pmatrix" => surround_with("(", ")", &cell_strings, 1),
        "bmatrix" => surround_with("[", "]", &cell_strings, 1),
        "vmatrix" => surround_with("\u{2502}", "\u{2502}", &cell_strings, 1),
        "Bmatrix" => surround_with("{", "}", &cell_strings, 1),
        "smallmatrix" => surround_with("(", ")", &cell_strings, 0),
        _ => {
            // --- Brackets ---
            let mut result = String::new();
            for (ri, s) in cell_strings.iter().enumerate() {
                if ri > 0 {
                    result.push('\n');
                }
                result.push_str(s);
            }
            result
        }
    }
}

/// --- Brackets ---
fn surround_with(left: &str, right: &str, rows: &[String], pad: usize) -> String {
    if rows.is_empty() {
        return format!("{left}{right}");
    }
    let mut result = String::new();
    if rows.len() == 1 {
        result.push_str(left);
        for _ in 0..pad {
            result.push(' ');
        }
        result.push_str(&rows[0]);
        for _ in 0..pad {
            result.push(' ');
        }
        result.push_str(right);
        return result;
    }
    // Multi-row: brackets on their own lines
    result.push_str(left);
    result.push('\n');
    for (ri, s) in rows.iter().enumerate() {
        if ri > 0 {
            result.push('\n');
        }
        for _ in 0..pad {
            result.push(' ');
        }
        result.push_str(s);
    }
    result.push('\n');
    result.push_str(right);
    result
}

/// Array environment: parse column spec and render table with vertical bars.
fn render_array(content: &str) -> String {
    let trimmed = content.trim_start();
    let (col_spec, body) = if let Some(after_brace) = trimmed.strip_prefix('{') {
        let close = after_brace.find('}').map(|i| i + 1).unwrap_or(0);
        if close > 0 {
            (&after_brace[..close], after_brace[close..].trim_start())
        } else {
            ("", trimmed)
        }
    } else {
        ("", trimmed)
    };
    let mut has_vline_start = false;
    let mut vlines = Vec::new();
    let mut cols = Vec::new();
    for ch in col_spec.chars() {
        match ch {
            'c' | 'l' | 'r' => cols.push(ch),
            '|' => {
                if cols.is_empty() {
                    has_vline_start = true;
                } else {
                    vlines.push(cols.len());
                }
            }
            _ => {}
        }
    }
    let n = cols.len();
    if n == 0 {
        return body.to_string();
    }
    let mut rows = Vec::new();
    parse_rows(body, |cells| rows.push(cells));
    let mut result = String::new();
    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 {
            result.push('\n');
        }
        if has_vline_start {
            result.push_str("| ");
        }
        for ci in 0..n {
            if ci > 0 {
                result.push(if vlines.contains(&ci) { '|' } else { ' ' });
                result.push(' ');
            }
            result.push_str(&render_latex_to_string(
                row.get(ci).unwrap_or(&String::new()),
            ));
        }
        if vlines.contains(&n) || has_vline_start {
            result.push_str(" |");
        }
    }
    result
}

/// Piecewise functions with cases environment.
fn render_cases(content: &str, right_brace: bool) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    parse_rows(content, |cells| rows.push(cells));

    if rows.is_empty() {
        return String::new();
    }

    let mut rendered_rows: Vec<(String, Option<String>)> = Vec::new();
    let mut left_width = 0;

    for cells in &rows {
        let left = render_latex_to_string(cells.first().map(|s| s.as_str()).unwrap_or(""));
        let left_w = UnicodeWidthStr::width(left.as_str());
        if left_w > left_width {
            left_width = left_w;
        }
        let right = if cells.len() > 1 {
            let r = render_latex_to_string(&cells[1]);
            Some(r)
        } else {
            None
        };
        rendered_rows.push((left, right));
    }

    let n = rendered_rows.len();
    let mut result = String::new();
    for (ri, (left, right)) in rendered_rows.iter().enumerate() {
        if ri > 0 {
            result.push('\n');
        }
        if !right_brace {
            result.push_str(match ri {
                0 => "\u{23a7} ",
                _ if ri == n - 1 => "\u{23a9} ",
                _ => "\u{23a8} ",
            });
        }

        // Left part + padding
        let left_pad = left_width.saturating_sub(UnicodeWidthStr::width(left.as_str()));
        result.push_str(left);
        for _ in 0..left_pad {
            result.push(' ');
        }

        if let Some(cond) = right {
            result.push_str(",  ");
            result.push_str(cond);
        }
    }
    result
}

// --- Helper: read_braced for chars iterator ---

fn read_braced_chars(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    let mut depth: u32 = 0;
    if chars.next_if_eq(&'{').is_some() {
        depth = 1;
    }
    while let Some(&c) = chars.peek() {
        match c {
            '{' => {
                depth += 1;
                s.push(c);
                chars.next();
            }
            '}' => {
                depth = depth.saturating_sub(1);
                chars.next();
                if depth == 0 {
                    break;
                }
                s.push('}');
            }
            _ => {
                s.push(c);
                chars.next();
            }
        }
    }
    s
}

// --- Styled symbols ---

fn render_styled_symbol(
    command: &str,
    chars: &mut std::iter::Peekable<std::str::Chars>,
    out: &mut String,
) {
    let argument = read_braced_chars(chars);
    let rendered = match (command, argument.as_str()) {
        ("mathbb", "R") => Some("\u{211d}"),
        ("mathbb", "C") => Some("\u{2102}"),
        ("mathbb", "N") => Some("\u{2115}"),
        ("mathbb", "Q") => Some("\u{211a}"),
        ("mathbb", "Z") => Some("\u{2124}"),
        ("mathbb", "P") => Some("\u{2119}"),
        ("mathbb", "H") => Some("\u{210d}"),
        ("mathbb", "F") => Some("\u{1d53b}"),
        ("mathcal", "L") => Some("\u{2112}"),
        ("mathcal", "H") => Some("\u{210b}"),
        ("mathcal", "R") => Some("\u{211b}"),
        ("mathcal", "A") => Some("\u{1d49c}"),
        ("mathcal", "B") => Some("\u{212c}"),
        ("mathcal", "C") => Some("\u{212d}"),
        ("mathcal", "D") => Some("\u{1d49f}"),
        ("mathcal", "E") => Some("\u{2130}"),
        ("mathcal", "F") => Some("\u{2131}"),
        ("mathcal", "I") => Some("\u{2110}"),
        ("mathcal", "M") => Some("\u{2133}"),
        ("mathcal", "O") => Some("\u{1d4aa}"),
        ("mathcal", "P") => Some("\u{1d4ab}"),
        ("mathcal", "S") => Some("\u{1d4ae}"),
        ("mathcal", "T") => Some("\u{1d4af}"),
        ("mathcal", "Z") => Some("\u{2128}"),
        _ => None,
    };
    if let Some(symbol) = rendered {
        out.push_str(symbol);
    } else {
        out.push('\\');
        out.push_str(command);
        out.push('{');
        out.push_str(&argument);
        out.push('}');
    }
}

// --- Main render function ---

/// Read a braced group `{...}` from an index-based cursor in a string.
/// Returns (content_string, new_cursor_position).
fn read_braced_at(input: &str, start: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    let mut pos = start;
    if pos >= input.len() || bytes[pos] != b'{' {
        return None;
    }
    pos += 1; // skip {
    let mut depth: u32 = 1;
    let mut content = String::new();
    while pos < input.len() {
        let ch = input[pos..].chars().next()?;
        let byte_len = ch.len_utf8();
        match ch {
            '{' => {
                depth += 1;
                if depth > 1 {
                    content.push('{');
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((content, pos + 1));
                }
                content.push('}');
            }
            _ => content.push(ch),
        }
        pos += byte_len;
    }
    None
}

/// Main LaTeX-to-Unicode rendering.
fn render_latex_to_string(latex: &str) -> String {
    let input = latex.trim();
    let mut out = String::new();
    let mut pos = 0;

    while pos < input.len() {
        let remaining = &input[pos..];

        // 1. Environment detection: \begin{name}...\end{name}
        if let Some(env_rendered) = try_render_env(remaining) {
            let (rendered, consumed) = env_rendered;
            // Start multi-line envs on a fresh line for alignment
            let needs_newline = rendered.starts_with('\u{23a7}') // cases ??
                || rendered.starts_with('(')                     // pmatrix
                || rendered.starts_with('[')                     // bmatrix
                || rendered.starts_with('\u{2502}'); // vmatrix
            if needs_newline && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&rendered);
            pos += consumed;
            continue;
        }

        let ch = remaining.chars().next().unwrap();
        let ch_len = ch.len_utf8();

        match ch {
            '\\' => {
                // Read command name
                let rest = &input[pos + 1..];
                let cmd_end = rest
                    .find(|c: char| !c.is_ascii_alphabetic())
                    .unwrap_or(rest.len());
                let cmd = &rest[..cmd_end];
                let _after_cmd = cmd_end;

                if cmd.is_empty() {
                    // Escape sequence for special chars
                    if let Some(&next) = rest.as_bytes().first() {
                        match next {
                            b'{' | b'}' | b'$' | b'%' | b'#' | b'&' | b'_' | b' ' => {
                                // Consume the escape
                                let skip = 1 + 1; // \ + char
                                out.push(next as char);
                                pos += skip;
                                continue;
                            }
                            _ => {
                                // Unknown single-char escape 闁?output the char
                                let char_len =
                                    rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                                out.push(rest.chars().next().unwrap_or(ch));
                                pos += 1 + char_len;
                                continue;
                            }
                        }
                    }
                    pos += 1;
                    continue;
                }

                let cmd_len = cmd.len();
                let _total_cmd_start = pos;
                let total_cmd_end = pos + 1 + cmd_len; // \ + name

                match cmd {
                    // --- Environments (handled above, just in case) ---
                    "begin" => {
                        // Should have been caught by try_render_env above.
                        // Fallback: try inline parsing
                        if let Some((env_name, after_name)) = read_braced_at(input, total_cmd_end) {
                            let end_tag = format!("\\end{{{env_name}}}");
                            let search_from = &input[after_name..];
                            if let Some(end_rel) = search_from.find(&end_tag) {
                                let env_content = &search_from[..end_rel];
                                let rendered = render_environment(&env_name, env_content);
                                out.push_str(&rendered);
                                pos = after_name + end_rel + end_tag.len();
                                continue;
                            }
                            out.push_str(&format!("\\begin{{{env_name}}}"));
                        }
                        out.push_str("\\begin");
                        pos = total_cmd_end;
                    }
                    "end" => {
                        // Shouldn't be reached; output raw.
                        let after_end = pos + 4;
                        if let Some((env_name, after_name)) = read_braced_at(input, after_end) {
                            out.push_str(&format!("\\end{{{env_name}}}"));
                            pos = after_name;
                        } else {
                            out.push_str("\\end");
                            pos = after_end;
                        }
                    }
                    // --- Text ---
                    "text" | "mathrm" | "mathit" | "mathsf" | "textrm" | "textit" | "textbf" => {
                        if let Some((arg, new_pos)) = read_braced_at(input, total_cmd_end) {
                            out.push_str(&arg);
                            pos = new_pos;
                        } else {
                            // No braces, try single char
                            let next = &input[total_cmd_end..].chars().next();
                            if let Some(c) = next {
                                out.push(*c);
                                pos = total_cmd_end + c.len_utf8();
                            } else {
                                out.push_str(cmd);
                                pos = total_cmd_end;
                            }
                        }
                    }
                    // --- Accents ---
                    "hat" | "bar" | "tilde" | "dot" | "ddot" | "vec" | "breve" | "check"
                    | "acute" | "grave" => {
                        if let Some((arg, new_pos)) = read_braced_at(input, total_cmd_end) {
                            let rendered = render_latex_to_string(&arg);
                            let accent = match cmd {
                                "hat" => "\u{0302}",
                                "bar" => "\u{0304}",
                                "tilde" => "\u{0303}",
                                "dot" => "\u{0307}",
                                "ddot" => "\u{0308}",
                                "vec" => "\u{20d7}",
                                "breve" => "\u{0306}",
                                "check" => "\u{030c}",
                                "acute" => "\u{0301}",
                                "grave" => "\u{0300}",
                                _ => unreachable!(),
                            };
                            out.push_str(&rendered);
                            out.push_str(accent);
                            pos = new_pos;
                        } else {
                            out.push_str(&format!("\\{cmd}"));
                            pos = total_cmd_end;
                        }
                    }
                    "operatorname" => {
                        if let Some((arg, new_pos)) = read_braced_at(input, total_cmd_end) {
                            out.push_str(&arg);
                            pos = new_pos;
                        } else {
                            out.push_str("\\operatorname");
                            pos = total_cmd_end;
                        }
                    }
                    // --- Underbrace / Overbrace (passthrough) ---
                    "underbrace" | "overbrace" | "underbracket" | "overbracket" => {
                        if let Some((arg, after_arg)) = read_braced_at(input, total_cmd_end) {
                            out.push_str(&render_latex_to_string(&arg));
                            pos = after_arg;
                        } else {
                            out.push_str(&format!("\\{cmd}"));
                            pos = total_cmd_end;
                        }
                    }
                    // --- Vertical/horizontal phantom (no-op) ---
                    "vphantom" | "hphantom" | "phantom" => {
                        if let Some((_, after_arg)) = read_braced_at(input, total_cmd_end) {
                            pos = after_arg;
                        } else {
                            out.push_str(&format!("\\{cmd}"));
                            pos = total_cmd_end;
                        }
                    }
                    // --- Substack ---
                    "substack" => {
                        if let Some((arg, after_arg)) = read_braced_at(input, total_cmd_end) {
                            out.push_str(&arg);
                            pos = after_arg;
                        } else {
                            out.push_str(&format!("\\{cmd}"));
                            pos = total_cmd_end;
                        }
                    }
                    // --- Fonts ---
                    "mathbf" | "bf" => {
                        if let Some((arg, new_pos)) = read_braced_at(input, total_cmd_end) {
                            out.push_str(&render_latex_to_string(&arg));
                            pos = new_pos;
                        } else {
                            let next = &input[total_cmd_end..].chars().next();
                            if let Some(c) = next {
                                out.push(*c);
                                pos = total_cmd_end + c.len_utf8();
                            } else {
                                out.push_str(cmd);
                                pos = total_cmd_end;
                            }
                        }
                    }
                    // --- Brackets ---
                    "left" | "bigl" | "Bigl" | "biggl" | "Biggl" => {
                        // Consume the next token (bracket/pipe/dot) and output it
                        let after = &input[total_cmd_end..];
                        let delimiter = after.trim_start();
                        if let Some(next) = delimiter.chars().next() {
                            let mut skip = after.len() - delimiter.len();
                            if next != '\\' {
                                if next == '.' {
                                    // \left. 闁?invisible delimiter, skip
                                } else {
                                    out.push(next);
                                }
                                skip += next.len_utf8();
                            }
                            pos = total_cmd_end + skip;
                        } else {
                            pos = total_cmd_end;
                        }
                    }
                    "right" | "bigr" | "Bigr" | "biggr" | "Biggr" => {
                        let after = &input[total_cmd_end..];
                        let delimiter = after.trim_start();
                        if let Some(next) = delimiter.chars().next() {
                            let mut skip = after.len() - delimiter.len();
                            if next != '\\' {
                                if next == '.' {
                                    // \right. 闁?invisible delimiter, skip
                                } else {
                                    out.push(next);
                                }
                                skip += next.len_utf8();
                            }
                            pos = total_cmd_end + skip;
                        } else {
                            pos = total_cmd_end;
                        }
                    }
                    "big" | "Big" | "bigg" | "Bigg" => {
                        // Size modifiers 闁?skip them, the next token is what matters
                        let after = &input[total_cmd_end..];
                        let delimiter = after.trim_start();
                        if let Some(next) = delimiter.chars().next() {
                            let mut skip = after.len() - delimiter.len();
                            if next != '\\' {
                                out.push(next);
                                skip += next.len_utf8();
                            }
                            pos = total_cmd_end + skip;
                        } else {
                            pos = total_cmd_end;
                        }
                    }
                    // --- Spacing ---
                    "quad" => {
                        out.push_str("    ");
                        pos = total_cmd_end;
                    }
                    "qquad" => {
                        out.push_str("        ");
                        pos = total_cmd_end;
                    }
                    "," | "thinspace" => {
                        out.push(' ');
                        pos = total_cmd_end;
                    }
                    ";" | "thickspace" => {
                        out.push_str("  ");
                        pos = total_cmd_end;
                    }
                    "!" | "negthinspace" => {
                        // Negative space: just skip
                        pos = total_cmd_end;
                    }
                    ":" | "medspace" => {
                        out.push_str("  ");
                        pos = total_cmd_end;
                    }
                    " " | "space" | "enspace" => {
                        out.push(' ');
                        pos = total_cmd_end;
                    }
                    // --- Styled symbols ---
                    "mathbb" | "mathcal" => {
                        let before = out.len();
                        if let Some((arg, after_arg)) = read_braced_at(input, total_cmd_end) {
                            let mut chars = arg.chars().peekable();
                            render_styled_symbol(cmd, &mut chars, &mut out);
                            if out.len() == before {
                                // render_styled_symbol didn't match
                                out.push_str(&format!("\\{cmd}{{{arg}}}"));
                            }
                            pos = after_arg;
                        } else {
                            out.push_str(&format!("\\{cmd}"));
                            pos = total_cmd_end;
                        }
                    }
                    // --- Fractions ---
                    "frac" | "dfrac" | "tfrac" | "cfrac" => {
                        if let Some((num_s, after_num)) = read_braced_at(input, total_cmd_end) {
                            if let Some((den_s, after_den)) = read_braced_at(input, after_num) {
                                let n = render_latex_to_string(&num_s);
                                let d = render_latex_to_string(&den_s);
                                out.push_str(&format!("({n}/{d})"));
                                pos = after_den;
                            } else {
                                out.push_str(&format!("({num_s}/?)"));
                                pos = after_num;
                            }
                        } else {
                            out.push_str(&format!("\\{cmd}"));
                            pos = total_cmd_end;
                        }
                    }
                    // --- Binomial coefficient ---
                    "binom" => {
                        if let Some((top_s, after_top)) = read_braced_at(input, total_cmd_end)
                            && let Some((bot_s, after_bot)) = read_braced_at(input, after_top)
                        {
                            out.push_str(&format!(
                                "({}/{})",
                                render_latex_to_string(&top_s),
                                render_latex_to_string(&bot_s)
                            ));
                            pos = after_bot;
                        } else {
                            // Malformed: keep the command literal and advance,
                            // or the loop would re-read it forever.
                            out.push_str("\\binom");
                            pos = total_cmd_end;
                        }
                    }
                    // --- Square root ---
                    "sqrt" => {
                        let after = &input[total_cmd_end..];
                        // Optional [n] root index, without its brackets.
                        let (root_text, after_root) = match after
                            .strip_prefix('[')
                            .and_then(|after_lb| after_lb.find(']').map(|end| (after_lb, end)))
                        {
                            Some((after_lb, end)) => {
                                (Some(&after_lb[..end]), total_cmd_end + end + 2)
                            }
                            None => (None, total_cmd_end),
                        };
                        if let Some(root) = root_text {
                            append_superscript(&render_latex_to_string(root), &mut out);
                        }
                        out.push('\u{221a}');
                        // One radical, and resume after the whole argument:
                        // the old path pushed a second radical and re-read the
                        // argument, and a radicand without braces never
                        // advanced at all (U04-m2).
                        if let Some((arg, after_arg)) = read_braced_at(input, after_root) {
                            out.push_str(&format!("({})", render_latex_to_string(&arg)));
                            pos = after_arg;
                        } else {
                            pos = after_root;
                        }
                    }
                    // --- Sum, product, integral ---
                    "sum" => {
                        out.push('\u{2211}');
                        pos = total_cmd_end;
                    }
                    "prod" => {
                        out.push('\u{220f}');
                        pos = total_cmd_end;
                    }
                    "int" => {
                        out.push('\u{222b}');
                        pos = total_cmd_end;
                    }
                    "iint" => {
                        out.push('\u{222c}');
                        pos = total_cmd_end;
                    }
                    "iiint" => {
                        out.push('\u{222d}');
                        pos = total_cmd_end;
                    }
                    "oint" => {
                        out.push('\u{222e}');
                        pos = total_cmd_end;
                    }
                    "oiint" => {
                        out.push('\u{222f}');
                        pos = total_cmd_end;
                    }
                    // --- Named operators ---
                    "lim" => {
                        out.push_str("lim");
                        pos = total_cmd_end;
                    }
                    "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "log" | "ln" | "lg" | "exp"
                    | "det" | "dim" | "ker" | "hom" | "max" | "min" | "sup" | "inf" | "arg"
                    | "deg" | "mod" | "gcd" | "lcm" | "Pr" | "Var" | "Cov" | "Corr" | "tr"
                    | "rank" | "Re" | "Im" | "sinh" | "cosh" | "tanh" | "coth" | "arcsin"
                    | "arccos" | "arctan" => {
                        out.push_str(cmd);
                        pos = total_cmd_end;
                    }
                    // --- Arrows ---
                    "to" | "rightarrow" => {
                        out.push('\u{2192}');
                        pos = total_cmd_end;
                    }
                    "leftarrow" => {
                        out.push('\u{2190}');
                        pos = total_cmd_end;
                    }
                    "Rightarrow" => {
                        out.push('\u{21d2}');
                        pos = total_cmd_end;
                    }
                    "Leftarrow" => {
                        out.push('\u{21d0}');
                        pos = total_cmd_end;
                    }
                    "Leftrightarrow" | "iff" => {
                        out.push('\u{21d4}');
                        pos = total_cmd_end;
                    }
                    "mapsto" => {
                        out.push('\u{21a6}');
                        pos = total_cmd_end;
                    }
                    "longrightarrow" => {
                        out.push('\u{27f6}');
                        pos = total_cmd_end;
                    }
                    "Longrightarrow" => {
                        out.push('\u{27f9}');
                        pos = total_cmd_end;
                    }
                    "uparrow" => {
                        out.push('\u{2191}');
                        pos = total_cmd_end;
                    }
                    "downarrow" => {
                        out.push('\u{2193}');
                        pos = total_cmd_end;
                    }
                    "Uparrow" => {
                        out.push('\u{21d1}');
                        pos = total_cmd_end;
                    }
                    "Downarrow" => {
                        out.push('\u{21d3}');
                        pos = total_cmd_end;
                    }
                    "longleftrightarrow" => {
                        out.push('\u{27f7}');
                        pos = total_cmd_end;
                    }
                    "Longleftrightarrow" => {
                        out.push('\u{27fa}');
                        pos = total_cmd_end;
                    }
                    "hookrightarrow" => {
                        out.push('\u{21aa}');
                        pos = total_cmd_end;
                    }
                    "hookleftarrow" => {
                        out.push('\u{21a9}');
                        pos = total_cmd_end;
                    }
                    "rightharpoonup" => {
                        out.push('\u{21c0}');
                        pos = total_cmd_end;
                    }
                    "rightharpoondown" => {
                        out.push('\u{21c1}');
                        pos = total_cmd_end;
                    }
                    "leftharpoonup" => {
                        out.push('\u{21bc}');
                        pos = total_cmd_end;
                    }
                    "leftharpoondown" => {
                        out.push('\u{21bd}');
                        pos = total_cmd_end;
                    }
                    "rightleftharpoons" => {
                        out.push('\u{21cc}');
                        pos = total_cmd_end;
                    }
                    "nrightarrow" => {
                        out.push('\u{219b}');
                        pos = total_cmd_end;
                    }
                    "nleftarrow" => {
                        out.push('\u{219a}');
                        pos = total_cmd_end;
                    }
                    // --- Unknown command ---
                    _ => {
                        if let Some(sym) = SYMBOLS.get_or_init(build_symbols).get(cmd) {
                            out.push_str(sym);
                            pos = total_cmd_end;
                            // Check for braces after symbol (e.g., \alpha_{i})
                            // The subscript/superscript will be handled by the
                            // main loop as _ and ^
                        } else {
                            // --- Unknown command ---
                            out.push('\\');
                            out.push_str(cmd);
                            pos = total_cmd_end;
                            // If followed by {, include the braced argument
                            if input[pos..].starts_with('{')
                                && let Some((arg, new_pos)) = read_braced_at(input, pos)
                            {
                                out.push('{');
                                out.push_str(&arg);
                                out.push('}');
                                pos = new_pos;
                            }
                        }
                    }
                }
            }
            '_' => {
                // Read subscript
                let after = &input[pos + 1..];
                if after.starts_with('{') {
                    if let Some((sub, new_pos)) = read_braced_at(input, pos + 1) {
                        append_subscript(&render_latex_to_string(&sub), &mut out);
                        pos = new_pos;
                    } else {
                        out.push('_');
                        pos += 1;
                    }
                } else {
                    // Subscript with command like _\mu _\nu
                    if let Some(after_bs) = after.strip_prefix('\\') {
                        let cmd_end = after_bs
                            .find(|c: char| !c.is_ascii_alphabetic())
                            .unwrap_or(after_bs.len());
                        let rendered = render_latex_to_string(&after[..1 + cmd_end]);
                        append_subscript(&rendered, &mut out);
                        pos += 1 + 1 + cmd_end;
                    } else {
                        let next = after.chars().next();
                        if let Some(c) = next {
                            append_subscript(&c.to_string(), &mut out);
                            pos += 1 + c.len_utf8();
                        } else {
                            out.push('_');
                            pos += 1;
                        }
                    }
                }
            }
            '^' => {
                // Read superscript
                let after = &input[pos + 1..];
                if after.starts_with('{') {
                    if let Some((sup, new_pos)) = read_braced_at(input, pos + 1) {
                        append_superscript(&render_latex_to_string(&sup), &mut out);
                        pos = new_pos;
                    } else {
                        out.push('^');
                        pos += 1;
                    }
                } else {
                    // Superscript with command like ^\dagger ^\rho
                    if let Some(after_bs) = after.strip_prefix('\\') {
                        let cmd_end = after_bs
                            .find(|c: char| !c.is_ascii_alphabetic())
                            .unwrap_or(after_bs.len());
                        let rendered = render_latex_to_string(&after[..1 + cmd_end]);
                        append_superscript(&rendered, &mut out);
                        pos += 1 + 1 + cmd_end;
                    } else {
                        let next = after.chars().next();
                        if let Some(c) = next {
                            append_superscript(&c.to_string(), &mut out);
                            pos += 1 + c.len_utf8();
                        } else {
                            out.push('^');
                            pos += 1;
                        }
                    }
                }
            }
            '{' | '}' => {
                pos += ch_len;
            }
            ' ' => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                pos += ch_len;
            }
            '\n' => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                pos += ch_len;
            }
            // Punctuation that shouldn't be duplicated
            '~' => {
                // Non-breaking space
                out.push(' ');
                pos += ch_len;
            }
            _ => {
                out.push(ch);
                pos += ch_len;
            }
        }
    }

    out.trim_end().to_string()
}

/// Try to parse a `\begin{env_name}...\end{env_name}` block at the start of `input`.
/// Returns (rendered_output, bytes_consumed) or None.
fn try_render_env(input: &str) -> Option<(String, usize)> {
    let input_bytes = input.as_bytes();

    // Check for \begin{
    if input.len() < 7 || &input_bytes[..7] != b"\\begin{" {
        return None;
    }

    // Find closing }
    let close = input[7..].find('}')?;
    let env_name = &input[7..7 + close];

    let content_start = 7 + close + 1; // after \begin{env_name}
    if content_start >= input.len() {
        return None;
    }

    // Find matching \end{env_name}
    let end_tag = format!("\\end{{{env_name}}}");
    let rest = &input[content_start..];

    // Simple depth tracking for nested braces
    let mut depth = 0i32;
    let mut search_pos = 0;

    while search_pos < rest.len() {
        let remaining_search = &rest[search_pos..];

        if remaining_search.starts_with(&end_tag) && depth == 0 {
            let env_content = &rest[..search_pos];
            let rendered = render_environment(env_name, env_content);
            let consumed = content_start + search_pos + end_tag.len();
            // --- Spacing ---
            return Some((rendered, consumed));
        }

        match remaining_search.as_bytes().first()? {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        search_pos += remaining_search.chars().next().map_or(1, char::len_utf8);
    }

    None
}

// --- Superscript / Subscript ---

fn append_superscript(s: &str, out: &mut String) {
    for c in s.chars() {
        out.push(match c {
            '0' => '\u{2070}',
            '1' => '\u{00b9}',
            '2' => '\u{00b2}',
            '3' => '\u{00b3}',
            '4' => '\u{2074}',
            '5' => '\u{2075}',
            '6' => '\u{2076}',
            '7' => '\u{2077}',
            '8' => '\u{2078}',
            '9' => '\u{2079}',
            '+' => '\u{207a}',
            '-' => '\u{207b}',
            '=' => '\u{207c}',
            '(' => '\u{207d}',
            ')' => '\u{207e}',
            'n' => '\u{207f}',
            'i' => '\u{2071}',
            'a' => '\u{1d43}',
            'b' => '\u{1d47}',
            'c' => '\u{1d9c}',
            'd' => '\u{1d48}',
            'e' => '\u{1d49}',
            'f' => '\u{1da0}',
            'g' => '\u{1d4d}',
            'h' => '\u{02b0}',
            'j' => '\u{02b2}',
            'k' => '\u{1d4f}',
            'l' => '\u{02e1}',
            'm' => '\u{1d50}',
            'o' => '\u{1d52}',
            'p' => '\u{1d56}',
            'r' => '\u{02b3}',
            's' => '\u{02e2}',
            't' => '\u{1d57}',
            'u' => '\u{1d58}',
            'v' => '\u{1d5b}',
            'w' => '\u{02b7}',
            'x' => '\u{02e3}',
            'y' => '\u{02b8}',
            'z' => '\u{1dbb}',
            _ => c,
        });
    }
}

fn append_subscript(s: &str, out: &mut String) {
    for c in s.chars() {
        out.push(match c {
            '0' => '\u{2080}',
            '1' => '\u{2081}',
            '2' => '\u{2082}',
            '3' => '\u{2083}',
            '4' => '\u{2084}',
            '5' => '\u{2085}',
            '6' => '\u{2086}',
            '7' => '\u{2087}',
            '8' => '\u{2088}',
            '9' => '\u{2089}',
            '+' => '\u{208a}',
            '-' => '\u{208b}',
            '=' => '\u{208c}',
            'a' => '\u{2090}',
            'e' => '\u{2091}',
            'h' => '\u{2095}',
            'i' => '\u{1d62}',
            'k' => '\u{2096}',
            'l' => '\u{2097}',
            'm' => '\u{2098}',
            'n' => '\u{2099}',
            'o' => '\u{2092}',
            'p' => '\u{209a}',
            'r' => '\u{1d63}',
            's' => '\u{209b}',
            't' => '\u{209c}',
            'u' => '\u{1d64}',
            'v' => '\u{1d65}',
            'x' => '\u{2093}',
            _ => c,
        });
    }
}

// --- Symbol table ---

type SymbolMap = HashMap<&'static str, &'static str>;
fn build_symbols() -> SymbolMap {
    let mut m = SymbolMap::new();
    // Lowercase Greek
    for (k, v) in [
        ("alpha", "\u{03b1}"),
        ("beta", "\u{03b2}"),
        ("gamma", "\u{03b3}"),
        ("delta", "\u{03b4}"),
        ("epsilon", "\u{03b5}"),
        ("zeta", "\u{03b6}"),
        ("eta", "\u{03b7}"),
        ("theta", "\u{03b8}"),
        ("iota", "\u{03b9}"),
        ("kappa", "\u{03ba}"),
        ("lambda", "\u{03bb}"),
        ("mu", "\u{03bc}"),
        ("nu", "\u{03bd}"),
        ("xi", "\u{03be}"),
        ("pi", "\u{03c0}"),
        ("rho", "\u{03c1}"),
        ("sigma", "\u{03c3}"),
        ("tau", "\u{03c4}"),
        ("upsilon", "\u{03c5}"),
        ("phi", "\u{03c6}"),
        ("chi", "\u{03c7}"),
        ("psi", "\u{03c8}"),
        ("omega", "\u{03c9}"),
        ("varepsilon", "\u{03b5}"),
        ("vartheta", "\u{03d1}"),
        ("varphi", "\u{03c6}"),
        ("varrho", "\u{03f1}"),
    ] {
        m.insert(k, v);
    }
    // Uppercase Greek
    for (k, v) in [
        ("Gamma", "\u{0393}"),
        ("Delta", "\u{0394}"),
        ("Theta", "\u{0398}"),
        ("Lambda", "\u{039b}"),
        ("Xi", "\u{039e}"),
        ("Pi", "\u{03a0}"),
        ("Sigma", "\u{03a3}"),
        ("Upsilon", "\u{03a5}"),
        ("Phi", "\u{03a6}"),
        ("Psi", "\u{03a8}"),
        ("Omega", "\u{03a9}"),
    ] {
        m.insert(k, v);
    }
    // Miscellaneous
    for (k, v) in [
        ("infty", "\u{221e}"),
        ("partial", "\u{2202}"),
        ("nabla", "\u{2207}"),
        ("ell", "\u{2113}"),
        ("hbar", "\u{210f}"),
        ("Im", "\u{2111}"),
        ("Re", "\u{211c}"),
        ("emptyset", "\u{2205}"),
        ("varnothing", "\u{2205}"),
        ("aleph", "\u{2135}"),
        ("angle", "\u{2220}"),
        ("measuredangle", "\u{2221}"),
        ("langle", "\u{27e8}"),
        ("rangle", "\u{27e9}"),
        ("perp", "\u{22a5}"),
        ("parallel", "\u{2225}"),
        ("nparallel", "\u{2226}"),
        ("prime", "\u{2032}"),
        ("surd", "\u{221a}"),
        ("top", "\u{22a4}"),
        ("bot", "\u{22a5}"),
        ("imath", "\u{0131}"),
        ("jmath", "\u{0237}"),
        ("wp", "\u{2118}"),
        ("clubsuit", "\u{2663}"),
        ("diamondsuit", "\u{2662}"),
        ("heartsuit", "\u{2661}"),
        ("spadesuit", "\u{2660}"),
        ("triangle", "\u{25b3}"),
        ("Box", "\u{25a1}"),
        ("Diamond", "\u{25c7}"),
        ("flat", "\u{266d}"),
        ("natural", "\u{266e}"),
        ("sharp", "\u{266f}"),
        ("colon", ":"),
        ("backslash", "\\"),
    ] {
        m.insert(k, v);
    }
    // Set / relation symbols
    for (k, v) in [
        ("in", "\u{2208}"),
        ("notin", "\u{2209}"),
        ("ni", "\u{220b}"),
        ("subset", "\u{2282}"),
        ("supset", "\u{2283}"),
        ("subseteq", "\u{2286}"),
        ("supseteq", "\u{2287}"),
        ("subsetneq", "\u{228a}"),
        ("supsetneq", "\u{228b}"),
        ("cup", "\u{222a}"),
        ("bigcup", "\u{22c3}"),
        ("cap", "\u{2229}"),
        ("bigcap", "\u{22c2}"),
        ("vee", "\u{2228}"),
        ("wedge", "\u{2227}"),
        ("oplus", "\u{2295}"),
        ("ominus", "\u{2296}"),
        ("otimes", "\u{2297}"),
        ("oslash", "\u{2298}"),
        ("odot", "\u{2299}"),
        ("sqcap", "\u{2293}"),
        ("sqcup", "\u{2294}"),
        ("uplus", "\u{228e}"),
        ("amalg", "\u{2a3f}"),
        ("forall", "\u{2200}"),
        ("exists", "\u{2203}"),
        ("nexists", "\u{2204}"),
        ("neg", "\u{00ac}"),
        ("lnot", "\u{00ac}"),
        ("land", "\u{2227}"),
        ("lor", "\u{2228}"),
        ("implies", "\u{21d2}"),
        ("iff", "\u{21d4}"),
        ("gets", "\u{2190}"),
        ("sim", "\u{223c}"),
        ("nsim", "\u{2241}"),
        ("simeq", "\u{2243}"),
        ("nsimeq", "\u{2244}"),
        ("cong", "\u{2245}"),
        ("ncong", "\u{2247}"),
        ("approx", "\u{2248}"),
        ("napprox", "\u{2249}"),
        ("neq", "\u{2260}"),
        ("ne", "\u{2260}"),
        ("equiv", "\u{2261}"),
        ("nequiv", "\u{2262}"),
        ("le", "\u{2264}"),
        ("ge", "\u{2265}"),
        ("leq", "\u{2264}"),
        ("geq", "\u{2265}"),
        ("leqq", "\u{2266}"),
        ("geqq", "\u{2267}"),
        ("lneq", "\u{2268}"),
        ("gneq", "\u{2269}"),
        ("ll", "\u{226a}"),
        ("gg", "\u{226b}"),
        ("lll", "\u{22d8}"),
        ("ggg", "\u{22d9}"),
        ("prec", "\u{227a}"),
        ("succ", "\u{227b}"),
        ("preceq", "\u{227c}"),
        ("succeq", "\u{227d}"),
        ("preccurlyeq", "\u{227c}"),
        ("succcurlyeq", "\u{227d}"),
        ("propto", "\u{221d}"),
        ("models", "\u{22a7}"),
        ("dashv", "\u{22a3}"),
        ("vdash", "\u{22a2}"),
        ("mid", "|"),
        ("nmid", "\u{2224}"),
    ] {
        m.insert(k, v);
    }
    // Operators
    for (k, v) in [
        ("times", "\u{00d7}"),
        ("div", "\u{00f7}"),
        ("pm", "\u{00b1}"),
        ("mp", "\u{2213}"),
        ("cdot", "\u{00b7}"),
        ("ast", "\u{2217}"),
        ("circ", "\u{2218}"),
        ("bullet", "\u{2022}"),
        ("setminus", "\u{2216}"),
        ("smallsetminus", "\u{2216}"),
        ("wr", "\u{2240}"),
        ("dagger", "\u{2020}"),
        ("ddagger", "\u{2021}"),
        ("star", "\u{22c6}"),
        ("diamond", "\u{22c4}"),
    ] {
        m.insert(k, v);
    }
    // Dots
    for (k, v) in [
        ("cdots", "\u{2026}"),
        ("ldots", "\u{2026}"),
        ("vdots", "\u{22ee}"),
        ("ddots", "\u{22f1}"),
        ("idots", "\u{2026}"),
    ] {
        m.insert(k, v);
    }
    // Named functions not covered by the inline list
    for (k, v) in [
        ("arccos", "arccos"),
        ("arcsin", "arcsin"),
        ("arctan", "arctan"),
        ("arg", "arg"),
        ("cos", "cos"),
        ("cosh", "cosh"),
        ("cot", "cot"),
        ("coth", "coth"),
        ("csc", "csc"),
        ("deg", "deg"),
        ("det", "det"),
        ("dim", "dim"),
        ("exp", "exp"),
        ("gcd", "gcd"),
        ("hom", "hom"),
        ("inf", "inf"),
        ("ker", "ker"),
        ("lg", "lg"),
        ("lim", "lim"),
        ("liminf", "liminf"),
        ("limsup", "limsup"),
        ("ln", "ln"),
        ("log", "log"),
        ("max", "max"),
        ("min", "min"),
        ("mod", "mod"),
        ("sec", "sec"),
        ("sin", "sin"),
        ("sinh", "sinh"),
        ("sup", "sup"),
        ("tan", "tan"),
        ("tanh", "tanh"),
    ] {
        m.insert(k, v);
    }
    m
}

static SYMBOLS: OnceLock<SymbolMap> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_superscript() {
        assert_eq!(render_latex_to_string("x^2"), "x\u{00b2}");
    }
    /// U04-m2: one radical, the index as a superscript, and rendering
    /// resumes after the argument instead of printing it twice.
    #[test]
    fn sqrt_renders_one_radical_and_consumes_its_argument() {
        assert_eq!(render_latex_to_string(r"\sqrt{x}+1"), "\u{221a}(x)+1");
        assert_eq!(
            render_latex_to_string(r"\sqrt[3]{x}"),
            "\u{00b3}\u{221a}(x)"
        );
    }

    /// A radicand or binomial without braces must still finish rendering;
    /// the old branches never advanced and spun the render loop forever.
    #[test]
    fn malformed_sqrt_and_binom_terminate() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send((
                render_latex_to_string(r"\sqrt 2"),
                render_latex_to_string(r"\binom{n} k"),
            ));
        });
        let (sqrt, binom) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("latex rendering must terminate");
        assert!(sqrt.starts_with('\u{221a}'), "{sqrt}");
        assert!(sqrt.ends_with('2'), "{sqrt}");
        assert!(binom.contains("binom"), "{binom}");
    }

    #[test]
    fn test_subscript() {
        assert_eq!(render_latex_to_string("x_1"), "x\u{2081}");
    }
    #[test]
    fn test_blackboard() {
        assert_eq!(render_latex_to_string(r"\mathbb{R}"), "\u{211d}");
    }
    #[test]
    fn test_infty() {
        assert_eq!(render_latex_to_string(r"\infty"), "\u{221e}");
    }
    #[test]
    fn test_inline_dollar() {
        let r = render_latex_in_text(r"text $x^2$ more");
        assert_eq!(r, "text x\u{00b2} more");
        assert!(matches!(r, std::borrow::Cow::Owned(_)));
    }
    #[test]
    fn test_display_bracket() {
        let r = render_latex_in_text(r"text \[x^2\] more");
        assert_eq!(r, "text x\u{00b2} more");
    }
    #[test]
    fn no_math_is_borrowed_without_copy() {
        let r = render_latex_in_text("plain prose with `code` but no math at all");
        assert!(matches!(r, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*r, "plain prose with `code` but no math at all");
        // The '$' fast path must not miss \(
        let p = render_latex_in_text("parens \\(x^2\\) inline");
        assert!(matches!(p, std::borrow::Cow::Owned(_)));
        assert_eq!(&*p, "parens x\u{00b2} inline");
    }
    #[test]
    fn preserves_currency() {
        assert_eq!(render_latex_in_text("cost $5 and $10"), "cost $5 and $10");
    }
    #[test]
    fn preserves_markdown_code() {
        assert_eq!(
            render_latex_in_text("`$x^2$` and $y^2$"),
            "`$x^2$` and y\u{00b2}"
        );
        assert_eq!(
            render_latex_in_text("```sh\necho $HOME\n```"),
            "```sh\necho $HOME\n```"
        );
    }
    #[test]
    fn preserves_escaped_dollars_and_unknown_commands() {
        assert_eq!(
            render_latex_in_text(r"cost \$5 and $\operatorname{foo}$"),
            r"cost \$5 and foo"
        );
    }
    #[test]
    fn test_text() {
        assert_eq!(render_latex_to_string(r"\text{hello}"), "hello");
    }
    #[test]
    fn test_operatorname() {
        assert_eq!(render_latex_to_string(r"\operatorname{sgn}"), "sgn");
    }
    #[test]
    fn test_left_right() {
        assert_eq!(
            render_latex_to_string(r"\left(\frac{a}{b}\right)"),
            "((a/b))"
        );
    }
    #[test]
    fn test_mathbf() {
        assert_eq!(render_latex_to_string(r"\mathbf{E}"), "E");
    }
    #[test]
    fn test_quad() {
        assert_eq!(render_latex_to_string(r"a \quad b"), "a     b");
    }
    #[test]
    fn test_environment_aligned() {
        let input = r"\begin{aligned} x &= y \\ a &= b \end{aligned}";
        let result = render_latex_to_string(input);
        assert!(result.contains("x"));
        assert!(result.contains("y"));
        assert!(result.contains("a"));
        assert!(result.contains("b"));
    }
    #[test]
    fn test_environment_matrix() {
        let input = r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}";
        let result = render_latex_to_string(input);
        assert!(result.contains("a"));
        assert!(result.contains("b"));
        assert!(result.contains("c"));
        assert!(result.contains("d"));
    }
    #[test]
    fn test_environment_cases() {
        let input = r"\begin{cases} x^2 & x < 0 \\ 0 & x = 0 \\ \ln x & x > 0 \end{cases}";
        let result = render_latex_to_string(input);
        assert!(result.contains("x\u{00b2}"));
        assert!(result.contains("ln"));
    }
}
