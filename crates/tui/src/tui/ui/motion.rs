//! Redraw pacing: animation intervals, live-motion predicates, and the rail's
//! size budgets.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;

/// Select a workbar panel from a keyboard shortcut and say what happened.
/// When the workbar is off the panel change is real but invisible, so the
/// status names that instead of implying something rendered.
pub(crate) fn rail_panel_shortcut(app: &mut App, panel: crate::tui::work_surface::RailPanel) {
    crate::tui::work_surface::select_dock_panel(app, panel);
    app.needs_redraw = true;
    let mut message = format!("Workbar panel: {}", panel.as_setting());
    if app.work_surface.placement == crate::tui::work_surface::WorkSurfacePlacement::Off {
        message.push_str(" (workbar is off — /workbar top to show)");
    }
    app.status_message = Some(message);
}

/// #3033: gate progress-driven repaints to at most one per 100ms.
///
/// Returns whether the current `AgentProgress` event may request a redraw,
/// updating the last-redraw timestamp when it may. Data updates are never
/// throttled — only the repaint request is.
pub(crate) fn agent_progress_redraw_permitted(
    last_redraw: &mut Option<Instant>,
    now: Instant,
) -> bool {
    match *last_redraw {
        Some(last) if now.duration_since(last) < Duration::from_millis(100) => false,
        _ => {
            *last_redraw = Some(now);
            true
        }
    }
}

/// #4095 residual: pace workflow budget-only repaints under fan-out.
///
/// Same 100ms floor as AgentProgress. High-signal workflow lifecycle events
/// bypass this gate and always paint.
pub(crate) fn workflow_budget_redraw_permitted(
    last_redraw: &mut Option<Instant>,
    now: Instant,
) -> bool {
    agent_progress_redraw_permitted(last_redraw, now)
}

pub(crate) fn agent_progress_redraw_permitted_for_drain(
    last_redraw: &mut Option<Instant>,
    seen_agents: &mut HashSet<String>,
    agent_id: &str,
    now: Instant,
) -> bool {
    if !seen_agents.insert(agent_id.to_string()) {
        return false;
    }
    agent_progress_redraw_permitted(last_redraw, now)
}

/// Rows the transcript can spare for the work rail this frame.
///
/// Everything above the transcript is decoration relative to the transcript
/// itself, so the rail is paid out of what is *left over* after the fixed
/// chrome and the transcript's own floor — not out of a fraction of the
/// terminal, which at 24 rows would hand the rail half the screen.
///
/// That floor moves. While the shell is fully idle the transcript is showing
/// the ocean, and the ocean does not draw at all below
/// [`AMBIENT_MIN_CHAT_HEIGHT`](crate::tui::underwater::AMBIENT_MIN_CHAT_HEIGHT)
/// rows — so on a 24-row terminal an always-on panel strip does not shrink
/// the water, it deletes it. Once there is real work on screen the floor
/// drops back to [`MIN_CHAT_HEIGHT`] and the rail gets its rows. Decorative
/// water yields to work; work never yields to decoration.
///
/// `idle_empty` alone is not enough to charge that floor. It is an
/// app-state predicate — it knows the session is quiet, not that the terminal
/// can draw. [`empty_state_mark_visible`](crate::tui::underwater::empty_state_mark_visible)
/// also demands
/// [`AMBIENT_MIN_CHAT_WIDTH`](crate::tui::underwater::AMBIENT_MIN_CHAT_WIDTH)
/// columns, so on a narrow terminal charging the ambient floor would reserve
/// 16 rows for a mark that cannot render at any height and make the strip
/// yield for nothing.
///
/// The row half of that gate is deliberately *not* mirrored here. It would be
/// a step down in terminal *height* — below the floor the rail would take the
/// rows, at the floor it would hand them back — and a strip that vanishes as
/// the terminal grows taller is the resize flicker this budget exists to
/// avoid. The swept axis must stay monotone.
///
/// The column gate is a real trade, not a free one, and an earlier version of
/// this comment wrongly claimed otherwise. Widening past
/// `AMBIENT_MIN_CHAT_WIDTH` on a short-but-tall terminal can swap a strip for
/// the ocean in one column step. That is accepted deliberately: a horizontal
/// resize past 60 columns is a deliberate act with a visible payoff (the
/// water appears), whereas the height version fires while dragging the axis
/// the strip is measured in. Both cannot be monotone at once — charging the
/// floor is what buys the whale its rows, and something has to give.
/// `rail_strip_and_whale_swap_at_the_ambient_width` pins the swap so it stays
/// a decision rather than drifting into an accident.
///
/// The composer is charged at a fixed floor rather than its measured height:
/// the real `composer_height` is itself computed from the strip height, and
/// feeding it back in here would close a loop that oscillates across a
/// resize instead of settling.
pub(crate) fn rail_row_budget(
    app: &App,
    terminal_width: u16,
    terminal_height: u16,
    idle_empty: bool,
) -> u16 {
    // An explicit work-bar choice takes priority over decorative empty-state
    // space, just as real transcript content does. Otherwise Ctrl+] can be
    // accepted while the ambient floor immediately hides its result.
    let ambient_mark_can_draw = idle_empty
        && !app.work_surface.explicit_view
        && terminal_width >= crate::tui::underwater::AMBIENT_MIN_CHAT_WIDTH;
    let chat_floor = if ambient_mark_can_draw {
        crate::tui::underwater::AMBIENT_MIN_CHAT_HEIGHT
    } else {
        MIN_CHAT_HEIGHT
    };
    let composer_floor = crate::tui::composer_chrome::desired_height(
        1,
        0,
        terminal_height,
        app.composer_density,
        crate::tui::widgets::composer_enclosure_enabled(app),
    );
    terminal_height
        .saturating_sub(info_row_height_for(terminal_height))
        // The merged Tideline footer is one row (spec §3: slots 6+8
        // collapsed into it) — the shell no longer brackets the composer
        // with two standing bands.
        .saturating_sub(crate::tui::phase_strip::height())
        .saturating_sub(composer_floor)
        .saturating_sub(chat_floor)
}

