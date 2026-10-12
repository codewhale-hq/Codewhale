//! Calm first-run onboarding: one decision per screen.
//!
//! The flow asks only for what Codewhale genuinely needs before the user can
//! begin: language (only when it cannot be confidently inferred from settings
//! or the environment), a provider/model route (only when no usable route is
//! configured), and workspace trust (only when a decision is required).
//! Appearance, command tours, mode primers, and tips stay in `/setup` and
//! contextual help so nothing delays first use. The last screen hands the
//! user the real composer pre-seeded with a first task for this folder.
//!
//! Rendering uses the shared Underwater instrument grammar — one title
//! hairline, one bottom action rail — never a bespoke centered card.

pub mod language;
pub mod trust_directory;
pub mod welcome;

use std::path::{Path, PathBuf};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::tui::app::{App, OnboardingState};
use crate::tui::views::{ActionHint, render_modal_footer, render_underwater_surface};
use codewhale_localization::MessageId;
use codewhale_palette as palette;

const ONBOARDED_MARKER_FILE: &str = ".onboarded";

/// Cheap workspace markers that identify a code project, so the seeded first
/// task can speak to what is actually in the folder. One `is_file` probe per
/// name; no directory walk.
const CODE_PROJECT_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "go.mod",
    "deno.json",
    "composer.json",
];

pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let title = surface_title(app);
    let hints = action_hints(app);
    let buf = f.buffer_mut();
    let inner = render_underwater_surface(area, buf, title);
    let content = render_modal_footer(inner, buf, &hints);
    let lines = screen_lines(app, usize::from(content.width), usize::from(content.height));
    if lines.is_empty() {
        return;
    }
    let body = center_vertically(content, lines.len());
    f.render_widget(Paragraph::new(lines), body);
}

/// Vertical rest for the short screens: half the leftover rows above, the
/// rest below. Content taller than the area stays top-anchored so nothing is
/// silently pushed out of view.
fn center_vertically(area: Rect, rows: usize) -> Rect {
    let pad = (area
        .height
        .saturating_sub(u16::try_from(rows).unwrap_or(area.height)))
        / 2;
    Rect {
        y: area.y.saturating_add(pad),
        height: area.height.saturating_sub(pad),
        ..area
    }
}

fn surface_title(app: &App) -> String {
    let base = app.tr(MessageId::OnboardStepsTitle).into_owned();
    match required_progress(app) {
        Some((current, total)) => format!("{base} · {current}/{total}"),
        None => base,
    }
}

/// The hairline surface title counts only the REQUIRED decisions left in
/// this run — never a "Step 1/7" spine. Screens that are not themselves a
/// required decision (welcome, ready) show no counter. A decision drops out
/// of the count once it is satisfied, so the counter only ever moves
/// forward.
fn required_progress(app: &App) -> Option<(usize, usize)> {
    // The counter's denominator is every required decision this run, not
    // only the ones still ahead: advancing past the language screen must
    // not shrink "1 of 2" into a bare "1 of 1".
    let mut steps = Vec::new();
    if app.onboarding_had_language_step {
        steps.push(OnboardingState::Language);
    }
    if app.onboarding_had_provider_step {
        steps.push(OnboardingState::Provider);
    }
    if app.onboarding_had_trust_step {
        steps.push(OnboardingState::TrustDirectory);
    }
    if steps.len() < 2 {
        return None;
    }
    let current = steps.iter().position(|step| *step == app.onboarding)?;
    Some((current + 1, steps.len()))
}

fn trust_decision_required(app: &App) -> bool {
    !app.trust_mode && needs_trust(&app.workspace)
}

