//! OSC 8 hyperlink emission and stripping.
//!
//! Modern terminals (iTerm2, Terminal.app 13+, Ghostty, Kitty, WezTerm,
//! Alacritty, recent gnome-terminal/konsole) make a substring clickable when
//! it is wrapped in:
//!
//! ```text
//! \x1b]8;;TARGET\x1b\\LABEL\x1b]8;;\x1b\\
//! ```
//!
//! Terminals that don't understand the sequence simply render the visible
//! `LABEL` and ignore the escape. So emitting OSC 8 is a strict UX upgrade for
//! supporting terminals and a no-op for the rest.
//!
//! # Architecture (#3029)
//!
//! Link targets never enter `Span::content` or a ratatui `Buffer`. Markdown
//! wrapping produces plain visible spans plus parallel [`LineLink`] metadata.
//! Transcript surfaces translate those relative columns into absolute
//! [`LinkRegion`]s for the current viewport. `ColorCompatBackend::draw` then
//! emits OSC 8 escapes around the corresponding cell runs. This keeps text
//! layout, selection, and clipboard extraction byte-for-byte identical with
//! links enabled or disabled, including long links wrapped across rows.
//! Markdown contributes only normalized HTTP(S) and absolute `file://`
//! targets, and emission percent-encodes terminal control characters as
//! defense in depth.
//!
//! Opening is terminal-owned: supporting terminals conventionally use
//! Cmd-click on macOS or Ctrl-click on Linux/Windows. CodeWhale does not
//! intercept those gestures or launch URLs itself, so mouse selection remains
//! independent of browser-opening policy.
//!
//! The clipboard/selection extraction path still strips any residual codes via
//! [`strip_into`] / [`strip_ansi_into`] as a defense-in-depth.

use std::sync::atomic::{AtomicBool, Ordering};

const OSC8_PREFIX: &str = "\x1b]8;;";
const OSC8_TERMINATOR: &str = "\x1b\\";
const OSC8_CLOSE: &str = "\x1b]8;;\x1b\\";

/// A contiguous run of cells on one terminal row that share a hyperlink target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRegion {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
    pub target: String,
}

/// Hyperlink metadata for one already-wrapped visible line. Columns are
/// zero-based display columns relative to that line and `col_end` is
/// inclusive, matching [`LinkRegion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineLink {
    pub col_start: usize,
    pub col_end: usize,
    pub target: String,
}

impl LineLink {
    #[must_use]
    pub fn shifted(&self, columns: usize) -> Self {
        Self {
            col_start: self.col_start.saturating_add(columns),
            col_end: self.col_end.saturating_add(columns),
            target: self.target.clone(),
        }
    }
}

/// Translate per-line relative metadata into absolute terminal regions for a
/// rendered viewport. Metadata outside `area` is clipped rather than allowed
/// to hyperlink adjacent chrome (for example the transcript scrollbar).
#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn link_regions_for_lines(
    area: ratatui::layout::Rect,
    links: &[Vec<LineLink>],
) -> Vec<LinkRegion> {
    link_regions_for_plan(
        &codewhale_ratatui::TranscriptViewport::new(&[]).plan(area),
        links,
    )
}

/// Project targets through the final painted geometry; no target enters kit data.
#[must_use]
pub fn link_regions_for_plan(
    plan: &codewhale_ratatui::TranscriptViewportPlan,
    links: &[Vec<LineLink>],
) -> Vec<LinkRegion> {
    let mut regions = Vec::new();
    for (row, links) in links.iter().take(usize::from(plan.area.height)).enumerate() {
        for link in links {
            for rect in plan.link_rects(row, link.col_start, link.col_end) {
                regions.push(LinkRegion {
                    row: rect.y,
                    col_start: rect.x,
                    col_end: rect.right().saturating_sub(1),
                    target: link.target.clone(),
                });
            }
        }
    }
    regions
}

/// Write an OSC 8 hyperlink open sequence for `target` to `w`.
pub fn write_osc8_open(w: &mut impl std::io::Write, target: &str) -> std::io::Result<()> {
    w.write_all(OSC8_PREFIX.as_bytes())?;
    write_sanitized_target(w, target)?;
    w.write_all(OSC8_TERMINATOR.as_bytes())
}