/// The info line is one row under the posture row (spec §5b:
/// `Constraint::Length(1)`), at every terminal height. Shared so the rail
/// budget charges the same chrome the layout actually reserves.
pub(crate) fn info_row_height_for(_terminal_height: u16) -> u16 {
    1
}

/// Column-axis twin of [`rail_row_budget`]: the columns a side rail must
/// leave the transcript.
pub(crate) fn rail_min_chat_width(idle_empty: bool) -> u16 {
    if idle_empty {
        crate::tui::underwater::AMBIENT_MIN_CHAT_WIDTH
    } else {
        0
    }
}

pub(crate) fn status_animation_interval_ms(app: &App) -> u64 {
    if app.effective_low_motion_for_status() {
        crate::tui::display_refresh::adaptive_animation_interval_ms(true)
    } else if decision_prompt_waiting(app)
        || !(app.is_loading
            || app.is_compacting
            || app.is_purging
            || crate::tui::subagent_routing::running_agent_count(app) > 0)
    {
        UI_QUIET_STATUS_ANIMATION_MS
    } else {
        // Keep the braille marker on its fixed 5 Hz table for width stability;
        // only atmosphere uses the measured display cadence.
        UI_STATUS_ANIMATION_MS
    }
}

/// Tick interval for the water. `tier` is the draw cadence the frame limiter
/// enforces this frame: while only ambience moves, the tick lands exactly on
/// the atmosphere interval the limiter will draw at, so no wake asks for a
/// frame the limiter then holds. While a turn streams or the user types, the
/// limiter runs at the interactive cap and the authored ocean cadence rides
/// inside it unchanged.
pub(crate) fn underwater_animation_interval_ms(
    app: &App,
    tier: crate::tui::display_refresh::DrawCadenceTier,
) -> u64 {
    use crate::tui::display_refresh::DrawCadenceTier;
    if app.effective_low_motion_for_status() || app.low_motion {
        crate::tui::display_refresh::adaptive_animation_interval_ms(true)
    } else if app.constrained_frame_rate {
        UI_CONSTRAINED_UNDERWATER_ANIMATION_MS
    } else if crate::tui::display_refresh::terminal_is_ghostty() {
        UI_GHOSTTY_UNDERWATER_ANIMATION_MS
    } else {
        // Measured display Hz can raise atmosphere cadence on high-Hz
        // panels; missing probe falls back to the ~8 fps floor.
        let atmosphere = crate::tui::display_refresh::adaptive_animation_interval_ms(false);
        match tier {
            DrawCadenceTier::Atmosphere => atmosphere,
            DrawCadenceTier::Interactive => atmosphere.min(UI_UNDERWATER_ANIMATION_MS),
        }
    }
}

/// Whether any underwater motion owner is actually visible in the transcript
/// host. This keeps the scheduler honest: only the explicit underwater
/// treatment earns a viewport budget, and obscured surfaces never request
/// frames.
#[must_use]
pub(crate) fn underwater_motion_surface_visible(
    area: Option<Rect>,
    underwater_atmosphere_enabled: bool,
    deepsea_field_breathes: bool,
    empty_water_visible: bool,
    obscured: bool,
) -> bool {
    if obscured || !underwater_atmosphere_enabled {
        return false;
    }
    area.is_some_and(|area| {
        area.width > 0
            && area.height > 0
            && (deepsea_field_breathes
                || (area.width >= crate::tui::ocean::AMBIENT_MIN_WIDTH
                    && area.height >= crate::tui::ocean::AMBIENT_MIN_HEIGHT)
                || (empty_water_visible && crate::tui::underwater::empty_state_mark_visible(area)))
    })
}

pub(crate) fn animation_interval_ms(
    app: &App,
    status_motion: bool,
    underwater_motion: bool,
    tier: crate::tui::display_refresh::DrawCadenceTier,
) -> u64 {
    let underwater = underwater_animation_interval_ms(app, tier);
    match (status_motion, underwater_motion) {
        (true, true) => status_animation_interval_ms(app).min(underwater),
        (true, false) => status_animation_interval_ms(app),
        (false, true) => underwater,
        (false, false) => underwater,
    }
}

pub(crate) fn should_tick_status_animation(
    app: &App,
    has_running_agents: bool,
    history_has_live_motion: bool,
    active_cell_has_live_motion: bool,
    translation_placeholder_has_live_motion: bool,
) -> bool {
    !matches!(app.motion_policy().mode(), MotionMode::Still)
        && (app.is_loading
            || has_running_agents
            || app.is_compacting
            || app.is_purging
            || history_has_live_motion
            || active_cell_has_live_motion
            || translation_placeholder_has_live_motion
            || visible_background_task_has_live_motion(app))
}

pub(crate) fn visible_background_task_has_live_motion(app: &App) -> bool {
    app.work_surface.panel == crate::tui::work_surface::RailPanel::Tasks
        && app.work_surface.last_area.is_some()
        && app.task_panel.iter().any(|task| task.status == "running")
}

pub(crate) fn active_cell_has_live_motion(app: &App) -> bool {
    app.active_cell
        .as_ref()
        .is_some_and(|active| active.entries().iter().any(HistoryCell::has_live_motion))
}

pub(crate) fn history_has_live_motion(history: &[HistoryCell]) -> bool {
    history.iter().any(HistoryCell::has_live_motion)
}
