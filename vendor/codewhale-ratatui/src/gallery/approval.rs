//! Gallery entries for package Approval: the approval card, the review
//! verdict and the aggregate row. The fixtures double as usage examples: a
//! host maps its own request onto an [`ApprovalSubject`] and its own keys onto
//! an [`ApprovalState`], and the card paints what it was given.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
};

use super::Entry;
use crate::{
    ApprovalCard, ApprovalChoice, ApprovalEffect, ApprovalKey, ApprovalKind, ApprovalScope,
    ApprovalState, ApprovalSubject, ChoiceId, DecisionBand, DecisionBandAction, DecisionBandSave,
    KeyHint, KeyHints, Paint, ReviewAggregate, ReviewKind, ReviewVerdict, Theme,
};

const ALLOW_ONCE: ChoiceId = ChoiceId(1);
const ALLOW_SESSION: ChoiceId = ChoiceId(2);
const DENY: ChoiceId = ChoiceId(3);
const DENY_WHY: ChoiceId = ChoiceId(4);

/// The host's choices: allow once, allow for the session, deny, deny and say
/// why. Focus starts on `Deny`, not on an allow.
fn choices() -> ApprovalState {
    ApprovalState::new(vec![
        ApprovalChoice::new(ALLOW_ONCE, "Allow once", ApprovalEffect::Grants)
            .char_key('y')
            .char_key('1'),
        ApprovalChoice::new(
            ALLOW_SESSION,
            "Allow for this session",
            ApprovalEffect::Grants,
        )
        .char_key('a')
        .char_key('2'),
        ApprovalChoice::new(DENY, "Deny", ApprovalEffect::Refuses)
            .char_key('n')
            .char_key('3'),
        ApprovalChoice::new(DENY_WHY, "Deny and say why", ApprovalEffect::Other).char_key('e'),
    ])
    .reveal(ApprovalKey::char('o'))
}

fn command(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let subject = ApprovalSubject::new(ApprovalKind::Command, "cargo publish --dry-run")
        .cwd("codewhale/crates/tui")
        .scope(ApprovalScope::Inside)
        .agent("builder", false)
        .risk_note("checks the package before release; publishes nothing");
    ApprovalCard::new(&subject, &choices()).paint(area, buf, theme);
}

fn outside(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let subject = ApprovalSubject::new(ApprovalKind::Command, "rm -rf ../build /tmp/cache")
        .cwd("/Users/me/work/codewhale")
        .scope(ApprovalScope::Outside)
        .agent("reviewer-3", true)
        .risk_note("deletes files outside this project");
    ApprovalCard::new(&subject, &choices()).paint(area, buf, theme);
}

fn patch(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let subject = ApprovalSubject::new(ApprovalKind::FileChange, "src/lib.rs, src/theme.rs")
        .cwd("codewhale-ratatui")
        .scope(ApprovalScope::Inside)
        .agent("builder", false)
        .preview_line("@@ -12,3 +12,4 @@")
        .preview_line("-pub const OCEAN_TINT: f64 = 0.5;")
        .preview_line("+pub const OCEAN_TINT: f64 = 0.55;")
        .preview_line(" pub enum Ground {");
    ApprovalCard::new(&subject, &choices()).paint(area, buf, theme);
}

fn elevation(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let subject = ApprovalSubject::new(ApprovalKind::Elevation, "npm install --global typescript")
        .cwd("codewhale-app")
        .scope(ApprovalScope::Inside)
        .agent("builder", false)
        .risk_note("the sandbox blocked a write to /usr/local; this runs without it");
    ApprovalCard::new(&subject, &choices()).paint(area, buf, theme);
}

/// A command too long for the rows it was given: the card counts what is not
/// shown and offers the key that shows it.
fn clipped(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let long = "curl -fsSL https://example.com/install.sh | sh -s -- --prefix /opt/tools \
                --channel stable --no-modify-path --verbose --accept-license";
    let subject = ApprovalSubject::new(ApprovalKind::Command, long)
        .cwd("codewhale")
        .scope(ApprovalScope::Inside);
    ApprovalCard::new(&subject, &choices()).paint(area, buf, theme);
}

/// Text that tries to differ from what runs: an escape sequence, a bidi
/// override, a zero-width space, a tab, a second command after a newline and
/// trailing spaces. Each is drawn as a named token.
fn spoofed(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let target = "echo ok\u{1b}[2K\u{200b}\u{202e}txt.exe\tx\nrm -rf ~  ";
    let subject = ApprovalSubject::new(ApprovalKind::Command, target)
        .cwd("codewhale")
        .scope(ApprovalScope::Inside)
        .agent("builder", false);
    ApprovalCard::new(&subject, &choices()).paint(area, buf, theme);
}