fn action_hints(app: &App) -> Vec<ActionHint> {
    match app.onboarding {
        OnboardingState::Welcome => vec![
            ActionHint::new("Enter", app.tr(MessageId::OnboardWelcomeBegin).to_string()),
            ActionHint::new("Ctrl+C", app.tr(MessageId::OnboardActionExit).to_string()),
        ],
        OnboardingState::Language => vec![
            ActionHint::new(
                "1-9/a-g",
                app.tr(MessageId::OnboardLanguagePick).to_string(),
            ),
            ActionHint::new("Enter", app.tr(MessageId::OnboardLanguageKeep).to_string()),
            ActionHint::new("Esc", app.tr(MessageId::OnboardActionBack).to_string()),
        ],
        OnboardingState::Provider => vec![
            ActionHint::new(
                "Enter",
                app.tr(MessageId::OnboardProviderChoose).to_string(),
            ),
            ActionHint::new(
                "Ctrl+O",
                app.tr(MessageId::OnboardProviderOffline).to_string(),
            ),
            ActionHint::new("Esc", app.tr(MessageId::OnboardActionBack).to_string()),
        ],
        OnboardingState::TrustDirectory => vec![
            ActionHint::new(
                "1/Y",
                app.tr(MessageId::OnboardTrustActionTrust).to_string(),
            ),
            ActionHint::new("2/U", app.tr(MessageId::OnboardTrustActionSkip).to_string()),
            ActionHint::new("3/N", app.tr(MessageId::OnboardTrustActionQuit).to_string()),
        ],
        OnboardingState::Ready => vec![
            // Only keys this screen handles: a `/rc` hint here could not be
            // typed, because the Ready screen owns the keyboard.
            ActionHint::new("Enter", app.tr(MessageId::OnboardReadyStart).to_string()),
            ActionHint::new("C", app.tr(MessageId::OnboardReadyCustomize).to_string()),
        ],
        OnboardingState::None => Vec::new(),
    }
}

fn screen_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    match app.onboarding {
        OnboardingState::Welcome => welcome::lines(app, width),
        OnboardingState::Language => language::lines(app, width, height),
        OnboardingState::Provider => provider_lines(app, width),
        OnboardingState::TrustDirectory => trust_directory::lines(app, width),
        OnboardingState::Ready => welcome::ready_lines(app, width),
        OnboardingState::None => Vec::new(),
    }
}

fn provider_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    heading(&mut lines, app, MessageId::OnboardProviderTitle, width);
    lines.push(Line::from(""));
    if let Some(notice) = app.onboarding_key_rejected.as_deref() {
        for segment in wrap_words(notice, width) {
            lines.push(Line::from(Span::styled(
                segment,
                Style::default().fg(palette::STATUS_ERROR),
            )));
        }
        lines.push(Line::from(""));
    }
    wrap_body(&mut lines, app, MessageId::OnboardProviderBlurb, width);
    lines
}

/// Same rule as the welcome headline: a heading is prose and wraps. Today's
/// provider title happens to fit at 40 columns in every shipped locale, but it
/// fit by luck rather than by construction.
fn heading(out: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    for segment in wrap_words(&app.tr(id), width) {
        out.push(Line::from(Span::styled(
            segment,
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )));
    }
}

/// Append a body sentence word-wrapped to `width` in the muted body lane.
fn wrap_body(lines: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    let text = app.tr(id);
    for segment in wrap_words(&text, width) {
        lines.push(Line::from(Span::styled(
            segment,
            Style::default().fg(palette::TEXT_PRIMARY),
        )));
    }
}

/// Characters that may not begin a line in Japanese and Chinese typography
/// (a small, uncontroversial kinsoku set: closing brackets, sentence-final
/// punctuation, and the sound-extension mark). When a width break would strand
/// one of these at the start of a line, carry its preceding grapheme forward.
const NO_LINE_START: &[char] = &[
    '。', '、', '．', '，', '，', '。', '」', '』', '）', '］', '｝', '〕', '〉', '》', '”', '’',
    '！', '？', '：', '；', 'ー', '々', '·', '…', '!', '?', ',', '.', ':', ';', ')', ']', '}',
];