/// Percent-encode terminal control characters before they enter an OSC
/// parameter. Markdown and restored transcripts are untrusted input: a raw
/// BEL, ESC/ST, or other control byte could terminate the link and inject an
/// arbitrary terminal sequence. Printable Unicode and ordinary URL bytes are
/// preserved byte-for-byte.
fn write_sanitized_target(w: &mut impl std::io::Write, target: &str) -> std::io::Result<()> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = [0u8; 4];
    for ch in target.chars() {
        let value = ch.encode_utf8(&mut encoded);
        if ch.is_control() {
            for &byte in value.as_bytes() {
                w.write_all(&[
                    b'%',
                    HEX[usize::from(byte >> 4)],
                    HEX[usize::from(byte & 0x0f)],
                ])?;
            }
        } else {
            w.write_all(value.as_bytes())?;
        }
    }
    Ok(())
}

/// Write an OSC 8 hyperlink close sequence to `w`.
pub fn write_osc8_close(w: &mut impl std::io::Write) -> std::io::Result<()> {
    w.write_all(OSC8_CLOSE.as_bytes())
}

/// Process-wide enable flag. Set once at app init from `[tui] osc8_links`
/// (when present); otherwise defaults to on for macOS/Linux and off for
/// Windows legacy consoles (see `ui.rs`'s `osc8_default_on`). Read by the
/// renderer to gate out-of-band OSC 8 emission.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Set the process-wide OSC 8 enable flag. Intended to be called once at
/// startup; subsequent calls take effect immediately.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether OSC 8 hyperlink emission is currently enabled.
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

// --- Thread-local link region accumulator (#3029) ---

use std::cell::RefCell;

thread_local! {
    /// Link regions collected during the current render frame.
    /// Populated by transcript widgets from their parallel line metadata;
    /// consumed and cleared by `ColorCompatBackend::draw()`.
    pub static FRAME_LINKS: RefCell<Vec<LinkRegion>> = const { RefCell::new(Vec::new()) };
}

/// Replace the thread-local frame link buffer with `links`.
pub fn set_frame_links(links: Vec<LinkRegion>) {
    FRAME_LINKS.with(|cell| {
        *cell.borrow_mut() = links;
    });
}

/// Append `links` to the thread-local frame link buffer. Used when more than
/// one widget renders link-bearing content into the same frame (e.g. the main
/// transcript and the live-transcript overlay): each seam appends rather than
/// replacing, so all regions reach `ColorCompatBackend::draw`.
pub fn append_frame_links(links: Vec<LinkRegion>) {
    FRAME_LINKS.with(|cell| cell.borrow_mut().extend(links));
}

/// Replace the portion of the current frame-link map covered by an opaque
/// overlay, preserving (and clipping) regions that remain visible around it.
/// This prevents a transcript URL underneath a modal from making unrelated
/// popup text clickable when both widgets paint in the same terminal frame.
pub fn overlay_frame_links(area: ratatui::layout::Rect, links: Vec<LinkRegion>) {
    if area.width == 0 || area.height == 0 {
        append_frame_links(links);
        return;
    }
    let x_start = area.x;
    let x_end = area.right();
    let y_start = area.y;
    let y_end = area.bottom();
    FRAME_LINKS.with(|cell| {
        let mut current = cell.borrow_mut();
        let mut visible = Vec::with_capacity(current.len().saturating_add(links.len()));
        for region in current.drain(..) {
            if region.row < y_start
                || region.row >= y_end
                || region.col_end < x_start
                || region.col_start >= x_end
            {
                visible.push(region);
                continue;
            }
            if region.col_start < x_start {
                let mut left = region.clone();
                left.col_end = x_start.saturating_sub(1);
                visible.push(left);
            }
            if region.col_end >= x_end {
                let mut right = region;
                right.col_start = x_end;
                visible.push(right);
            }
        }
        visible.extend(links);
        *current = visible;
    });
}