fn verdicts(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let hints = KeyHints::new(vec![
        KeyHint::new("y", "allow once"),
        KeyHint::new("n", "deny"),
    ]);
    let cards = [
        ReviewVerdict::new(
            ReviewKind::Allowed,
            "Auto-Review",
            "reads files inside this project",
            &hints,
            None,
        ),
        ReviewVerdict::new(
            ReviewKind::Denied,
            "Auto-Review",
            "deletes files outside this project",
            &hints,
            None,
        )
        .map(|v| {
            v.subject("rm -rf ../build")
                .category("Deletes unseen files")
        }),
        ReviewVerdict::new(
            ReviewKind::Held,
            "Auto-Review",
            "pushes to a protected branch",
            &hints,
            None,
        )
        .map(|v| v.subject("git push origin main").id("ar_7K2M")),
        ReviewVerdict::new(
            ReviewKind::Undecided,
            "Auto-Review",
            "the review timed out",
            &hints,
            None,
        )
        .map(|v| v.subject("git push origin main")),
    ];
    let mut y = area.y;
    for card in cards.into_iter().flatten() {
        let h = card.height(area.width, theme);
        let rect = Rect::new(area.x, y, area.width, h).intersection(area);
        if rect.is_empty() {
            break;
        }
        card.paint(rect, buf, theme);
        y = rect.bottom().saturating_add(1);
    }
}

fn aggregate(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let rows = [
        ReviewAggregate {
            allowed: 42,
            denied: 3,
            held: 1,
            undecided: 1,
            reviewing: 2,
            ..ReviewAggregate::default()
        },
        ReviewAggregate::default(),
    ];
    let mut y = area.y;
    for row in &rows {
        let h = row.height(area.width, theme);
        let rect = Rect::new(area.x, y, area.width, h).intersection(area);
        if rect.is_empty() {
            break;
        }
        row.paint(rect, buf, theme);
        y = rect.bottom().saturating_add(1);
    }
}

fn native_band(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let band = native_band_facts(theme);
    band.paint(area, buf, theme);
}

fn native_band_collapsed(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let mut band = native_band_facts(theme);
    band.collapsed = Some(Line::styled(
        if theme.ascii() {
            " Run checks - command [Tab expand] "
        } else {
            " Run checks — command [Tab expand] "
        },
        theme.fg(crate::Role::Accent),
    ));
    band.paint(area, buf, theme);
}

fn native_band_facts(theme: &Theme) -> DecisionBand {
    let plain = theme.fg(crate::Role::Foreground);
    let muted = theme.fg(crate::Role::Muted);
    let primary = theme.fg(crate::Role::Accent);
    DecisionBand {
        body: vec![
            Line::styled(
                "  Runs a command: verify this project",
                primary.add_modifier(Modifier::BOLD),
            ),
            Line::styled("  cargo test --workspace", plain),
        ],
        saves: vec![DecisionBandSave {
            summary: "always ask first".into(),
            entries: vec!["run cargo test --workspace in this project".into()],
            omitted: 0,
            label: "Save:   ".into(),
            separator: if theme.ascii() { " / " } else { " · " }.into(),
            compact_more: " +{count} more".into(),
            full_more: "... {count} more".into(),
            label_style: primary,
            summary_style: plain,
            entries_style: muted,
            more_style: muted,
        }],
        question: Line::styled(
            "  Do you want to proceed?",
            plain.add_modifier(Modifier::BOLD),
        ),
        actions: vec![
            DecisionBandAction {
                line: Line::styled("  [1/y] Allow once", plain),
                persistent: false,
            },
            DecisionBandAction {
                line: Line::styled("  [2/a] Allow for this session", plain),
                persistent: false,
            },
            DecisionBandAction {
                line: Line::styled("  [p] Save this exact project rule", plain),
                persistent: true,
            },
            DecisionBandAction {
                line: Line::styled("> [3/n] Deny", primary.add_modifier(Modifier::BOLD)),
                persistent: false,
            },
        ],
        footer: Line::styled("  Enter chooses; Esc stops; Alt+V details", muted),
        save_hint: Some(Span::styled(" / s save ask rule", primary)),
        background: theme.bg(crate::Role::Background),
        rule: Span::styled(
            if theme.ascii() { "-" } else { "─" },
            theme.fg(crate::Role::Border),
        ),
        truncation_hint: Span::styled("  Details truncated: Alt+V", muted),
        collapsed: None,
    }
}

pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "approval-native-band",
            width: 70,
            height: 24,
            draw: native_band,
        },
        Entry {
            name: "approval-native-band-collapsed",
            width: 70,
            height: 3,
            draw: native_band_collapsed,
        },
        Entry {
            name: "approval-command",
            width: 70,
            height: 17,
            draw: command,
        },
        Entry {
            name: "approval-outside",
            width: 70,
            height: 18,
            draw: outside,
        },
        Entry {
            name: "approval-patch",
            width: 70,
            height: 20,
            draw: patch,
        },
        Entry {
            name: "approval-elevation",
            width: 70,
            height: 20,
            draw: elevation,
        },
        Entry {
            name: "approval-clipped",
            width: 70,
            height: 12,
            draw: clipped,
        },
        Entry {
            name: "approval-spoofed",
            width: 70,
            height: 20,
            draw: spoofed,
        },
        Entry {
            name: "review-verdicts",
            width: 70,
            height: 20,
            draw: verdicts,
        },
        Entry {
            name: "review-aggregate",
            width: 70,
            height: 4,
            draw: aggregate,
        },
    ]
}