/// Break one unbreakable token into lines of at most `width` display columns.
///
/// Japanese, Chinese, and Thai do not separate words with spaces, so an entire
/// sentence arrives as a single token. Breaking on grapheme clusters by display
/// width is the conventional behaviour for those scripts and is the only way to
/// show the text at all; the alternative is the clip this replaces.
fn break_by_display_width(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for cluster in text.graphemes(true) {
        let cluster_width = UnicodeWidthStr::width(cluster);
        if current_width + cluster_width > width && !current.is_empty() {
            let starts_forbidden = cluster
                .chars()
                .next()
                .is_some_and(|c| NO_LINE_START.contains(&c));
            if starts_forbidden {
                // Carry the preceding text with its punctuation onto the next
                // line. Extending a full line instead silently clips it in a
                // terminal viewport, including on the redaction consent gate.
                if let Some((split, _)) = current.grapheme_indices(true).rev().find(|(_, part)| {
                    !part
                        .chars()
                        .next()
                        .is_some_and(|ch| NO_LINE_START.contains(&ch))
                }) && split > 0
                    && UnicodeWidthStr::width(&current[split..]) + cluster_width <= width
                {
                    let carry = current.split_off(split);
                    out.push(std::mem::replace(&mut current, carry));
                    current_width = UnicodeWidthStr::width(current.as_str());
                } else {
                    // A punctuation-only run cannot satisfy both typography
                    // and width; preserve every character inside the viewport.
                    out.push(std::mem::take(&mut current));
                    current_width = 0;
                }
            } else {
                out.push(std::mem::take(&mut current));
                current_width = 0;
            }
        }
        current.push_str(cluster);
        current_width += cluster_width;
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Word wrap by display width so the composed row count is exact and no
/// paragraph re-wrap can clip a locale with longer sentences.
pub(crate) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let width = width.max(8);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in text.split_whitespace() {
        let word_width = UnicodeWidthStr::width(word);

        // A token wider than the whole lane cannot fit on any line. Scripts
        // that do not delimit words produce exactly one such token per
        // sentence, and the word-only path below never breaks it — it appended
        // the token whole and the terminal clipped the tail, silently dropping
        // the second half of every long Japanese string.
        if word_width > width {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                current_width = 0;
            }
            let mut chunks = break_by_display_width(word, width);
            if let Some(last) = chunks.pop() {
                out.extend(chunks);
                current_width = UnicodeWidthStr::width(last.as_str());
                current = last;
            }
            continue;
        }

        let needed = if current.is_empty() {
            word_width
        } else {
            current_width + 1 + word_width
        };
        if !current.is_empty() && needed > width {
            out.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_width += 1;
        }
        current.push_str(word);
        current_width += word_width;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub fn default_marker_path() -> Option<PathBuf> {
    let primary_home = codewhale_config::codewhale_home().ok()?;
    let legacy_home = if codewhale_config::codewhale_home_is_explicit() {
        None
    } else {
        codewhale_config::legacy_deepseek_home().ok()
    };
    Some(marker_path_with_roots(
        &primary_home,
        legacy_home.as_deref(),
    ))
}

#[cfg(test)]
fn marker_path_with_home(home: &Path) -> PathBuf {
    marker_path_with_roots(
        &home.join(".codewhale"),
        Some(home.join(".deepseek").as_path()),
    )
}

fn marker_path_with_roots(primary_home: &Path, legacy_home: Option<&Path>) -> PathBuf {
    let primary = primary_home.join(ONBOARDED_MARKER_FILE);
    if primary.exists() {
        return primary;
    }
    if let Some(legacy_home) = legacy_home {
        let legacy = legacy_home.join(ONBOARDED_MARKER_FILE);
        if legacy.exists() {
            return legacy;
        }
    }
    primary
}

pub fn is_onboarded() -> bool {
    default_marker_path().is_some_and(|path| path.exists())
}

pub fn mark_onboarded() -> std::io::Result<PathBuf> {
    let path = default_marker_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Codewhale home directory not found",
        )
    })?;
    mark_onboarded_at_path(path)
}

#[cfg(test)]
fn mark_onboarded_at_home(home: &Path) -> std::io::Result<PathBuf> {
    let path = marker_path_with_home(home);
    mark_onboarded_at_path(path)
}

fn mark_onboarded_at_path(path: PathBuf) -> std::io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

pub fn needs_trust(workspace: &Path) -> bool {
    if crate::config::is_workspace_trusted(workspace) {
        return false;
    }

    let markers = [
        workspace.join(".deepseek").join("trusted"),
        workspace.join(".deepseek").join("trust.json"),
    ];
    !markers.iter().any(|path| path.exists())
}

pub fn mark_trusted(workspace: &Path) -> anyhow::Result<PathBuf> {
    crate::config::save_workspace_trust(workspace)
}