/// Take the thread-local frame links, leaving an empty vec behind.
pub fn take_frame_links() -> Vec<LinkRegion> {
    FRAME_LINKS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// Strip ANSI/OSC/control sequences from `s` into `out`.
///
/// Delegates to the single shared implementation in
/// [`codewhale_secrets::sanitize`] (FEAT-025 D4) so `/export`, `/structcopy`,
/// and the renderer cannot drift.
pub fn strip_ansi_into(s: &str, out: &mut String) {
    codewhale_secrets::sanitize::strip_ansi_into(s, out);
}

/// Like [`strip_ansi_into`], but SGR sequences (`ESC [ … m`: colour, bold,
/// underline, reset) pass through untouched so a renderer that understands
/// them can paint the output as the tool emitted it. Everything else — OSC
/// (including OSC 8 hyperlink wrappers), cursor movement, DCS, lone control
/// bytes — is still removed; only the styling survives.
pub fn strip_ansi_keep_sgr_into(s: &str, out: &mut String) {
    codewhale_secrets::sanitize::strip_ansi_keep_sgr_into(s, out);
}

/// Length in bytes of the UTF-8 sequence that starts with `lead`. Falls back
/// to `1` for continuation bytes / invalid leads so callers always make
/// forward progress.
///
/// Delegates to the shared implementation in [`codewhale_secrets::sanitize`].
fn utf8_seq_len(lead: u8) -> usize {
    codewhale_secrets::sanitize::utf8_seq_len(lead)
}

/// Strip OSC 8 escape sequences from `s` into `out`, preserving the visible
/// label text. Other escapes (color, style) pass through untouched. The
/// implementation handles both the standard `ESC \` and the lone `BEL`
/// terminators that some emitters use.
pub fn strip_into(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for the OSC 8 prefix `ESC ] 8 ;`
        if i + 4 <= bytes.len()
            && bytes[i] == 0x1b
            && bytes[i + 1] == b']'
            && bytes[i + 2] == b'8'
            && bytes[i + 3] == b';'
        {
            // Skip until the string terminator (ESC \) or BEL.
            let mut j = i + 4;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    j += 1;
                    break;
                }
                if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                    j += 2;
                    break;
                }
                j += 1;
            }
            i = j;
            continue;
        }
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else {
            let len = utf8_seq_len(b);
            let end = (i + len).min(bytes.len());
            if let Ok(chunk) = std::str::from_utf8(&bytes[i..end]) {
                out.push_str(chunk);
            }
            i = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that read or write the `ENABLED` flag so they don't
    /// race each other under cargo's default parallel test runner.
    static FLAG_GUARD: Mutex<()> = Mutex::new(());

    fn strip(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        strip_into(s, &mut out);
        out
    }

    fn wrapped_link(target: &str, label: &str) -> String {
        format!("{OSC8_PREFIX}{target}{OSC8_TERMINATOR}{label}{OSC8_CLOSE}")
    }

    #[test]
    fn wrapped_link_fixture_is_osc_8_compliant() {
        let wrapped = wrapped_link("https://example.com", "click me");
        assert_eq!(
            wrapped,
            "\x1b]8;;https://example.com\x1b\\click me\x1b]8;;\x1b\\"
        );
    }

    #[test]
    fn strip_removes_wrapper_keeps_label() {
        let wrapped = wrapped_link("https://example.com", "click me");
        assert_eq!(strip(&wrapped), "click me");
    }

    #[test]
    fn strip_handles_bel_terminator() {
        let wrapped = "\x1b]8;;https://example.com\x07click me\x1b]8;;\x07";
        assert_eq!(strip(wrapped), "click me");
    }

    #[test]
    fn strip_passes_through_text_with_no_escapes() {
        let plain = "no escapes here";
        assert_eq!(strip(plain), plain);
    }

    #[test]
    fn strip_preserves_non_osc_8_escapes() {
        // Color escape stays in place; only OSC 8 wrappers are removed.
        let mixed = format!(
            "\x1b[31mred\x1b[0m {wrapped}",
            wrapped = wrapped_link("https://example.com", "click")
        );
        assert_eq!(strip(&mixed), "\x1b[31mred\x1b[0m click");
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        strip_ansi_into(s, &mut out);
        out
    }

    #[test]
    fn strip_ansi_removes_csi_sgr_and_keeps_text() {
        let coloured = "526   \x1b[1;32mOPEN\x1b[0m  bug fix";
        assert_eq!(strip_ansi(coloured), "526   OPEN  bug fix");
    }

    #[test]
    fn strip_keep_sgr_keeps_colour_and_drops_everything_else() {
        let mut out = String::new();
        strip_ansi_keep_sgr_into(
            "\x1b]8;;https://x\x07\x1b[1;32mok\x1b[0m\x1b]8;;\x07\x1b[2K\x1b[?25l tail",
            &mut out,
        );
        assert_eq!(out, "\x1b[1;32mok\x1b[0m tail");
        let mut plain = String::new();
        strip_ansi_into(&out, &mut plain);
        assert_eq!(plain, "ok tail");
    }

    #[test]
    fn strip_ansi_removes_osc_8_wrapper() {
        let wrapped = wrapped_link("https://example.com", "click");
        assert_eq!(strip_ansi(&wrapped), "click");
    }

    #[test]
    fn strip_ansi_preserves_newlines_tabs_and_cr() {
        let s = "a\nb\tc\rd";
        assert_eq!(strip_ansi(s), "a\nb\tc\rd");
    }

    #[test]
    fn strip_ansi_drops_lone_control_bytes() {
        // Bare BEL or other C0 control bytes that aren't \n/\r/\t are dropped
        // so they can't paint as visible cells.
        let s = "a\x07b\x01c";
        assert_eq!(strip_ansi(s), "abc");
    }

    #[test]
    fn strip_ansi_preserves_utf8_multibyte_chars() {
        // CJK, accented Latin, and emoji must survive the strip without being
        // re-decoded as Latin-1 (which would explode 你 -> ä½ ).
        let s = "Phase 1: 第一步 README é 🚀";
        assert_eq!(strip_ansi(s), "Phase 1: 第一步 README é 🚀");

        let coloured = "\x1b[1;32m第一步\x1b[0m done";
        assert_eq!(strip_ansi(coloured), "第一步 done");
    }

    #[test]
    fn strip_preserves_utf8_multibyte_chars() {
        let wrapped = wrapped_link("https://example.com", "点击我");
        assert_eq!(strip(&wrapped), "点击我");
    }

    #[test]
    fn open_sequence_percent_encodes_target_control_injection() {
        let target = "https://safe.test/a\x07b\x1b]8;;https://evil.test\x1b\\c\x7f\u{009c}";
        let mut bytes = Vec::new();
        write_osc8_open(&mut bytes, target).expect("write OSC 8 open");
        let rendered = String::from_utf8(bytes.clone()).expect("valid UTF-8 output");

        assert_eq!(rendered.matches(OSC8_PREFIX).count(), 1, "{rendered:?}");
        assert_eq!(rendered.matches(OSC8_TERMINATOR).count(), 1, "{rendered:?}");
        assert_eq!(bytes.iter().filter(|byte| **byte == 0x1b).count(), 2);
        assert!(!bytes.contains(&0x07), "BEL escaped: {rendered:?}");
        assert!(!bytes.contains(&0x7f), "DEL escaped: {rendered:?}");
        assert!(
            rendered.contains("a%07b%1B]8;;https://evil.test%1B\\c%7F%C2%9C"),
            "control bytes must be percent-encoded: {rendered:?}"
        );
    }

    #[test]
    fn enabled_is_true_by_default_when_untouched() {
        // Hold the flag guard so we observe the initial state, not a value
        // mid-flight from `set_enabled_round_trips`. The flag *defaults* to
        // true at static init and tests in this module are the only writers.
        let _g = FLAG_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        assert!(enabled());
    }

    #[test]
    fn set_enabled_round_trips() {
        let _g = FLAG_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prior = enabled();
        set_enabled(false);
        assert!(!enabled());
        set_enabled(true);
        assert!(enabled());
        set_enabled(prior);
    }

    #[test]
    fn line_links_translate_to_absolute_clipped_regions() {
        let area = ratatui::layout::Rect::new(7, 3, 8, 2);
        let links = vec![
            vec![
                LineLink {
                    col_start: 2,
                    col_end: 20,
                    target: "https://example.test/long".to_string(),
                },
                LineLink {
                    col_start: 8,
                    col_end: 9,
                    target: "outside".to_string(),
                },
            ],
            vec![LineLink {
                col_start: 0,
                col_end: 1,
                target: "https://example.test/next".to_string(),
            }],
            vec![LineLink {
                col_start: 0,
                col_end: 0,
                target: "below viewport".to_string(),
            }],
        ];

        assert_eq!(
            link_regions_for_lines(area, &links),
            vec![
                LinkRegion {
                    row: 3,
                    col_start: 9,
                    col_end: 14,
                    target: "https://example.test/long".to_string(),
                },
                LinkRegion {
                    row: 4,
                    col_start: 7,
                    col_end: 8,
                    target: "https://example.test/next".to_string(),
                },
            ]
        );
    }

    #[test]
    fn opaque_overlay_replaces_and_clips_underlying_regions() {
        set_frame_links(vec![
            LinkRegion {
                row: 4,
                col_start: 0,
                col_end: 20,
                target: "under-wide".to_string(),
            },
            LinkRegion {
                row: 5,
                col_start: 6,
                col_end: 8,
                target: "under-covered".to_string(),
            },
        ]);
        overlay_frame_links(
            ratatui::layout::Rect::new(5, 4, 10, 2),
            vec![LinkRegion {
                row: 4,
                col_start: 7,
                col_end: 8,
                target: "modal".to_string(),
            }],
        );

        assert_eq!(
            take_frame_links(),
            vec![
                LinkRegion {
                    row: 4,
                    col_start: 0,
                    col_end: 4,
                    target: "under-wide".to_string(),
                },
                LinkRegion {
                    row: 4,
                    col_start: 15,
                    col_end: 20,
                    target: "under-wide".to_string(),
                },
                LinkRegion {
                    row: 4,
                    col_start: 7,
                    col_end: 8,
                    target: "modal".to_string(),
                },
            ]
        );
    }
}
