//! Golden-buffer contract for the posture bar — the first row under the
//! composer. Goldens: `footer_{w}x{h}` — the one-row band at the bottom of
//! each blocker-size buffer. Re-bless by DELETING the golden and running
//! with `CODEWHALE_BLESS_GOLDENS=1`.

use ratatui::layout::Rect;
use unicode_width::UnicodeWidthChar;

use super::{ChromeInk, TidelineFooter, render_tideline_footer};
use crate::tui::golden_harness::{BLOCKER_SIZES, assert_matches_golden, render_golden_text};
use codewhale_palette::UI_THEME;

struct Fixture {
    permission: (&'static str, ChromeInk),
    permission_key: Option<&'static str>,
    mode: Option<(&'static str, ChromeInk)>,
    mode_key: Option<&'static str>,
    turn_clock: Option<(&'static str, ChromeInk)>,
    counts: Vec<(String, ChromeInk)>,
    session_clock: Option<(&'static str, ChromeInk)>,
    hint: Option<(&'static str, ChromeInk)>,
    context_percent: u8,
    right: Option<(&'static str, ChromeInk)>,
}

/// A working turn with two sub-agents, ask posture, work mode.
fn working() -> Fixture {
    Fixture {
        permission: ("ask", ChromeInk::PermissionAsk),
        permission_key: Some("Shift+Tab to change"),
        mode: Some(("work", ChromeInk::PolicyAct)),
        mode_key: Some("Tab"),
        turn_clock: Some(("working 1m 15s", ChromeInk::Active)),
        counts: vec![("2 agents".to_string(), ChromeInk::Active)],
        session_clock: Some(("worked 41m 12s", ChromeInk::Active)),
        hint: Some(("Esc to interrupt", ChromeInk::MetadataHint)),
        context_percent: 61,
        right: None,
    }
}

impl Fixture {
    fn widget<'a>(&'a self, theme: &'a codewhale_palette::UiTheme) -> TidelineFooter<'a> {
        TidelineFooter::new(theme, self.permission)
            .permission_key(self.permission_key)
            .mode_chip(self.mode)
            .mode_key(self.mode_key)
            .turn_clock(self.turn_clock)
            .counts(&self.counts)
            .session_clock(self.session_clock)
            .hint(self.hint)
            .context_percent(self.context_percent)
            .right(self.right)
    }
}

fn draw(width: u16, height: u16, footer: &TidelineFooter<'_>) -> String {
    render_golden_text(width, height, |buf| {
        // The shell reserves exactly one row for the bar.
        render_tideline_footer(
            Rect::new(0, height.saturating_sub(1), width, 1),
            buf,
            footer,
        );
    })
}

#[test]
fn footer_matches_goldens_at_blocker_sizes() {
    let fixture = working();
    for (w, h) in BLOCKER_SIZES {
        let footer = fixture.widget(&UI_THEME);
        assert_matches_golden(&format!("footer_{w}x{h}"), &draw(w, h, &footer));
    }
}

/// Permission chip with its cycle key, mode
/// with its cycle key, the working clock, live counts, then the one hint
/// that applies now.
#[test]
fn posture_bar_reads_permission_mode_clock_counts_hint() {
    let text = draw(120, 30, &working().widget(&UI_THEME));
    let band = text.lines().last().unwrap_or_default().trim_end();
    assert_eq!(
        band,
        " ● ask  Shift+Tab to change   work (Tab)   working 1m 15s   2 agents   worked 41m 12s   Esc to interrupt"
    );
}

/// Experience mark 8: the current permission reads without color. With every
/// ink stripped the row still marks which chip is the permission in force
/// (`●`) and says what its key does, instead of printing two look-alike
/// `word (key)` chips that read as a menu of options.
#[test]
fn current_permission_is_identifiable_without_color() {
    for (w, h) in BLOCKER_SIZES {
        let text = draw(w, h, &working().widget(&UI_THEME));
        let band = text.lines().last().unwrap_or_default();
        assert!(band.starts_with(" ● ask"), "{w}x{h}: {band}");
        assert_eq!(band.matches('●').count(), 1, "{w}x{h}: {band}");
        assert!(band.contains("Shift+Tab to change"), "{w}x{h}: {band}");
        assert!(!band.contains("(Shift+Tab)"), "{w}x{h}: {band}");
    }
}

/// The bar carries no cost and no context reading: the metrics line owns
/// both. The elapsed reading is this bar's again (#5914) — a fixed row is
/// the only place a glancing user can find it during a multi-hour session —
/// so it is asserted here rather than excluded.
#[test]
fn posture_bar_states_no_cost_or_context_reading() {
    for pct in [0u8, 12, 61, 79] {
        let mut fixture = working();
        fixture.context_percent = pct;
        let text = draw(120, 32, &fixture.widget(&UI_THEME));
        assert!(!text.contains(&format!("{pct}%")), "{pct}: {text}");
        assert!(!text.contains("<·>"), "{text}");
        assert!(!text.contains('$'), "{text}");
        assert!(text.contains("working 1m 15s"), "{text}");
        assert!(text.contains("worked 41m 12s"), "{text}");
        assert!(text.contains("2 agents"), "{text}");
    }
}

/// At the cap the bar's hint slot says what to do, outranking the
/// interrupt hint; the reading itself still is not here.
#[test]
fn posture_bar_warns_at_eighty_percent_cap() {
    let mut fixture = working();
    fixture.context_percent = 83;
    let text = draw(100, 30, &fixture.widget(&UI_THEME));
    assert!(text.contains("▲ surface soon — /compact"), "{text}");
    assert!(!text.contains("Esc to interrupt"), "{text}");
    assert!(!text.contains("83%"), "{text}");
}

/// The cap warning outranks the clock: a working session that cannot start
/// its next turn needs the instruction more than it needs the stopwatch.
#[test]
fn cap_warning_outranks_the_clock_when_only_one_fits() {
    let mut fixture = working();
    fixture.context_percent = 83;
    let mut saw_warning_alone = false;
    for width in 8..=160u16 {
        let text = draw(width, 12, &fixture.widget(&UI_THEME));
        let warning = text.contains("surface soon");
        let clock = text.contains("worked 41m 12s");
        assert!(
            !(clock && !warning),
            "width {width} kept the clock and shed the cap warning: {text}"
        );
        if warning && !clock {
            saw_warning_alone = true;
        }
    }
    assert!(saw_warning_alone, "no width shed the clock alone");
}

/// The right slot is the notice when one is owed, else the remote-control
/// state; it never covers the permission chip.
#[test]
fn posture_bar_pins_notice_or_remote_control_right() {
    let mut fixture = working();
    fixture.right = Some(("/rc connected", ChromeInk::Info));
    let text = draw(100, 30, &fixture.widget(&UI_THEME));
    assert!(text.trim_end().ends_with("/rc connected"), "{text}");
    assert!(text.contains(" ● ask  Shift+Tab to change"), "{text}");

    fixture.right = Some(("Auto-denied exec_shell", ChromeInk::Attention));
    let text = draw(100, 30, &fixture.widget(&UI_THEME));
    assert!(
        text.trim_end().ends_with("Auto-denied exec_shell"),
        "{text}"
    );

    // Narrow: the notice truncates against the permission floor, never
    // over it.
    let narrow = draw(30, 12, &fixture.widget(&UI_THEME));
    assert!(narrow.contains(" ask"), "{narrow}");
}

/// Shed ladder, most expendable first: the turn clock, the session clock,
/// the permission key (a binding reminder, and the widest optional item),
/// the hint, the counts, mode key, mode. The permission chip
/// never sheds (#5796); the clock is what a glance wants and the hint and
/// counts are what a keystroke wants, so on a row too narrow for both the
/// clock goes (#5914).
#[test]
fn posture_bar_sheds_the_clocks_then_the_hint_counts_and_posture_chips() {
    let fixture = working();
    let narrowest_showing = |needle: &str| -> u16 {
        (8..=160u16)
            .filter(|w| draw(*w, 3, &fixture.widget(&UI_THEME)).contains(needle))
            .min()
            .unwrap_or_else(|| panic!("{needle} never painted"))
    };
    let turn_clock = narrowest_showing("working 1m 15s");
    let session_clock = narrowest_showing("worked 41m 12s");
    let hint = narrowest_showing("Esc to interrupt");
    let counts = narrowest_showing("2 agents");
    // `work` alone would also match `working`, so measure the mode chip by
    // a needle only the mode paints.
    let mode_key = narrowest_showing("work (Tab)");
    let mode = narrowest_showing("   work ");
    let permission_key = narrowest_showing("Shift+Tab to change");
    assert!(
        turn_clock > session_clock
            && session_clock > permission_key
            && permission_key > hint
            && hint > counts
            && counts > mode_key
            && mode_key > mode,
        "turn_clock@{turn_clock} session_clock@{session_clock} hint@{hint} counts@{counts} mode_key@{mode_key} mode@{mode} permission_key@{permission_key}"
    );
    for w in 8..=160u16 {
        let text = draw(w, 3, &fixture.widget(&UI_THEME));
        assert!(
            text.contains("ask"),
            "{w}: the permission chip never sheds: {text}"
        );
        // Whole chips or none — a clipped posture word is worse than none.
        assert!(
            !text.contains("(Ta") || text.contains("(Tab)"),
            "{w}: {text}"
        );
    }
}

/// `tui.posture_bar = "compact"` (#5950) starts the ladder past the clocks,
/// the hint and the counts at any width: the row states its posture — the
/// permission and mode chips, and the cap warning when it is owed — and
/// nothing live. Width still sheds from there, and the right slot is
/// untouched.
#[test]
fn compact_posture_bar_states_posture_and_nothing_live() {
    let mut fixture = working();
    fixture.right = Some(("/rc connected", ChromeInk::Info));
    let wide = draw(160, 3, &fixture.widget(&UI_THEME).compact(true));
    for kept in [" ● ask", "   work (Tab)", "/rc connected"] {
        assert!(wide.contains(kept), "compact keeps {kept}: {wide}");
    }
    for gone in [
        "working 1m 15s",
        "worked 41m 12s",
        "Shift+Tab to change",
        "2 agents",
        "Esc to interrupt",
    ] {
        assert!(!wide.contains(gone), "compact drops {gone}: {wide}");
    }
    // The full row at the same width is the row the user had before.
    assert!(draw(160, 3, &fixture.widget(&UI_THEME)).contains("working 1m 15s"));

    // The cap warning is not a hint: a compact row still says what to do
    // about a full context.
    fixture.context_percent = 85;
    let capped = draw(160, 3, &fixture.widget(&UI_THEME).compact(true));
    assert!(capped.contains("surface soon"), "{capped}");
    assert!(!capped.contains("Esc to interrupt"), "{capped}");

    for w in 8..=160u16 {
        let text = draw(w, 3, &fixture.widget(&UI_THEME).compact(true));
        assert!(
            text.contains("ask"),
            "{w}: the permission chip never sheds: {text}"
        );
        assert!(
            !text.contains("working") && !text.contains("agents"),
            "{w}: nothing live in a compact row: {text}"
        );
    }
}

/// Permission outranks mode when only one posture chip fits: the longest
/// mode word must never displace `full access`.
#[test]
fn posture_bar_permission_outranks_mode_when_only_one_fits() {
    let mut fixture = working();
    fixture.permission = ("full access", ChromeInk::PermissionFullAccess);
    fixture.mode = Some(("operate", ChromeInk::PolicyOperate));
    let mut saw_permission_alone = false;
    for width in 8..=120u16 {
        let text = draw(width, 12, &fixture.widget(&UI_THEME));
        let has_mode = text.contains("operate");
        let has_permission = text.contains("full access");
        assert!(
            !(has_mode && !has_permission),
            "width {width} kept the mode word and shed the permission phrase: {text}"
        );
        if has_permission && !has_mode {
            saw_permission_alone = true;
        }
    }
    assert!(saw_permission_alone, "no width shed the mode word alone");
}

/// The cycle keys print only when the caller says the binding is live —
/// the launch stage's Tab moves focus, so the mode chip there has no key.
#[test]
fn posture_bar_prints_cycle_keys_only_when_live() {
    let mut fixture = working();
    fixture.mode_key = None;
    fixture.permission_key = None;
    let text = draw(120, 30, &fixture.widget(&UI_THEME));
    assert!(
        text.contains(" ● ask   work   working 1m 15s   2 agents   worked 41m 12s"),
        "{text}"
    );
    assert!(!text.contains('('), "{text}");
}

#[test]
fn posture_bar_ascii_safe_projects_glyphs() {
    let mut fixture = working();
    fixture.context_percent = 90;
    let text = draw(100, 30, &fixture.widget(&UI_THEME).ascii_safe(true));
    let band = text.lines().last().unwrap_or_default();
    assert!(band.starts_with(" . ask"), "inset preserved: {band}");
    assert!(text.contains("^ surface soon"), "{text}");
    for ch in text.chars() {
        if ch != '\n' {
            assert_eq!(ch.width(), Some(1), "ascii-safe single-width: {ch:?}");
        }
    }
}

#[test]
fn posture_bar_degenerate_sizes_do_not_panic() {
    for (w, h) in [(0u16, 0), (2, 1), (8, 1), (300, 2)] {
        let fixture = working();
        let _ = draw(w, h, &fixture.widget(&UI_THEME));
    }
}

// ---------------------------------------------------------------------------
// Footer-hint retirement: each hint shows at 0 and 1 uses, and is gone at 2.
// These drive `tideline_footer_from_app` with a live session `App`, so the
// gating between the use counts and the facts is covered, not just the
// predicate in `footer_hints`.
// ---------------------------------------------------------------------------

use super::tideline_footer_from_app;
use crate::tui::app::{App, OnboardingState};
use crate::tui::footer_hints::{
    AGENT_ARROWS, ENTER_AGAIN, ESC_INTERRUPT, MODE_CYCLE, PERMISSION_CYCLE,
};

fn session_app() -> App {
    let mut app = crate::test_support::test_app_with_options(
        crate::test_support::test_tui_options(std::path::PathBuf::from(".")),
    );
    app.onboarding = OnboardingState::None;
    app.launch.visible = false;
    app
}

fn set_uses(app: &mut App, key: &str, uses: u8) {
    if uses == 0 {
        app.footer_hint_uses.remove(key);
    } else {
        app.footer_hint_uses.insert(key.to_string(), uses);
    }
}

/// The cycle chords print while their bindings are fresh, and the chips go
/// bare — never away — once each binding has been used twice.
#[test]
fn cycle_keys_show_at_zero_and_one_use_and_go_bare_at_two() {
    let mut app = session_app();
    app.approval_mode = codewhale_execpolicy::ApprovalMode::Suggest;
    let facts = tideline_footer_from_app(&mut app, 120);
    assert_eq!(facts.permission_key.as_deref(), Some("Shift+Tab to change"));
    assert_eq!(facts.mode_key, Some("Tab"));

    for key in [PERMISSION_CYCLE, MODE_CYCLE] {
        set_uses(&mut app, key, 1);
    }
    let facts = tideline_footer_from_app(&mut app, 120);
    assert_eq!(facts.permission_key.as_deref(), Some("Shift+Tab to change"));
    assert_eq!(facts.mode_key, Some("Tab"));

    set_uses(&mut app, PERMISSION_CYCLE, 2);
    let facts = tideline_footer_from_app(&mut app, 120);
    assert_eq!(facts.permission_key, None);
    assert_eq!(
        facts.mode_key,
        Some("Tab"),
        "the mode key retires on its own count"
    );

    set_uses(&mut app, MODE_CYCLE, 2);
    let facts = tideline_footer_from_app(&mut app, 120);
    assert_eq!(facts.permission_key, None);
    assert_eq!(facts.mode_key, None);
    assert!(
        !facts.permission_chip.0.is_empty(),
        "the permission chip never retires with its key"
    );
    assert!(
        facts.mode_chip.is_some(),
        "the mode chip never retires with its key"
    );
}

/// A running turn advertises the interrupt affordance until Esc has been
/// used to interrupt twice.
#[test]
fn interrupt_hint_shows_at_zero_and_one_use_and_clears_at_two() {
    let mut app = session_app();
    app.is_loading = true;
    let facts = tideline_footer_from_app(&mut app, 120);
    let (text, _) = facts.hint.as_ref().expect("a running turn names Esc");
    assert!(text.contains("Esc"), "{text}");

    set_uses(&mut app, ESC_INTERRUPT, 1);
    assert!(tideline_footer_from_app(&mut app, 120).hint.is_some());

    set_uses(&mut app, ESC_INTERRUPT, 2);
    assert!(tideline_footer_from_app(&mut app, 120).hint.is_none());
}

/// #6502: while the workbar owns focus, Esc closes the workbar instead of
/// interrupting, so the posture bar must not promise an interrupt.
#[test]
fn interrupt_hint_stands_down_while_the_workbar_owns_esc() {
    let mut app = session_app();
    app.is_loading = true;
    app.work_surface.focused = true;
    let facts = tideline_footer_from_app(&mut app, 120);
    assert!(
        facts
            .hint
            .as_ref()
            .is_none_or(|(text, _)| !text.contains("Esc")),
        "{:?}",
        facts.hint.as_ref().map(|(text, _)| text)
    );
}

/// The open double-tap window advertises the second Enter until that steer
/// has fired twice.
#[test]
fn enter_again_hint_shows_at_zero_and_one_use_and_clears_at_two() {
    let mut app = session_app();
    app.is_loading = true;
    app.arm_double_tap_window();
    let facts = tideline_footer_from_app(&mut app, 120);
    let (text, _) = facts
        .hint
        .as_ref()
        .expect("an open double-tap window names Enter");
    assert!(text.contains("Enter"), "{text}");

    set_uses(&mut app, ENTER_AGAIN, 1);
    assert!(tideline_footer_from_app(&mut app, 120).hint.is_some());

    set_uses(&mut app, ENTER_AGAIN, 2);
    assert!(tideline_footer_from_app(&mut app, 120).hint.is_none());
}

fn completed_subagent(id: &str) -> crate::tools::subagent::SubAgentResult {
    crate::tools::subagent::SubAgentResult {
        usage: None,
        name: id.to_string(),
        agent_id: id.to_string(),
        context_mode: "fresh".to_string(),
        fork_context: false,
        workspace: None,
        git_branch: None,
        agent_type: crate::tools::subagent::FleetRole::Worker,
        assignment: crate::tools::subagent::SubAgentAssignment {
            native_preset: None,
            objective: format!("objective-{id}"),
            role: Some("worker".to_string()),
        },
        model: String::new(),
        nickname: None,
        status: crate::tools::subagent::SubAgentStatus::Completed,
        worker_status: None,
        runtime_permissions: None,
        parent_run_id: None,
        spawn_depth: 0,
        child_route: None,
        result: None,
        steps_taken: 0,
        checkpoint: None,
        needs_input: None,
        duration_ms: 0,
        started_at: None,
        from_prior_session: false,
        idle_ms: None,
        heartbeat_timeout_ms: None,
    }
}

/// The empty composer lends its arrows to the roster until those shortcuts
/// have been used twice. The roster entry is a finished agent: live progress
/// would put the phase back to Working, where the interrupt hint outranks.
#[test]
fn agent_arrow_hints_show_at_zero_and_one_use_and_clear_at_two() {
    let mut app = session_app();
    app.subagent_cache.push(completed_subagent("agent-a"));
    assert!(
        tideline_footer_from_app(&mut app, 120).hint.is_some(),
        "the empty composer lends its arrows to the roster"
    );

    set_uses(&mut app, AGENT_ARROWS, 1);
    assert!(tideline_footer_from_app(&mut app, 120).hint.is_some());

    set_uses(&mut app, AGENT_ARROWS, 2);
    assert!(tideline_footer_from_app(&mut app, 120).hint.is_none());
}

/// Esc Esc rewinds the conversation and leaves the files as they are. The
/// notice gets half the row and loses whole trailing sentences that do not
/// fit, so the file fact comes first: it is still on screen at 80 columns,
/// and the pointer to `/undo` joins it once the row is wide enough. The
/// notice used to read `Rewound to previous user message` and said nothing
/// about files.
#[test]
fn rewind_notice_states_the_file_fact_first_and_names_undo_when_it_fits() {
    use crate::tui::history::HistoryCell;
    let mut app = session_app();
    app.add_message(HistoryCell::User {
        content: "make whole hours read 2h".to_string(),
    });
    app.add_message(HistoryCell::Assistant {
        content: "Updated duration.mjs".to_string(),
        streaming: false,
    });

    crate::tui::ui::apply_backtrack(&mut app, 0);

    assert_eq!(app.input, "make whole hours read 2h");
    let shown = |app: &mut App, width: u16| {
        tideline_footer_from_app(app, width)
            .right
            .map(|(text, _)| text)
    };
    assert_eq!(
        shown(&mut app, 80).as_deref(),
        Some("Files not changed. /undo puts them back.")
    );
    assert_eq!(
        shown(&mut app, 124).as_deref(),
        Some("Files not changed. /undo puts them back. Conversation rewound.")
    );
}

// ---------------------------------------------------------------------------
// The working clock (#5914). The founder watching a multi-hour operate
// session could not find how long the thing had been working: the classic
// footer's `worked` chip went with the legacy footer path (146ab7f756) and
// the phase band's `working_detail` went with the 0.9.12 merged shell
// (329960fcbf). These pin what came back — both readings, the state word
// that says what the clock is counting, and its place in the shed ladder.
// ---------------------------------------------------------------------------

use std::time::{Duration, Instant};

use super::{ShellPhase, working_clock};

/// A live turn states both readings: what the session is doing now and for
/// how long, then how long it has worked in total.
#[test]
fn live_turn_clock_states_the_turn_and_the_session() {
    let mut app = session_app();
    app.ui_locale = codewhale_localization::Locale::En;
    app.is_loading = true;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(75));
    app.cumulative_turn_duration = Duration::from_secs(2_400);

    let facts = tideline_footer_from_app(&mut app, 160);
    let (turn, turn_ink) = facts.turn_clock.expect("a live turn states its elapsed");
    assert_eq!(turn, "working 1m 15s");
    assert_eq!(turn_ink, ChromeInk::Active);
    let (session, session_ink) = facts
        .session_clock
        .expect("a working session states its total");
    assert_eq!(session, "worked 41m 15s");
    assert_eq!(session_ink, ChromeInk::Active);
}

/// The session reading is model work, not wall clock: it is the sum of
/// finished turns plus the live one, so it never jumps at `TurnComplete`.
#[test]
fn session_reading_carries_finished_turns_plus_the_live_one() {
    let mut app = session_app();
    app.ui_locale = codewhale_localization::Locale::En;
    app.cumulative_turn_duration = Duration::from_secs(3_600);

    // Turn in flight: the finished total plus this turn.
    app.is_loading = true;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(30));
    let (_, live) = working_clock(&app, ShellPhase::Working, "working");
    assert_eq!(live.expect("live session clock").0, "worked 60m 30s");

    // Turn done: the engine folded it into the cumulative total, so the
    // reading is unchanged and the clock stops.
    app.is_loading = false;
    app.turn_started_at = None;
    app.cumulative_turn_duration = Duration::from_secs(3_630);
    let facts = tideline_footer_from_app(&mut app, 160);
    assert!(facts.turn_clock.is_none(), "no turn, no turn clock");
    let (idle, ink) = facts
        .session_clock
        .expect("an idle session still states its total");
    assert_eq!(idle, "worked 60m 30s");
    assert_eq!(ink, ChromeInk::MetadataValue, "a stopped clock reads quiet");
}

/// #6041: a session's first long turn must not print one duration twice.
/// The turn half names the phase; the session half returns only once a
/// finished turn makes the totals different.
#[test]
fn first_turn_does_not_repeat_the_turn_duration_as_a_session_total() {
    let mut app = session_app();
    app.ui_locale = codewhale_localization::Locale::En;
    app.is_loading = true;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(22 * 60 + 39));

    let facts = tideline_footer_from_app(&mut app, 160);
    assert_eq!(
        facts.turn_clock.expect("the turn clock states the phase").0,
        "working 22m 39s"
    );
    assert!(
        facts.session_clock.is_none(),
        "a session total that repeats the turn reading is furniture (#6041)"
    );

    // One finished turn later the two readings differ, and both are honest.
    app.cumulative_turn_duration = Duration::from_secs(14 * 60);
    let facts = tideline_footer_from_app(&mut app, 160);
    assert_eq!(
        facts
            .session_clock
            .expect("a finished total earns the chip")
            .0,
        "worked 36m 39s"
    );
}

/// Actively working, waiting on a sub-agent, and waiting on you are three
/// different readings — a bare duration cannot tell them apart.
#[test]
fn clock_distinguishes_working_from_waiting_on_something() {
    let mut app = session_app();
    app.ui_locale = codewhale_localization::Locale::En;
    app.is_loading = true;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(75));
    app.cumulative_turn_duration = Duration::from_secs(2_400);

    let working = tideline_footer_from_app(&mut app, 160)
        .turn_clock
        .expect("working clock");
    assert_eq!(working.0, "working 1m 15s");
    assert_eq!(working.1, ChromeInk::Active);

    // A live sub-agent: the parent turn's clock keeps running, but the word
    // says what it is waiting on (#5906 is the same founder session).
    app.agent_progress
        .insert("agent-a".to_string(), "reading".to_string());
    let subagents = tideline_footer_from_app(&mut app, 160)
        .turn_clock
        .expect("sub-agent clock");
    assert_eq!(subagents.0, "agents underway 1m 15s");
    app.agent_progress.clear();

    // Waiting on the user parks the clock in the waiting ink.
    app.pending_user_input_prompt = Some((
        "tool-call-1".to_string(),
        crate::tools::user_input::UserInputRequest {
            questions: Vec::new(),
        },
    ));
    let waiting = tideline_footer_from_app(&mut app, 160)
        .turn_clock
        .expect("waiting clock");
    assert_eq!(waiting.0, "needs you 1m 15s");
    assert_eq!(waiting.1, ChromeInk::Waiting);
    assert_ne!(waiting.1, working.1, "waiting must not read as working");
}

/// A fresh session says nothing: no turn has run, so there is no clock to
/// state, and a sub-minute total stays quiet rather than rendering a noisy
/// `worked 4s` that immediately ticks.
#[test]
fn a_session_that_has_not_worked_states_no_clock() {
    let mut app = session_app();
    app.ui_locale = codewhale_localization::Locale::En;
    let facts = tideline_footer_from_app(&mut app, 160);
    assert!(facts.turn_clock.is_none());
    assert!(facts.session_clock.is_none());

    app.cumulative_turn_duration = Duration::from_secs(42);
    assert!(
        tideline_footer_from_app(&mut app, 160)
            .session_clock
            .is_none(),
        "under a minute the session total stays quiet"
    );

    // A live turn states its own elapsed from the first second, though — the
    // whole point is telling a stuck turn from a working one.
    app.is_loading = true;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(3));
    let facts = tideline_footer_from_app(&mut app, 160);
    assert_eq!(
        facts.turn_clock.expect("a live turn always times itself").0,
        "working 3s"
    );
    assert!(
        facts.session_clock.is_none(),
        "the sub-minute total is still quiet"
    );
}

/// #6084: when the session half will not paint, the shed ladder must not
/// drop the turn clock at the turn-only rung — that would leave no clock
/// at all. Find a width where both-clocks already sheds the turn half while
/// keeping the session half; with only the turn clock, that width must keep
/// it (shed at the session-clock rung instead).
#[test]
fn absent_session_clock_keeps_turn_past_the_turn_only_rung() {
    let mut first_turn = working();
    first_turn.session_clock = None;
    let both = working();
    let width = (8..=160u16)
        .find(|&w| {
            let text = draw(w, 3, &both.widget(&UI_THEME));
            text.contains("worked 41m 12s") && !text.contains("working 1m 15s")
        })
        .expect("both-clocks fixture must shed turn before session at some width");
    let text = draw(width, 3, &first_turn.widget(&UI_THEME));
    assert!(
        text.contains("working 1m 15s"),
        "{width}: with no session half, the turn clock must survive the turn-only shed rung (#6084): {text}"
    );
}

/// Narrow terminals shed both clock halves before the hint and the counts,
/// the turn half before the session half, and neither reading is ever
/// painted half-truncated.
#[test]
fn narrow_widths_shed_the_clocks_before_the_hint_and_counts() {
    let fixture = working();
    for w in 8..=160u16 {
        let text = draw(w, 3, &fixture.widget(&UI_THEME));
        let turn = text.contains("working 1m 15s");
        let session = text.contains("worked 41m 12s");
        assert!(
            !turn || session,
            "{w} kept the turn clock and dropped the session clock: {text}"
        );
        assert!(
            !session || text.contains("Esc to interrupt"),
            "{w} kept the session clock and dropped the hint: {text}"
        );
        assert!(
            !text.contains("Esc to interrupt") || text.contains("2 agents"),
            "{w} kept the hint and dropped the counts: {text}"
        );
        // Whole readings or none: a clipped duration is a lie.
        assert!(
            !text.contains("working 1m") || turn,
            "{w} painted a half-truncated turn clock: {text}"
        );
        assert!(
            !text.contains("worked 41m") || session,
            "{w} painted a half-truncated session clock: {text}"
        );
    }
}

#[test]
fn scheduled_count_opens_automations_directly() {
    let mut app =
        crate::test_support::test_app_with_options(crate::test_support::test_tui_options("."));
    app.automation_panel.active_automations = 2;
    let facts = super::tideline_footer_from_app(&mut app, 140);
    assert_eq!(
        facts.count_actions.last(),
        Some(&crate::tui::tideline::InteractionAction::OpenAutomations)
    );
}

#[test]
fn posture_shortcuts_recede_without_changing_count_click_targets() {
    use ratatui::{buffer::Buffer, style::Modifier};
    let fixture = working();
    let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
    let targets = render_tideline_footer(buf.area, &mut buf, &fixture.widget(&UI_THEME));
    let text: String = buf.content().iter().map(|cell| cell.symbol()).collect();
    let column = |needle: &str| text[..text.find(needle).unwrap()].chars().count() as u16;
    let label = column("ask");
    let key = column("Shift+Tab to change");
    assert!(buf[(label, 0)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(key, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(
        buf[(key, 0)].fg,
        super::tchrome(&UI_THEME, ChromeInk::MetadataHint)
            .fg
            .unwrap()
    );
    assert_ne!(buf[(label, 0)].fg, buf[(key, 0)].fg);
    assert_eq!(targets.len(), 1);
    let (index, target) = targets[0];
    assert_eq!(index, 0);
    let painted: String = (target.x..target.right())
        .map(|x| buf[(x, 0)].symbol())
        .collect();
    assert_eq!(painted, "2 agents");
}