/// Whether the UI locale can be trusted without asking. An explicit settings
/// value or an environment locale that resolves to a shipped pack is
/// confident; anything else defaults to English silently, so first run asks
/// once. Returning users never see the language screen.
pub fn locale_confidently_inferred(setting: &str) -> bool {
    let normalized = codewhale_localization::normalize_configured_locale(setting);
    if normalized.is_some_and(|tag| tag != "auto") {
        return true;
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().any(|key| {
        std::env::var(key)
            .ok()
            .filter(|value| locale_var_names_a_language(value))
            .and_then(|value| codewhale_localization::normalize_configured_locale(&value))
            .is_some_and(|tag| tag != "auto")
    })
}

/// A POSIX/C locale names an encoding, not a language: `C` and `C.UTF-8`
/// pass through `normalize_configured_locale` as concrete tags, so the
/// inference gate must reject them explicitly or a stock terminal
/// environment reads as a confident language pick.
fn locale_var_names_a_language(value: &str) -> bool {
    let language = value.split(['.', '_', '@']).next().unwrap_or_default();
    !matches!(language, "" | "C" | "POSIX" | "c" | "posix")
}

/// The example task the ready screen seeds into the composer, chosen from
/// what is cheaply visible in the workspace.
pub fn first_task_seed(workspace: &Path, locale: codewhale_localization::Locale) -> String {
    let id = if CODE_PROJECT_MARKERS
        .iter()
        .any(|marker| workspace.join(marker).is_file())
    {
        MessageId::OnboardSeedCodeProject
    } else {
        MessageId::OnboardSeedFolder
    };
    codewhale_localization::tr(locale, id).into_owned()
}

/// Welcome → the first decision this run actually needs.
pub fn advance_onboarding_from_welcome(app: &mut App) {
    app.status_message = None;
    app.onboarding = if app.onboarding_had_language_step {
        OnboardingState::Language
    } else if app.onboarding_needs_api_key {
        OnboardingState::Provider
    } else if trust_decision_required(app) {
        OnboardingState::TrustDirectory
    } else {
        OnboardingState::Ready
    };
}

/// Language → the next decision; the language step never repeats.
pub fn advance_onboarding_after_language(app: &mut App) {
    app.status_message = None;
    app.onboarding = if app.onboarding_needs_api_key {
        OnboardingState::Provider
    } else if trust_decision_required(app) {
        OnboardingState::TrustDirectory
    } else {
        OnboardingState::Ready
    };
}

/// Provider setup → trust when a decision is required, otherwise the ready
/// screen. A message already typed skips the ready screen and returns to the
/// composer with the draft intact.
pub fn advance_onboarding_after_provider(app: &mut App) {
    app.status_message = None;
    if trust_decision_required(app) {
        app.onboarding = OnboardingState::TrustDirectory;
    } else if !app.composer.input.trim().is_empty() && !app.onboarding_explore_offline {
        app.finish_onboarding_without_feature_intro();
    } else {
        app.onboarding = OnboardingState::Ready;
    }
}

/// Take the explicit "explore offline" exit advertised by Provider setup
/// (#3927).
///
/// The contract this encodes, in full:
///
/// * **No provider is selected and no route is activated.** This function must
///   never reach `switch_provider`, never persist `provider`, and never write a
///   credential. Callers pass only `&mut App`, which makes that structural.
/// * **No draft secret is owned by `App`.** The caller closes the canonical
///   picker before entering this transition, dropping its private draft.
/// * **`onboarding_needs_api_key` stays true**, because nothing was supplied.
///   The launch surface, `/setup`, and doctor keep telling the truth.
/// * **The remaining required decisions still run** — trust, then the ready
///   screen — so browsing offline is a complete first run and not an early
///   exit.
/// * Queue semantics are inherited from `offline_mode`, untouched here.
pub fn choose_offline_explore(app: &mut App) {
    app.api_key_env_only = false;
    app.onboarding_needs_api_key = true;
    app.onboarding_explore_offline = true;
    app.offline_mode = true;
    // `advance_*` clears the status bar, so the label is applied after it.
    advance_onboarding_after_provider(app);
    app.status_message = Some(
        app.tr(codewhale_localization::MessageId::OnboardOfflineNotice)
            .into_owned(),
    );
    app.needs_redraw = true;
}

/// Clear the offline-explore label once a real route is activated (#3927).
///
/// This is the *only* thing that retires the label: it is not time-based and
/// not cleared by dismissing a screen.
pub fn clear_offline_explore_on_route_activation(app: &mut App) {
    app.onboarding_explore_offline = false;
}

/// Finish first run from the ready screen and land in the real composer,
/// pre-seeded with a useful first task for this folder. Enter opens the
/// product; it never opens another educational surface.
pub fn finish_ready_and_open_composer(app: &mut App) {
    app.finish_onboarding_without_feature_intro();
    if app.composer.input.trim().is_empty() {
        let seed = first_task_seed(&app.workspace, app.ui_locale);
        app.composer.input = seed;
        app.composer.cursor_position = app.composer.input.chars().count();
    }
    app.needs_redraw = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions};
    use codewhale_localization::{Locale, MessageId, tr};
    use std::path::PathBuf;

    /// A first-run app with the onboarding decision reset to "nothing asked
    /// yet". `App::new` derives that decision from the ambient machine —
    /// inferable locale, an existing `settings.toml`, a provider key in the
    /// environment — so a fixture that overrides only the flags it names
    /// inherits the rest of the developer's box and asserts something
    /// different in CI. Every test below opts in to the steps it is about.
    fn test_app_with_locale(locale: Locale) -> App {
        let options = TuiOptions {
            ..crate::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = locale;
        app.onboarding_needs_api_key = false;
        app.onboarding_missing_key_recovery = false;
        app.onboarding_explore_offline = false;
        app.onboarding_had_language_step = false;
        app.onboarding_had_provider_step = false;
        app.onboarding_had_trust_step = false;
        app
    }

    fn flattened(lines: Vec<Line<'static>>) -> String {
        lines
            .into_iter()
            .flat_map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── Navigation: one decision per screen, conditional ─────────────────

    #[test]
    fn welcome_routes_to_the_first_decision_this_run_needs() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Everything configured: welcome → ready with zero questions between.
        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = true;
        app.onboarding_needs_api_key = false;
        app.onboarding_had_language_step = false;
        advance_onboarding_from_welcome(&mut app);
        assert_eq!(app.onboarding, OnboardingState::Ready);

        // No route configured: provider comes first.
        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = true;
        app.onboarding_needs_api_key = true;
        advance_onboarding_from_welcome(&mut app);
        assert_eq!(app.onboarding, OnboardingState::Provider);

        // Route present, workspace untrusted: trust is the only decision.
        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = false;
        app.onboarding_needs_api_key = false;
        advance_onboarding_from_welcome(&mut app);
        assert_eq!(app.onboarding, OnboardingState::TrustDirectory);

        // Language cannot be inferred: it precedes every other decision.
        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = false;
        app.onboarding_needs_api_key = true;
        app.onboarding_had_language_step = true;
        advance_onboarding_from_welcome(&mut app);
        assert_eq!(app.onboarding, OnboardingState::Language);
    }

    #[test]
    fn language_step_never_repeats_and_falls_through_to_ready() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = true;
        app.onboarding_had_language_step = true;
        app.onboarding_needs_api_key = false;

        advance_onboarding_after_language(&mut app);
        assert_eq!(app.onboarding, OnboardingState::Ready);
    }

    #[test]
    fn provider_step_routes_to_trust_only_when_a_decision_is_required() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = false;
        app.onboarding_missing_key_recovery = false;
        advance_onboarding_after_provider(&mut app);
        assert_eq!(app.onboarding, OnboardingState::TrustDirectory);

        let mut trusted = test_app_with_locale(Locale::En);
        trusted.workspace = tmp.path().to_path_buf();
        trusted.trust_mode = true;
        advance_onboarding_after_provider(&mut trusted);
        assert_eq!(trusted.onboarding, OnboardingState::Ready);
    }

    #[test]
    fn missing_key_recovery_ends_on_ready_like_a_first_run() {
        let mut app = test_app_with_locale(Locale::En);
        app.trust_mode = true;
        app.onboarding_missing_key_recovery = true;
        advance_onboarding_after_provider(&mut app);
        assert_eq!(app.onboarding, OnboardingState::Ready);
    }

    #[test]
    fn explore_offline_still_traverses_trust_then_ready() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = test_app_with_locale(Locale::En);
        app.onboarding = OnboardingState::Provider;
        app.trust_mode = false;
        app.workspace = tmp.path().to_path_buf();

        choose_offline_explore(&mut app);
        assert_eq!(app.onboarding, OnboardingState::TrustDirectory);
        assert!(app.onboarding_explore_offline);

        // A trusted workspace skips only the trust screen, never the ending.
        let mut trusted = test_app_with_locale(Locale::En);
        trusted.onboarding = OnboardingState::Provider;
        trusted.trust_mode = true;
        choose_offline_explore(&mut trusted);
        assert_eq!(trusted.onboarding, OnboardingState::Ready);
    }

    #[test]
    fn offline_explore_selects_no_provider_and_writes_no_credential() {
        let mut app = test_app_with_locale(Locale::En);
        app.onboarding = OnboardingState::Provider;
        app.onboarding_needs_api_key = true;
        app.trust_mode = true;
        let provider_before = app.api_provider;
        let model_before = app.model.clone();

        choose_offline_explore(&mut app);

        assert_eq!(app.api_provider, provider_before);
        assert_eq!(app.model, model_before);
        assert!(!app.api_key_env_only);
        assert!(app.onboarding_needs_api_key);
        assert!(app.onboarding_explore_offline);
        assert!(app.offline_mode);
    }

    #[test]
    fn offline_label_only_clears_when_a_route_is_activated() {
        let mut app = test_app_with_locale(Locale::En);
        app.trust_mode = true;
        choose_offline_explore(&mut app);
        assert!(app.onboarding_explore_offline);

        clear_offline_explore_on_route_activation(&mut app);
        assert!(!app.onboarding_explore_offline);
    }

    // ── Conditional required steps: the counter ──────────────────────────

    #[test]
    fn progress_counts_only_required_decisions() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // One required decision → no counter at all.
        let mut app = test_app_with_locale(Locale::En);
        app.workspace = tmp.path().to_path_buf();
        app.trust_mode = true;
        app.onboarding_needs_api_key = true;
        app.onboarding = OnboardingState::Provider;
        assert_eq!(required_progress(&app), None);

        // Language + provider: the language screen is 1 of 2.
        app.onboarding_had_language_step = true;
        app.onboarding_had_provider_step = true;
        app.onboarding = OnboardingState::Language;
        assert_eq!(required_progress(&app), Some((1, 2)));
        app.onboarding = OnboardingState::Provider;
        assert_eq!(required_progress(&app), Some((2, 2)));

        // Completing provider setup changes live route state, but never
        // rewrites the receipt-backed denominator for this run.
        app.onboarding_needs_api_key = false;
        app.onboarding_had_trust_step = true;
        app.onboarding = OnboardingState::TrustDirectory;
        assert_eq!(required_progress(&app), Some((3, 3)));

        // Welcome and ready are not decisions and never carry a counter.
        app.onboarding = OnboardingState::Welcome;
        assert_eq!(required_progress(&app), None);
        app.onboarding = OnboardingState::Ready;
        assert_eq!(required_progress(&app), None);
    }

    // ── Language inference gate ───────────────────────────────────────────

    #[test]
    fn language_step_is_required_only_when_the_locale_is_not_inferable() {
        let _env_lock = crate::test_support::lock_test_env();
        let _guard = crate::test_support::EnvVarGuard::remove("LC_ALL");
        let _messages = crate::test_support::EnvVarGuard::remove("LC_MESSAGES");
        let _lang = crate::test_support::EnvVarGuard::remove("LANG");

        assert!(!locale_confidently_inferred("auto"));
        assert!(!locale_confidently_inferred(""));

        // An explicit settings pick is always confident.
        assert!(locale_confidently_inferred("ja"));
        assert!(locale_confidently_inferred("zh-Hans"));

        // A shipped locale in the environment is confident…
        let _lang = crate::test_support::EnvVarGuard::set("LANG", "ja_JP.UTF-8");
        assert!(locale_confidently_inferred("auto"));

        // …but a POSIX/C environment is not a language signal.
        let _lang = crate::test_support::EnvVarGuard::set("LANG", "C");
        assert!(!locale_confidently_inferred("auto"));
        let _lang = crate::test_support::EnvVarGuard::set("LANG", "C.UTF-8");
        assert!(!locale_confidently_inferred("auto"));
    }

    // ── The seeded first task ────────────────────────────────────────────

    #[test]
    fn seed_speaks_to_the_workspace_contents() {
        let code_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(code_dir.path().join("Cargo.toml"), "[package]\n").expect("marker");
        assert_eq!(
            first_task_seed(code_dir.path(), Locale::En),
            tr(Locale::En, MessageId::OnboardSeedCodeProject)
        );

        let plain_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(plain_dir.path().join("README.md"), "notes\n").expect("readme");
        assert_eq!(
            first_task_seed(plain_dir.path(), Locale::En),
            tr(Locale::En, MessageId::OnboardSeedFolder)
        );
    }

    #[test]
    fn finishing_from_ready_seeds_the_composer_and_marks_onboarding_done() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", home.path());
        let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", home.path());
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());

        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("package.json"), "{}\n").expect("marker");

        let mut app = test_app_with_locale(Locale::En);
        app.workspace = workspace.path().to_path_buf();
        app.onboarding = OnboardingState::Ready;

        finish_ready_and_open_composer(&mut app);

        assert_eq!(app.onboarding, OnboardingState::None);
        assert!(is_onboarded(), "the ready screen completes first run");
        assert_eq!(
            app.composer.input,
            tr(Locale::En, MessageId::OnboardSeedCodeProject)
        );
        assert_eq!(
            app.composer.cursor_position,
            app.composer.input.chars().count()
        );
    }

    #[test]
    fn finishing_from_ready_preserves_a_real_cli_prompt() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("HOME", home.path());
        let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", home.path());
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());

        let mut app = test_app_with_locale(Locale::En);
        app.onboarding = OnboardingState::Ready;
        app.composer.input = "Fix the failing build I asked for.".to_string();
        app.composer.cursor_position = app.composer.input.chars().count();

        finish_ready_and_open_composer(&mut app);

        assert_eq!(app.onboarding, OnboardingState::None);
        assert_eq!(app.composer.input, "Fix the failing build I asked for.");
        assert_eq!(
            app.composer.cursor_position,
            app.composer.input.chars().count()
        );
    }

    // ── Onboarded-state persistence contract ─────────────────────────────

    #[test]
    fn fresh_install_marker_path_uses_codewhale_not_legacy() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let expected = tmp.path().join(".codewhale").join(ONBOARDED_MARKER_FILE);
        assert_eq!(marker_path_with_home(tmp.path()), expected);

        let written = mark_onboarded_at_home(tmp.path()).expect("mark onboarded");
        assert_eq!(written, expected);
        assert!(expected.exists());
        assert!(
            !tmp.path().join(".deepseek").exists(),
            "fresh onboarding must not recreate the legacy .deepseek dir"
        );
    }

    #[test]
    fn existing_legacy_marker_is_preserved() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join(".deepseek").join(ONBOARDED_MARKER_FILE);
        std::fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("mkdir legacy");
        std::fs::write(&legacy, "").expect("seed legacy marker");

        assert_eq!(marker_path_with_home(tmp.path()), legacy);
        assert_eq!(
            mark_onboarded_at_home(tmp.path()).expect("mark onboarded"),
            legacy
        );
    }

    #[test]
    fn codewhale_marker_wins_over_legacy_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join(".codewhale").join(ONBOARDED_MARKER_FILE);
        let legacy = tmp.path().join(".deepseek").join(ONBOARDED_MARKER_FILE);
        for marker in [&primary, &legacy] {
            std::fs::create_dir_all(marker.parent().expect("marker parent")).expect("mkdir");
            std::fs::write(marker, "").expect("seed marker");
        }

        assert_eq!(marker_path_with_home(tmp.path()), primary);
    }

    #[test]
    fn explicit_codewhale_home_marker_survives_restart_resolution() {
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ambient_home = tmp.path().join("ambient profile");
        let isolated_home = tmp.path().join("isolated Codewhale state");
        let ambient_legacy = ambient_home.join(".deepseek").join(ONBOARDED_MARKER_FILE);
        std::fs::create_dir_all(ambient_legacy.parent().expect("legacy parent")).expect("mkdir");
        std::fs::write(&ambient_legacy, "").expect("seed ambient legacy marker");
        let _home = crate::test_support::EnvVarGuard::set("HOME", &ambient_home);
        let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", &ambient_home);
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &isolated_home);

        let expected = isolated_home.join(ONBOARDED_MARKER_FILE);
        assert_eq!(default_marker_path().as_deref(), Some(expected.as_path()));
        assert!(!is_onboarded());

        let written = mark_onboarded().expect("mark onboarded");

        assert_eq!(written, expected);
        assert!(is_onboarded());
        assert_eq!(default_marker_path().as_deref(), Some(expected.as_path()));
        assert!(ambient_legacy.exists(), "legacy marker remains untouched");
        assert!(
            !ambient_home.join(".codewhale").exists(),
            "an explicit state root must not write into the ambient profile"
        );
    }

    // ── Locale completeness for the new copy ─────────────────────────────

    #[test]
    fn calm_onboarding_copy_is_translated_in_every_complete_pack() {
        for locale in Locale::shipped_complete() {
            for id in [
                MessageId::OnboardWelcomeTitle,
                MessageId::OnboardWelcomeLead,
                MessageId::OnboardWelcomeBegin,
                MessageId::OnboardActionBack,
                MessageId::OnboardActionExit,
                MessageId::OnboardStepsTitle,
                MessageId::OnboardLanguagePick,
                MessageId::OnboardLanguageKeep,
                MessageId::OnboardProviderChoose,
                MessageId::OnboardProviderOffline,
                MessageId::OnboardTrustActionTrust,
                MessageId::OnboardTrustActionSkip,
                MessageId::OnboardTrustActionQuit,
                MessageId::OnboardReadyTitle,
                MessageId::OnboardReadyLead,
                MessageId::OnboardReadyStart,
                MessageId::OnboardReadyCustomize,
                MessageId::CmdRemoteControlDescription,
                MessageId::OnboardSeedCodeProject,
                MessageId::OnboardSeedFolder,
            ] {
                let text = tr(*locale, id);
                assert!(!text.is_empty(), "{locale:?} {id:?} is empty");
                if *locale != Locale::En {
                    assert_ne!(
                        text,
                        tr(Locale::En, id),
                        "{locale:?} {id:?} silently fell back to English"
                    );
                }
            }
        }
    }

    #[test]
    fn ready_screen_advertises_only_keys_it_handles() {
        use crate::tui::views::action_footer_lines;

        let mut app = test_app_with_locale(Locale::En);
        app.onboarding = OnboardingState::Ready;
        let rail = flattened(action_footer_lines(&action_hints(&app), 80));
        assert!(rail.contains("Enter"), "{rail}");
        assert!(rail.contains("change the look"), "{rail}");
        assert!(
            !rail.contains("/rc"),
            "the Ready screen owns the keyboard, so /rc cannot be typed: {rail}"
        );
    }

    #[test]
    fn provider_screen_advertises_the_offline_choice() {
        use crate::tui::views::action_footer_lines;

        let mut app = test_app_with_locale(Locale::En);
        app.onboarding = OnboardingState::Provider;
        let rail = flattened(action_footer_lines(&action_hints(&app), 70));
        assert!(rail.contains("Enter"), "primary choice first: {rail}");
        assert!(
            rail.contains("Ctrl+O"),
            "offline exit must be advertised: {rail}"
        );
        assert!(
            rail.contains(tr(Locale::En, MessageId::OnboardProviderOffline).as_ref()),
            "offline exit needs its translated label: {rail}"
        );
    }

    #[test]
    fn wrap_words_breaks_scripts_that_have_no_spaces() {
        use unicode_width::UnicodeWidthStr;
        // The real ja provider blurb. `split_whitespace` yields one token, so
        // the word-only wrapper emitted a single 110-column line and an 80-column
        // terminal clipped everything after "ローカ" — the half of the sentence
        // that tells the user local runtimes need no key.
        let ja = "モデルの実行先を選びます。ホステッドプロバイダーにはキーが必要ですが、ローカルランタイムはキーなしで続行できます。";
        assert!(
            ja.split_whitespace().count() == 1,
            "fixture must be a single whitespace-delimited token"
        );

        let lines = wrap_words(ja, 76);
        assert!(
            lines.len() > 1,
            "space-less text must wrap, not clip: {lines:?}"
        );
        for line in &lines {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 76,
                "line exceeds the lane: {:?} ({} cols)",
                line,
                UnicodeWidthStr::width(line.as_str())
            );
        }
        // Nothing may be dropped: the rejoined lines must reproduce the source.
        assert_eq!(lines.concat(), ja, "wrapping must not lose characters");
    }

    #[test]
    fn wrap_words_keeps_latin_wrapping_unchanged() {
        let text = "Pick where your model runs. Hosted providers need a key; local runtimes can continue without one.";
        let lines = wrap_words(text, 60);
        assert!(lines.len() >= 2);
        for line in &lines {
            assert!(line.len() <= 60, "{line:?}");
            assert!(!line.starts_with(' ') && !line.ends_with(' '), "{line:?}");
        }
        assert_eq!(lines.join(" "), text, "word wrapping must round-trip");
    }

    #[test]
    fn wrap_words_does_not_open_a_line_with_japanese_closing_punctuation() {
        // Kinsoku: 。、」） and friends may not begin a line.
        let text = "あいうえおかきくけこさしすせそたちつてと。";
        for width in 4..=20 {
            for line in wrap_words(text, width) {
                let first = line.chars().next().unwrap();
                assert!(
                    !NO_LINE_START.contains(&first),
                    "width {width}: line starts with {first:?} in {line:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_words_handles_mixed_latin_and_cjk() {
        let text = "Codewhale はこのフォルダーで一緒に作業します。";
        let lines = wrap_words(text, 20);
        assert_eq!(
            lines.join("").replace(' ', ""),
            text.replace(' ', ""),
            "mixed-script text must not lose characters: {lines:?}"
        );
    }
}
