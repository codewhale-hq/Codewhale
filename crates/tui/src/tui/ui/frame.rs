//! Frame composition: the draw entry point, the builders that assemble what a
//! frame needs, and streaming-text accumulation into history cells.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;
use crate::tui::infoline::{InfoLine, InfoSegment, InfoSegmentId, infoline_hitboxes};

/// Context window percentage for the metrics line's reading — the same
/// snapshot the posture bar's ≥80% microcopy reads, so the two can never
/// disagree.
pub(crate) fn info_context_percent(app: &App) -> u8 {
    crate::tui::phase_strip::context_percent_from_app(app)
}

/// Format the session's cumulative usage chip for the metrics line. `/cost`
/// has its own detailed receipt and coverage report; it does not call this
/// formatter. Chips without a cost display produce an empty string.
///
/// Incomplete cost includes its receipt's reason, including an unclassified
/// billing route. A provider switch cannot erase earlier missing coverage.
pub(crate) fn session_cost_label(app: &App) -> String {
    use crate::route_billing::UsageChip;
    let usage_chip = app.cumulative_usage_chip();
    match &usage_chip {
        UsageChip::Money(amount) => Some(amount.clone()),
        UsageChip::PricedSubtotal { .. } | UsageChip::Unknown(_) => {
            crate::route_billing::format_usage_chip(&usage_chip, app.ui_locale)
        }
        _ => None,
    }
    .unwrap_or_default()
}

/// The clock-dependent billing tier of the active route, when the route has
/// one: DeepSeek's V4 Pro/Flash and Flash halve their rates off-peak. `None`
/// for flat-priced routes, for other vendors, and while auto routing has not
/// pinned a concrete model.
pub(crate) fn billing_tier_label(app: &App, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    use crate::config::ProviderKind;
    use codewhale_localization::{MessageId, tr};
    if app.auto_model || !matches!(app.api_provider, ProviderKind::Deepseek) {
        return None;
    }
    let peak = crate::pricing::deepseek_time_tier(&app.model, now)?;
    let id = if peak {
        MessageId::InfoLinePeak
    } else {
        MessageId::InfoLineOffPeak
    };
    Some(tr(app.ui_locale, id).into_owned())
}

/// Output tokens for the metrics line: the live stream's running estimate,
/// else the last turn's provider receipt. Request throughput is independently
/// sourced from SessionMetrics, so a long tool call cannot lower that rate.
fn output_tokens(app: &App) -> Option<u64> {
    if app.is_loading && app.streaming_output_token_estimate > 0 {
        return Some(app.streaming_output_token_estimate);
    }
    app.session
        .last_completion_tokens
        .filter(|tokens| *tokens > 0)
        .map(u64::from)
}

/// Build the metrics line's segments from live `App` state. Shedding is the
/// widget's job; this only states the facts, in display order: model,
/// context, cost, balance, time to first token, output rate, output tokens.
///
/// Repository and branch left this row (2026-09-02): the launch header and
/// the git bottom view own them. Fleet, whale and automation counts left too —
/// the posture bar's live counts own activity.
///
/// Composition is the user's (#5950): every segment here is gated on the
/// matching [`StatusItem`] in `app.status_items`, which is what `/statusline`
/// edits and `tui.status_items` persists. Between 0.9.12 and this change the
/// row ignored that list entirely and the picker's toggles did nothing.
pub(crate) fn info_segments(app: &App, width: u16) -> Vec<InfoSegment> {
    use crate::config::StatusItem;
    use codewhale_localization::MessageId;
    use codewhale_palette::ChromeInk;
    let mut segments = Vec::new();
    let tier = crate::tui::underwater::ShellTier::for_chrome_width(width);
    let shows = |item: StatusItem| app.status_items.contains(&item);

    // Where this session writes (#6112): the workspace leaf and the branch
    // the next commit lands on. Both read cached state only — the branch
    // comes from `app.workspace_context`, refreshed off the render path on
    // the workspace-context TTL, so neither chip costs IO per frame. They
    // lead the row: identity of place before identity of route. The branch
    // chip degrades to absent outside a repository rather than printing a
    // permanent dash.
    if shows(StatusItem::Workspace) {
        let name = crate::tui::workspace_context::status_workspace_name(
            &app.workspace,
            app.workspace_is_linked_worktree,
        );
        segments.push(InfoSegment::new(
            InfoSegmentId::Workspace,
            "",
            crate::tui::workspace_context::truncate_left(
                &name,
                crate::tui::workspace_context::STATUS_CHIP_MAX_WIDTH,
            ),
            ChromeInk::MetadataValue,
        ));
    }
    if shows(StatusItem::GitBranch)
        && let Some(branch) = app
            .workspace_context
            .as_deref()
            .and_then(crate::tui::workspace_context::branch_from_context)
    {
        segments.push(InfoSegment::new(
            InfoSegmentId::GitBranch,
            "",
            crate::tui::workspace_context::truncate_left(
                &if app.workspace_is_linked_worktree {
                    format!("{branch} (wt)")
                } else {
                    branch.to_string()
                },
                crate::tui::workspace_context::STATUS_CHIP_MAX_WIDTH,
            ),
            ChromeInk::MetadataValue,
        ));
    }

    // Route identity — the old identity band's fact, same shed discipline:
    // provider first, then effort, whole names or none. When no model is
    // configured the segment says so and waits.
    // Off the row when the user says so: `/model`, the picker and the launch
    // header all still name the route.
    if shows(StatusItem::Model) {
        let (_, model) = app.effective_route_identity_display();
        // A keyless first run carries a default model id but nothing can
        // answer it: the chip says "not connected", matching the launch
        // card's no-model line (U3), instead of naming a route that fails.
        if model.is_empty() || app.onboarding_needs_api_key {
            segments.push(InfoSegment::new(
                InfoSegmentId::Model,
                app.tr(MessageId::StartupDefaultSubjectModel).as_ref(),
                app.tr(MessageId::InfoLineNotConnected).as_ref(),
                ChromeInk::Waiting,
            ));
        } else {
            // The context reading and the metrics claim the rest of the row;
            // the route sheds its own qualifiers first.
            let budget = crate::tui::phase_strip::info_route_budget(width);
            let fields = info_route_fields(app, tier, budget).unwrap_or_else(|| {
                vec![crate::tui::phase_strip::RouteIdentityField {
                    kind: crate::tui::phase_strip::RouteFieldKind::Model,
                    text: model,
                }]
            });
            segments.push(InfoSegment::new(
                InfoSegmentId::Model,
                "",
                fields
                    .iter()
                    .map(|field| field.text.as_str())
                    .collect::<Vec<_>>()
                    .join(ROUTE_FIELD_JOIN),
                ChromeInk::MetadataValue,
            ));
        }
    }

    // The context reading: painted here and nowhere else, at every
    // fullness. The 0.9.12 row went silent below 50% and left most of a
    // session with no context signal at all (#5950); watching the number
    // climb from 4% is the whole point of the reading. At the 80% cap the
    // whole reading turns to the error token — it is the one fact on this
    // row that becomes a problem rather than a status.
    let pct = info_context_percent(app);
    if shows(StatusItem::ContextPercent) {
        segments.push(InfoSegment::new(
            InfoSegmentId::Context,
            app.tr(MessageId::InfoLineContext).as_ref(),
            format!("{pct}%"),
            // The posture bar one row above calls this exact threshold
            // `ChromeInk::Attention` (`phase_strip::at_context_cap`, also >= 80).
            // One condition, one family: a full context is consequential, not a
            // failure — the next turn still runs and `/compact` is the remedy.
            if pct >= 80 {
                ChromeInk::Attention
            } else {
                ChromeInk::Info
            },
        ));
    }

    // The active goal's live reading: elapsed time plus the model's latest
    // reported progress with its bar. Painted only while a goal is actually
    // active — the percent is the model's own estimate, and the row never
    // invents one for a goal that has not reported.
    if app.goal.status == crate::tools::goal::GoalStatus::Active
        && app.goal.objective.is_some()
        && let Some(started) = app.goal.started_at
    {
        let secs = started.elapsed().as_secs();
        let elapsed = if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m", secs / 60)
        };
        let value = match app.goal.progress.as_ref() {
            Some(progress) => format!(
                "({elapsed}) {}% {}",
                progress.percent,
                crate::tools::goal::goal_progress_bar(progress.percent)
            ),
            None => format!("({elapsed})"),
        };
        segments.push(InfoSegment::new(
            InfoSegmentId::Goal,
            app.tr(MessageId::GoalProgressLabel).as_ref(),
            value,
            ChromeInk::Info,
        ));
    }

    let cost = session_cost_label(app);
    if shows(StatusItem::Cost) && !cost.is_empty() {
        segments.push(InfoSegment::new(
            InfoSegmentId::Cost,
            "",
            cost,
            ChromeInk::MetadataValue,
        ));
    }

    // DeepSeek bills by the clock: the same flag that halves the rates
    // off-peak is painted beside the cost, so the operator can see which tier
    // the next turn buys without opening /cost. Gated on the cost item, whose
    // owner asked for price readings by name.
    if shows(StatusItem::Cost)
        && let Some(tier) = billing_tier_label(app, chrono::Utc::now())
    {
        segments.push(InfoSegment::new(
            InfoSegmentId::BillingTier,
            "",
            tier,
            ChromeInk::MetadataValue,
        ));
    }

    // The prepaid-credit reading: opt-in, and the same status item that
    // authorises the background fetch (`should_fetch_provider_balance`), so
    // the row can only show a number this session actually asked for.
    if shows(StatusItem::Balance)
        && let Some(balance) = app.balance_cell.lock().ok().and_then(|guard| {
            guard
                .as_ref()
                .and_then(crate::pricing::BalanceInfo::chip_label)
        })
    {
        segments.push(InfoSegment::new(
            InfoSegmentId::Balance,
            app.tr(MessageId::FooterBalancePrefix).as_ref(),
            balance,
            ChromeInk::MetadataValue,
        ));
    }

    // The DeepSeek-harness session metrics, from the same accumulators
    // `/cost` prints: nothing here is estimated except the live stream's
    // running token count, which the provider's receipt replaces.
    if (shows(StatusItem::SessionMetrics) || shows(StatusItem::Ttft))
        && let Some(ttft) = app.session_metrics.ttft_average()
    {
        segments.push(InfoSegment::new(
            InfoSegmentId::Ttft,
            app.tr(MessageId::InfoLineTtft).as_ref(),
            crate::tui::session_metrics::format_duration(ttft),
            ChromeInk::MetadataValue,
        ));
    }
    if (shows(StatusItem::SessionMetrics) || shows(StatusItem::OutputRate))
        && let Some(rate) = app.session_metrics.tokens_per_second()
    {
        segments.push(InfoSegment::new(
            InfoSegmentId::Rate,
            "",
            format!(
                "{} {}",
                crate::tui::session_metrics::format_rate(rate),
                app.tr(MessageId::SessionMetricsTokensPerSecond)
            ),
            ChromeInk::MetadataValue,
        ));
    }
    if let Some(tokens) = output_tokens(app) {
        if shows(StatusItem::Cache)
            && let Some(cache_pct) =
                crate::tui::session_metrics::snapshot_from_app(app).cache_hit_percent
        {
            segments.push(InfoSegment::new(
                InfoSegmentId::Cache,
                "cache",
                format!("{cache_pct}%"),
                ChromeInk::MetadataValue,
            ));
        }
        if shows(StatusItem::Tokens) {
            segments.push(InfoSegment::new(
                InfoSegmentId::OutputTokens,
                "↓",
                crate::tui::session_metrics::format_tokens(tokens),
                ChromeInk::MetadataValue,
            ));
        }
    } else {
        if shows(StatusItem::Cache)
            && let Some(cache_pct) =
                crate::tui::session_metrics::snapshot_from_app(app).cache_hit_percent
        {
            segments.push(InfoSegment::new(
                InfoSegmentId::Cache,
                "cache",
                format!("{cache_pct}%"),
                ChromeInk::MetadataValue,
            ));
        }
    }

    segments
}

/// Route fields the info line paints, or `None` when it paints the
/// "not connected" chip instead. `info_segments` and the hitbox split both
/// read this, so a click can never land on a route the row did not draw.
fn info_route_fields(
    app: &App,
    tier: crate::tui::underwater::ShellTier,
    budget: usize,
) -> Option<Vec<crate::tui::phase_strip::RouteIdentityField>> {
    if app.onboarding_needs_api_key {
        return None;
    }
    crate::tui::phase_strip::route_identity_fields(app, tier, budget)
}

/// The info line's controls that actually painted in this frame.
///
/// The route target intentionally contains no copied route metadata. The
/// provider picker retains catalog, readiness, credential, and apply
/// authority; chrome only exposes its entry point.
#[derive(Debug, Clone, Copy, Default)]
struct InfoLineInteractionHitboxes {
    context: Option<Rect>,
    /// The provider name inside the route segment, when the row is wide
    /// enough to render one.
    route: Option<Rect>,
    /// The model name and its effort tier — one span, because they are
    /// always adjacent and `/model` owns both.
    model: Option<Rect>,
}

/// Separator between rendered route fields. Three columns wide, matching
/// `phase_strip::ITEM_SEPARATOR_WIDTH`, which is what the shed budget counts.
const ROUTE_FIELD_JOIN: &str = " · ";

/// Split the route segment's rect back into its fields.
///
/// The segment renders as `provider · model · effort`; a click on the
/// provider belongs to `/provider` and a click on the model or its effort
/// tier belongs to `/model`. Widths come from the same field texts the
/// segment was built from, so the split cannot disagree with what is on
/// screen. Returns `(provider, model-and-effort)`.
fn split_route_hitbox(
    fields: &[crate::tui::phase_strip::RouteIdentityField],
    area: Rect,
) -> (Option<Rect>, Option<Rect>) {
    use crate::tui::phase_strip::RouteFieldKind;
    use unicode_width::UnicodeWidthStr as _;

    let join = ROUTE_FIELD_JOIN.width();
    let right = usize::from(area.right());
    let mut x = usize::from(area.x);
    let mut provider = None;
    let mut model: Option<Rect> = None;
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            x += join;
        }
        let end = (x + field.text.width()).min(right);
        if x >= end {
            break;
        }
        let rect = Rect {
            x: x as u16,
            y: area.y,
            width: (end - x) as u16,
            height: 1,
        };
        match field.kind {
            RouteFieldKind::Provider => provider = Some(rect),
            // Model and effort are adjacent and share a destination, so the
            // span grows rather than replacing — a two-target registration
            // for one idea just gives the pointer a seam to fall into.
            RouteFieldKind::Model | RouteFieldKind::Effort => {
                model = Some(match model {
                    Some(prev) => Rect {
                        x: prev.x,
                        y: prev.y,
                        width: rect.right().saturating_sub(prev.x),
                        height: 1,
                    },
                    None => rect,
                });
            }
        }
        x = end;
    }
    (provider, model)
}

/// Render the info line into its one row and record its segment
/// hitboxes (spec §5b `Constraint::Length(1)`). The ONE header on every
/// screen: the session shell and the launch screen both call this, so the
/// brand lockup, contextual segments, and the pinned meter + clock never
/// change identity between pre- and post-session states. Segment rects are
/// recorded for hover (this frame's highlight resolves against the previous
/// frame's rects, the standard one-frame-lag registry pattern) and for typed
/// click routing.
fn render_info_row(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    identity_only: bool,
) -> InfoLineInteractionHitboxes {
    if area.height == 0 {
        app.viewport.last_infoline_hitboxes.clear();
        return InfoLineInteractionHitboxes::default();
    }
    // The two bottom rows share the composer's one-cell inset. Paint the
    // full band before insetting so hover/click geometry uses the same area.
    Block::default()
        .style(Style::default().bg(app.ui_theme.header_bg))
        .render(area, f.buffer_mut());
    let area = area.inner(ratatui::layout::Margin::new(u16::from(area.width >= 8), 0));
    let mut segments = info_segments(app, area.width);
    if identity_only {
        segments.retain(|segment| {
            matches!(
                segment.id,
                InfoSegmentId::Model | InfoSegmentId::Workspace | InfoSegmentId::GitBranch
            )
        });
    }
    let hovered = app.last_mouse_pos.and_then(|(mx, my)| {
        app.viewport
            .last_infoline_hitboxes
            .iter()
            .find(|hb| {
                matches!(hb.id, InfoSegmentId::Model | InfoSegmentId::Context)
                    && hb.area.x <= mx
                    && mx < hb.area.right()
                    && hb.area.y == my
            })
            .map(|hb| hb.id)
    });
    // The metrics line no longer pins `/help` forever (founder, 2026-09-08).
    // The route still appears until its binding has been used, then retires
    // with the other footer hints so the row stays quiet once help is learned.
    let help_hint = if crate::tui::footer_hints::retired(
        &app.footer_hint_uses,
        crate::tui::footer_hints::HELP_ROUTE,
    ) {
        String::new()
    } else {
        crate::tui::shell_key_routing::info_help_hint(app.ui_locale)
    };
    let info = InfoLine::new(&app.ui_theme, &help_hint, &segments)
        .ascii_safe(crate::tui::color_compat::ascii_safe_enabled())
        .hovered(hovered)
        .compact(app.metrics_line == crate::config::ChromeRowPreset::Compact);
    let hitboxes = infoline_hitboxes(&info, area);
    let route_area = hitboxes
        .iter()
        .find(|hitbox| hitbox.id == InfoSegmentId::Model)
        .map(|hitbox| hitbox.area);
    // Same pure call `info_segments` made, with the same budget owner, so the
    // split lines up with the text that was just measured.
    let route_fields = info_route_fields(
        app,
        crate::tui::underwater::ShellTier::for_chrome_width(area.width),
        crate::tui::phase_strip::info_route_budget(area.width),
    );
    let (provider_area, model_area) = match (route_area, route_fields.as_deref()) {
        (Some(area), Some(fields)) => split_route_hitbox(fields, area),
        // No configured model: the segment says so and is not a route control.
        // No drawn segment: nothing to point at either way.
        (area, None) => (None, area),
        (None, Some(_)) => (None, None),
    };
    let interaction_hitboxes = InfoLineInteractionHitboxes {
        context: crate::tui::infoline::context_meter_hitbox(&info, area),
        route: provider_area,
        model: model_area,
    };
    // Keep the row's quiet background under the widget itself.
    let buf = f.buffer_mut();
    Block::default()
        .style(Style::default().bg(app.ui_theme.header_bg))
        .render(area, buf);
    ratatui::widgets::Widget::render(info, area, buf);
    app.viewport.last_infoline_hitboxes = hitboxes;
    interaction_hitboxes
}

/// Paint the workbar: live runs first (in start order), then settled ones,
/// each with the runtime's count of follow-ups queued on its busy agents.
fn render_workbar(f: &mut Frame, app: &App, area: Rect) {
    let queued_for = |panel: &crate::tui::widgets::workflow_panel::WorkflowPanel| {
        panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .filter(|row| row.status.is_running())
            .filter_map(|row| app.agent_queued_follow_ups.get(&row.task_id))
            .sum::<usize>()
    };
    let (live, settled): (Vec<_>, Vec<_>) = app
        .workflow_runs
        .iter()
        .partition(|panel| panel.lifecycle.is_running());
    let runs: Vec<crate::tui::widgets::workbar::WorkbarRun<'_>> = live
        .into_iter()
        .chain(settled)
        .map(|panel| crate::tui::widgets::workbar::WorkbarRun {
            panel,
            queued: queued_for(panel),
        })
        .collect();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let buf = f.buffer_mut();
    Block::default()
        .style(Style::default().bg(app.ui_theme.footer_bg))
        .render(area, buf);
    crate::tui::widgets::workbar::render(area, buf, &runs, now_ms, &app.ui_theme, app.ui_locale);
}

/// Register the chrome that already answers a click, so it also answers the
/// pointer.
///
/// "What responds to the pointer going over it right now across the entire
/// app" — the honest answer had been: links, truncated text, and the info
/// line. The jump-to-latest button, the plugin call-to-action, and the
/// workflow panel all handled clicks in `mouse_ui` and lit up for nothing,
/// which teaches a person that pointing at things does not work here.
///
/// This runs after the frame body has recorded its rects and before hover is
/// resolved, so it stays one list rather than a `register_rect` scattered
/// through every widget that happens to remember.
fn register_clickable_chrome_for_hover(app: &App) {
    use codewhale_localization::MessageId;
    let targets: [(Option<Rect>, MessageId); 5] = [
        (
            app.viewport.jump_to_latest_button_area,
            MessageId::KbJumpTopBottom,
        ),
        (
            // The pinned prompt header jumps to the user message it names.
            app.viewport.pinned_prompt_area,
            MessageId::PinnedPromptJumpToMessage,
        ),
        (
            app.viewport.last_plugin_cta_review_area,
            MessageId::PluginCtaReview,
        ),
        (
            app.viewport.last_plugin_cta_dismiss_area,
            MessageId::KbCloseMenu,
        ),
        (
            // A workbar row opens `/workflows`.
            app.viewport.last_workbar_area,
            MessageId::CmdWorkflowDescription,
        ),
    ];
    for (area, label) in targets {
        let Some(area) = area else { continue };
        crate::tui::hover_layer::register_rect(
            crate::tui::hover_hit::HoverTargetKind::Link,
            area,
            codewhale_localization::tr(app.ui_locale, label).into_owned(),
            false,
        );
    }

    // The composer's `[↵]` submit control. It registers only when a click
    // there would actually send: an affordance that lights up and then does
    // nothing is the same defect as one that acts without lighting up.
    if let Some(composer) = app.viewport.last_composer_area
        && let Some(submit) = crate::tui::widgets::active_composer_submit_rect(app, composer)
        && app.composer_enter_would_submit()
    {
        crate::tui::hover_layer::register_rect(
            crate::tui::hover_hit::HoverTargetKind::Link,
            submit,
            codewhale_localization::tr(
                app.ui_locale,
                codewhale_localization::MessageId::KbSendDraft,
            )
            .into_owned(),
            false,
        );
    }
}

/// Register the info line's drawn controls as one typed input surface.
///
/// Both the launch stage and a live session use this exact registration, so
/// mouse routing cannot advertise a header segment on only one shell state.
fn register_info_interaction_targets(app: &mut App, hitboxes: InfoLineInteractionHitboxes) {
    if let (Some(hitbox), Some(context_budget)) = (
        hitboxes.context,
        crate::tui::tideline::ContextBudgetSnapshot::from_app(app),
    ) {
        app.viewport
            .interaction_targets
            .register(crate::tui::tideline::InteractionTarget {
                id: crate::tui::tideline::InteractionTargetId::HEADER_CONTEXT,
                area: hitbox,
                focus: crate::tui::tideline::InteractionFocus::Direct,
                keyboard_action: Some(crate::tui::tideline::InteractionAction::InspectContext),
                mouse_action: Some(crate::tui::tideline::InteractionAction::InspectContext),
                inspect_detail: crate::tui::tideline::InspectDetail::ContextBudget(context_budget),
            });
    }
    if let Some(hitbox) = hitboxes.route {
        app.viewport
            .interaction_targets
            .register(crate::tui::tideline::InteractionTarget {
                id: crate::tui::tideline::InteractionTargetId::HEADER_ROUTE,
                area: hitbox,
                focus: crate::tui::tideline::InteractionFocus::Direct,
                keyboard_action: Some(crate::tui::tideline::InteractionAction::OpenProviderPicker),
                mouse_action: Some(crate::tui::tideline::InteractionAction::OpenProviderPicker),
                inspect_detail: crate::tui::tideline::InspectDetail::Route,
            });
    }
    if let Some(hitbox) = hitboxes.model {
        app.viewport
            .interaction_targets
            .register(crate::tui::tideline::InteractionTarget {
                id: crate::tui::tideline::InteractionTargetId::HEADER_MODEL,
                area: hitbox,
                focus: crate::tui::tideline::InteractionFocus::Direct,
                keyboard_action: Some(crate::tui::tideline::InteractionAction::OpenModelPicker),
                mouse_action: Some(crate::tui::tideline::InteractionAction::OpenModelPicker),
                inspect_detail: crate::tui::tideline::InspectDetail::Route,
            });
    }

    for target in app.viewport.interaction_targets.iter() {
        let label = match target.mouse_action {
            Some(crate::tui::tideline::InteractionAction::InspectContext) => format!(
                "{} · {}",
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::CtxMenuContextInspector,
                ),
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::CtxMenuContextInspectorDesc,
                ),
            ),
            Some(crate::tui::tideline::InteractionAction::OpenProviderPicker) => format!(
                "{} · {}",
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::RoutePanelHeader,
                ),
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::CmdProviderDescription,
                ),
            ),
            // `/model` is a command name, not prose, so it stays verbatim in
            // every locale; only the description is translated.
            Some(crate::tui::tideline::InteractionAction::OpenModelPicker) => format!(
                "/model · {}",
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::CmdModelDescription,
                ),
            ),
            Some(crate::tui::tideline::InteractionAction::ShowDockPanel(panel)) => {
                panel.title().to_string()
            }
            Some(crate::tui::tideline::InteractionAction::OpenAutomations) => {
                "/automation".to_string()
            }
            Some(crate::tui::tideline::InteractionAction::DismissDock) => {
                codewhale_localization::tr(
                    app.ui_locale,
                    codewhale_localization::MessageId::KbCloseMenu,
                )
                .into_owned()
            }
            None => continue,
        };
        crate::tui::hover_layer::register_rect(
            crate::tui::hover_hit::HoverTargetKind::Link,
            target.area,
            label,
            false,
        );
    }
}

/// The posture bar's live counts are the bottom-of-screen way into the
/// dock: each one opens the view it counts (agents → AGENTS, shells / tasks
/// → BACKGROUND, automations → their own view, the idle `todo` word → TODO).
/// Dock destinations use the same `ShowDockPanel` action as the strip's tabs.
fn register_footer_count_targets(
    app: &mut App,
    facts: &crate::tui::phase_strip::TidelineFooterFacts,
    count_rects: &[(usize, Rect)],
) {
    for (index, area) in count_rects {
        let Some(action) = facts.count_actions.get(*index).copied() else {
            continue;
        };
        app.viewport
            .interaction_targets
            .register(crate::tui::tideline::InteractionTarget {
                id: crate::tui::tideline::InteractionTargetId::FOOTER_COUNT,
                area: *area,
                focus: crate::tui::tideline::InteractionFocus::Direct,
                keyboard_action: Some(action),
                mouse_action: Some(action),
                inspect_detail: crate::tui::tideline::InspectDetail::Route,
            });
    }
}

/// Map the host terminal rect onto the session shell canvas.
///
/// Wide terminals use the full available width (v0.8.65 behavior; #5322). A
/// brief v0.9 gutter capped usable columns beyond 112 and left dead margins on
/// large displays / tmux panes; that cap is gone. Keep this helper so layout
/// and PTY oracles share one geometry entry point if a future setting wants a
/// configurable measure again.
pub(crate) fn session_shell_area(area: Rect) -> Rect {
    area
}

/// Snapshot the posture a real `Op::SendMessage` would carry, and — when the
/// user supplied a hypothetical prompt — resolve the next turn's route with
/// the **same shared planner** dispatch uses (#1004).
///
/// The hypothetical prompt is taken through the deterministic part of the real
/// submit path, in the real order: the **active skill** it would be wrapped
/// with, file and git mention resolution with the same error propagation, and
/// the paused-command note a real submit appends. That is what makes the body
/// the engine hashes the body a real turn would build. It is never added to
/// the conversation, no state is consumed, and the previewed request itself is
/// never sent.
///
/// Two things a real submit does that an inspection must not, and what happens
/// instead:
///
/// - **`message_submit` hooks.** They run first, before mentions, skill
///   wrapping, route planning, and the tool policy, and they may replace the
///   text or block the turn outright. Running them would give a *preview* the
///   side effects of a submit. So when any are configured, nothing downstream
///   of the text can be claimed exact and the whole manifest reports
///   [`crate::core::engine::preview::PreviewUnresolved::MessageSubmitHooksConfigured`] —
///   including under a
///   fixed model, because the tool policy is derived from the content too.
/// - **Consuming the active skill.** A real submit *takes* `app.active_skill`.
///   The preview clones it: the skill is still pending after an inspection,
///   and the previewed body is the one it would have produced. Dropping it
///   instead — which the first pass did — previewed an unwrapped prompt and
///   quietly under-reported the request by the whole skill instruction.
///
/// Without a prompt there is no next-turn route to resolve under auto model
/// routing and no next-turn body under any routing, so this reports a typed
/// unresolved state instead of recycling the installed route.
pub(crate) async fn build_preview_request_inputs(
    app: &App,
    config: &Config,
    engine_handle: &EngineHandle,
    hypothetical_prompt: Option<String>,
) -> crate::core::engine::preview::PreviewRequestInputs {
    use crate::core::engine::preview::{PreviewNextTurn, PreviewRequestInputs, PreviewUnresolved};

    let requested_model = if app.auto_model {
        "auto".to_string()
    } else {
        app.model.clone()
    };
    let prompt_supplied = hypothetical_prompt.is_some();
    let posture = |next_turn, unresolved| PreviewRequestInputs {
        mode: app.mode,
        allow_shell: app.allow_shell,
        trust_mode: app.trust_mode,
        auto_approve: app_auto_approve_enabled(app),
        approval_mode: app.approval_mode,
        allowed_tools: app.active_allowed_tools.clone(),
        dynamic_tools: Vec::new(),
        provenance: crate::core::ops::UserInputProvenance::ExternalUser,
        requested_model: requested_model.clone(),
        requested_reasoning: app.reasoning_effort.as_setting().to_string(),
        auto_model: app.auto_model,
        hypothetical_prompt_supplied: prompt_supplied,
        next_turn,
        unresolved,
    };

    let Some(prompt) = hypothetical_prompt else {
        // Never clear the unresolved flag just because a session has a route:
        // under auto routing the next prompt is what decides it.
        return posture(
            None,
            if app.auto_model {
                PreviewUnresolved::AutoRouteNeedsPrompt
            } else {
                PreviewUnresolved::NoPrompt
            },
        );
    };

    // Auto routing runs a model classifier. `/preview-request` is an offline
    // inspection command, so it stops before prompt resolution or the shared
    // planner can reach that call. Production remains responsible for Auto.
    if auto_router::should_resolve_auto_model_selection(app) {
        return posture(None, PreviewUnresolved::AutoRouteClassificationNotExecuted);
    }

    if app
        .hooks
        .has_hooks_for_event(crate::hooks::HookEvent::MessageSubmit)
    {
        return posture(None, PreviewUnresolved::MessageSubmitHooksConfigured);
    }

    // Clone, never `take`: an inspection may not consume the pending skill.
    let message = QueuedMessage {
        display: prompt.clone(),
        skill_instruction: app.active_skill.clone(),
        skill_provenance: app.active_skill_provenance.clone(),
        history_echoed: false,
    };
    let mut git_cache = crate::tui::git_mention::GitMentionCache::default();
    // Same failure surface as a real submit: a plugin-skill authority mismatch
    // aborts the turn there and must not be papered over with the raw prompt
    // here — that would describe a request the user could not send.
    let mut content = match queued_message_content_for_app(
        app,
        &message,
        std::env::current_dir().ok(),
        &mut git_cache,
    ) {
        Ok(content) => content,
        Err(error) => {
            return posture(
                None,
                PreviewUnresolved::PromptResolutionFailed(error.to_string()),
            );
        }
    };
    // A real submit appends the paused-command note before planning the route.
    // `plan_paused_command_message` is pure — it decides, it does not resume or
    // discard anything — so the preview can use the same value.
    let paused_dispatch = plan_paused_command_message(app, &prompt);
    if let Some(note) = paused_dispatch.note() {
        content.push_str(note);
    }

    let (app_route_identity, route_config) = match app_scoped_runtime_config(app, config) {
        Ok(route) => route,
        Err(error) => return posture(None, PreviewUnresolved::PlanFailed(error)),
    };
    let planned = plan_turn_route(TurnRoutePlanRequest {
        route_config: &route_config,
        app_route_identity: &app_route_identity,
        api_provider: app.api_provider,
        app_model: &app.model,
        auto_model: app.auto_model,
        reasoning_effort: app.reasoning_effort,
        mode: app.mode,
        content: &content,
        auto_router_context: &auto_router::recent_auto_router_context(&app.api_messages),
        should_auto_resolve: false,
        allow_auto_router_response_cache: false,
        preflight_required: engine_handle.client_preflight_required(),
        auto_compact_user_configured: app.auto_compact_user_configured,
        auto_compact: app.auto_compact,
        auto_compact_threshold_percent: app.auto_compact_threshold_percent,
    })
    .await;

    match planned {
        Ok(planned) => {
            let prompt_context = crate::core::engine::NextTurnPromptContext::for_planned_turn(
                planned.route.identity.provider,
                planned.route.model.clone(),
                crate::route_budget::known_route_limits(planned.route.candidate.limits()),
                app.mode,
                paused_dispatch.goal_objective(app),
                app.goal.status,
                app.goal.token_budget,
                app.translation_enabled,
                app.verbosity.clone(),
            );
            posture(
                Some(Box::new(PreviewNextTurn {
                    content,
                    route: Box::new(planned.route),
                    prompt_context,
                    reasoning_effort: planned.effective_reasoning_effort,
                    reasoning_effort_auto: planned.auto_controls_reasoning,
                    auto_route_source: planned
                        .auto_selection
                        .as_ref()
                        .map(|selection| selection.source.label().to_string()),
                    routing_source: planned.routing_source,
                    compaction: planned.compaction,
                })),
                PreviewUnresolved::NoPrompt,
            )
        }
        Err(error) => posture(None, PreviewUnresolved::PlanFailed(error)),
    }
}

pub(crate) fn build_engine_config(app: &App, config: &Config) -> EngineConfig {
    let identity = app
        .provider_identity
        .as_ref()
        .filter(|identity| config.verify_provider_identity(identity).is_ok());
    let max_subagents = app.max_subagents.clamp(1, crate::config::MAX_SUBAGENTS);
    EngineConfig {
        model: app.model.clone(),
        active_route_limits: app.active_route_limits,
        workspace: app.workspace.clone(),
        // The App owns the session id (claimed before the Runtime store lock
        // and used for every checkpoint/autosave); the engine adopts it so the
        // engine conversation and the persisted session are the same record.
        session_id: app.current_session_id.clone(),
        subagent_state_root: None,
        allow_shell: app.allow_shell,
        trust_mode: app.trust_mode,
        notes_path: config.notes_path(),
        mcp_config_path: config.mcp_config_path(),
        mcp_oauth_callback_port: config.mcp_oauth_callback_port,
        mcp_oauth_callback_url: config.mcp_oauth_callback_url.clone(),
        skills_dir: app.skills_dir.clone(),
        skills_discovery_mode: app.skills_discovery_mode,
        plugin_registry: Some(std::sync::Arc::clone(&app.plugin_registry)),
        instructions: configured_instruction_sources(config),
        project_context_pack_enabled: config.project_context_pack_enabled(),
        translation_enabled: app.translation_enabled,
        verbosity: app.verbosity.clone(),
        // Only an explicit `[tui].max_model_steps` installs a step ceiling.
        max_steps: config.max_model_steps(),
        max_subagents,
        max_admitted_subagents: identity
            .map_or_else(
                || config.max_admitted_subagents(),
                |identity| config.max_admitted_subagents_for_provider(identity),
            )
            .max(max_subagents),
        launch_concurrency: identity
            .map_or_else(
                || config.launch_concurrency(),
                |identity| config.launch_concurrency_for_provider(identity),
            )
            .max(app.mode.mode_delegation_launch_floor()),
        subagents_enabled: identity
            .is_some_and(|identity| config.subagents_enabled_for_provider(identity)),
        features: config.features(),
        auto_review_policy: config.auto_review_policy(),
        compaction: app.compaction_config(),
        todos: app.todos.clone(),
        plan_state: app.plan_state.clone(),
        goal_state: app.last_known_goal_state.as_ref().map_or_else(
            || {
                crate::tools::goal::new_shared_goal_state_from_host_status(
                    app.goal.objective.clone(),
                    app.goal.token_budget,
                    app.goal.status,
                )
            },
            |goal| {
                crate::tools::goal::new_shared_goal_state_from_snapshot(&goal.to_runtime_snapshot())
            },
        ),
        max_spawn_depth: identity.map_or_else(
            || config.subagent_max_spawn_depth(),
            |identity| config.subagent_max_spawn_depth_for_provider(identity),
        ),
        allowed_tools: app.active_allowed_tools.clone(),
        disallowed_tools: None,
        max_tool_calls: None,
        hook_executor: app.runtime_services.hook_executor.clone(),
        network_policy: config.network.clone().map(|toml_cfg| {
            crate::network_policy::NetworkPolicyDecider::with_default_audit(toml_cfg.into_runtime())
        }),
        snapshots_enabled: config.snapshots_config().enabled,
        snapshots_max_workspace_bytes: config
            .snapshots_config()
            .max_workspace_gb
            .saturating_mul(1024 * 1024 * 1024),
        // The TUI records no snapshot receipts; its post-turn snapshot stays
        // off the input path (#234).
        record_restore_points: false,
        lsp_config: config
            .lsp
            .clone()
            .map(crate::config::LspConfigToml::into_runtime),
        runtime_services: app.runtime_services.clone(),
        subagent_model_overrides: config.subagent_model_overrides(),
        fleet_roster: std::sync::Arc::new(crate::fleet::identity::load_effective_roster(
            &config.fleet_config(),
            &app.workspace,
            Some(app.extension_plugin_view().as_ref()),
        )),
        subagent_api_timeout: Duration::from_secs(identity.map_or_else(
            || config.subagent_api_timeout_secs(),
            |identity| config.subagent_api_timeout_secs_for_provider(identity),
        )),
        stream_chunk_timeout: Duration::from_secs(app.stream_chunk_timeout_secs),
        turn_wall_clock: config.turn_wall_clock(),
        stream_max_content_bytes: config.stream_max_content_bytes(),
        stream_max_duration: config.stream_max_duration(),
        stream_retry_limits: config.stream_retry_limits(),
        stream_open_timeout: config.stream_open_timeout(),
        subagent_heartbeat_timeout: Duration::from_secs(identity.map_or_else(
            || config.subagent_heartbeat_timeout_secs(),
            |identity| config.subagent_heartbeat_timeout_secs_for_provider(identity),
        )),
        prefer_bwrap: config.prefers_bwrap(),
        bwrap_extensions: crate::sandbox::BwrapMountExtensions {
            read_only_roots: config.bwrap_ro_roots.clone(),
            device_roots: config.bwrap_dev_roots.clone(),
        },
        read_denylist: config.read_denylist(),
        memory_enabled: config.memory_enabled(),
        memory_path: config.memory_path(),
        speech_output_dir: config.speech_output_dir(),
        vision_config: config.vision_model_config(),
        strict_tool_mode: config.strict_tool_mode.unwrap_or(false),
        goal_objective: app.goal.objective.clone(),
        goal_token_budget: app.goal.token_budget,
        goal_status: app.goal.status,
        goal_max_continuations: config.goal_max_continuations(),
        goal_continuation_delay_seconds: config.goal_continuation_delay_seconds(),
        goal_enforce_token_budget: config.goal_enforce_token_budget(),
        reasoning_only_max_reprompts: config.reasoning_only_max_reprompts(),
        reasoning_only_reprompt_message: Some(config.reasoning_only_reprompt_message().to_string()),
        locale_tag: app.ui_locale.tag().to_string(),
        workshop: {
            crate::tools::large_output_router::WorkshopConfig::install_active(
                config.workshop.as_ref(),
            );
            config.workshop.clone()
        },
        search_provider: config.search_provider(),
        search_api_key: config.search.as_ref().and_then(|s| s.api_key.clone()),
        search_base_url: config.search.as_ref().and_then(|s| s.base_url.clone()),
        search_native: config.search_native(),
        tools_always_load: config.tools_always_load(),
        user_input_limits: config.user_input_limits(),
        user_input_timeout: config.user_input_timeout(),
        goal_max_steps: Some(config.goal_max_steps()),
        tools: config.tools.clone(),
        workspace_follow_symlinks: app.workspace_follow_symlinks,
        exec_policy_engine: config.exec_policy_engine.clone(),
        terminal_chrome_enabled: true,
        advisor_config: config
            .advisor
            .as_ref()
            .map(crate::tools::subagent::AdvisorConfig::from_toml)
            .unwrap_or_else(crate::tools::subagent::AdvisorConfig::disabled),
    }
}

#[cfg(test)]
pub(crate) fn build_app_system_prompt(app: &App, config: &Config) -> SystemPrompt {
    build_app_system_prompt_with_goal(app, config, app.goal.objective.as_deref())
}

pub(crate) fn build_app_system_prompt_with_goal(
    app: &App,
    config: &Config,
    goal_objective: Option<&str>,
) -> SystemPrompt {
    let instructions = configured_instruction_sources(config);
    let user_memory_block = crate::native_memory::native_prompt_block(
        config.memory_enabled(),
        &config.memory_path(),
        &app.workspace,
    );
    // Keep the previewed/rebuilt prompt identical to the engine's: the
    // recovery hint is part of the prefix when a prior workspace session
    // ended mid-turn (#5715).
    let recovery_hint = crate::session_manager::session_recovery_hint(
        &app.workspace,
        app.current_session_id.as_deref(),
    );
    prompts::system_prompt_for_mode_with_context_skills_and_session(
        &app.workspace,
        None,
        Some(&app.skills_dir),
        Some(&instructions),
        prompts::PromptSessionContext {
            user_memory_block: user_memory_block.as_deref(),
            goal_objective,
            project_context_pack_enabled: config.project_context_pack_enabled(),
            locale_tag: app.ui_locale.tag(),
            translation_enabled: app.translation_enabled,
            model_id: &app.model,
            context_window_override: Some(crate::route_budget::route_context_window_tokens(
                app.api_provider,
                &app.model,
                app.active_route_limits,
            )),
            verbosity: app.verbosity.as_deref(),
            recovery_hint: recovery_hint.as_deref(),
            skills_discovery_mode: app.skills_discovery_mode,
            plugin_registry: Some(app.extension_plugin_view().as_ref()),
            mode: app.mode,
        },
    )
}

/// Build the session snapshot every product caller queues into the
/// persistence actor. Journal-only (#6214 T3): the `messages` projection is
/// left empty because the queue drops it anyway, and serialization rehydrates
/// it from the journal — the on-disk bytes are unchanged. Callers must not
/// read `.messages` off the returned snapshot; save or serialize it.
pub(crate) fn build_session_snapshot(
    app: &mut App,
    manager: &SessionManager,
) -> Result<SavedSession, String> {
    let model = app.model_selection_for_persistence();
    let work_state = match app.try_work_state_snapshot() {
        Ok(work_state) => work_state,
        Err(err) => app.last_known_work_state.clone().ok_or_else(|| {
            format!("automatic session snapshot skipped while Work state is busy: {err}")
        })?,
    };
    // Drop what a bounded save already archived (#6842) so the live journal
    // matches the document. No I/O: the ids come from the save path.
    if let Some(session_id) = app.current_session_id.as_deref() {
        let archived = crate::session_manager::take_archived_journal_ids(session_id);
        if let Err(error) = app.session_journal.remove_entries(&archived) {
            tracing::warn!(%error, "kept archived journal entries in memory");
        }
    }
    app.session_journal
        .rebranch_active_messages_stamped(&app.api_messages, &app.api_message_stamps);
    let mut session = crate::session_manager::create_saved_session_journal_only(
        app.current_session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        &app.api_messages,
        app.session_journal.clone(),
        &model,
        &app.workspace,
        u64::from(app.session.total_tokens),
        app.system_prompt.as_ref(),
        Some(app.mode.as_setting()),
    );
    let computed_title = session.metadata.title.clone();
    if let Some(cached) = app
        .current_session_metadata
        .as_ref()
        .filter(|cached| cached.id == session.metadata.id)
    {
        session.metadata.created_at = cached.created_at;
        session
            .metadata
            .parent_session_id
            .clone_from(&cached.parent_session_id);
        session.metadata.forked_from_message_count = cached.forked_from_message_count;
        session.metadata.archived = cached.archived;
        session
            .metadata
            .runtime_store
            .clone_from(&cached.runtime_store);
    }
    // The cache above is a hint; disk is the authority for lifecycle state.
    // Re-reading here is what makes "an archive or rename cannot be reverted
    // by autosave" true regardless of which surface applied it or when
    // (#2934 / #4397). One bounded metadata-prefix read, not a transcript scan.
    let merged = manager.merge_persisted_lifecycle(&mut session.metadata);
    if let Some(binding) = app
        .runtime_services
        .task_manager
        .as_ref()
        .and_then(|tasks| tasks.session_store_binding())
    {
        if session
            .metadata
            .runtime_store
            .as_ref()
            .is_some_and(|saved| {
                saved != &binding && !saved.is_missing_session_store().unwrap_or(false)
            })
        {
            return Err(
                "session snapshot refused to replace its saved Runtime store ownership".into(),
            );
        }
        session.metadata.runtime_store = Some(binding);
    }
    // Title resolution, in priority order:
    // 1. Disk, when the session already exists (#2934/#4397: a rename applied
    //    through the session manager is persisted and must survive autosave).
    // 2. The in-memory cache, when there is no disk record for the session
    //    yet. (The session picker normally persists renames to disk first via
    //    `rename_selected`; this branch covers sessions that have never been
    //    saved, where the cache is the only title source.)
    // 3. The title computed from the conversation (first user message).
    //    The cache is NOT a candidate on its own: it is only refreshed at the
    //    end of this function, so a snapshot taken before any user message
    //    pins it to the `DEFAULT_SESSION_TITLE` placeholder, and restoring it
    //    would prevent every later title update (the bug this block fixes).
    if !merged
        && let Some(cached) = app.current_session_metadata.as_ref()
        && cached.id == session.metadata.id
    {
        session.metadata.title.clone_from(&cached.title);
    }
    if session.metadata.title == crate::session_manager::DEFAULT_SESSION_TITLE
        && computed_title != crate::session_manager::DEFAULT_SESSION_TITLE
    {
        // The placeholder survived from an earlier snapshot; the conversation
        // now has a real first user message, so let the computed title win.
        // Known edge: a session deliberately renamed to the literal
        // placeholder title is treated the same way and yields to the
        // computed title on the next snapshot.
        session.metadata.title = computed_title;
    }
    if let Some(cached) = app.current_session_metadata.as_mut()
        && cached.id == session.metadata.id
    {
        cached.title.clone_from(&session.metadata.title);
        cached.archived = session.metadata.archived;
    }
    session
        .metadata
        .set_model_provider_route(app.api_provider.as_str(), app.provider_id_for_persistence());
    app.sync_cost_to_metadata(&mut session.metadata);
    session.context_references = app.session_context_references.clone();
    session.artifacts = app.session_artifacts.clone();
    session.turn_outcomes = app.session_turn_outcomes.clone();
    session.work_state = work_state;
    session.last_auto_route = app.auto_route_for_persistence();
    session.window_title.clone_from(&app.window_title);
    session.composer_color.clone_from(&app.composer_color);
    app.current_session_metadata = Some(session.metadata.clone());
    // Claim ownership of this session for the process. From here on the
    // Runtime API refuses external renames/archives of it with a typed 409
    // rather than writing something the next snapshot would revert.
    //
    // Claiming here rather than at each of the ten `current_session_id`
    // assignment sites is deliberate: this is the function that establishes
    // "the TUI holds the authoritative copy", which is exactly the condition
    // the conflict protects. A session that has never been snapshotted has no
    // in-memory state to lose, so leaving it unclaimed is correct, not a gap.
    manager.claim_live_session(&session.metadata.id);
    Ok(session)
}

/// The stream sanitizer lives with the other output sanitizers in
/// `codewhale-secrets`; the event loop reaches it through this module.
pub(crate) use codewhale_secrets::sanitize::sanitize_stream_chunk;

/// Ensure an in-flight streaming Assistant cell exists in history and return
/// its index. Thinking cells go through `streaming_thinking::ensure_active_entry`
/// (active cell) instead.
pub(crate) fn ensure_streaming_assistant_history_cell(app: &mut App) -> usize {
    if let Some(index) = app.streaming_message_index {
        return index;
    }
    app.add_message(HistoryCell::Assistant {
        content: String::new(),
        streaming: true,
    });
    let index = app.history.len().saturating_sub(1);
    app.streaming_message_index = Some(index);
    index
}

pub(crate) fn append_streaming_text(app: &mut App, index: usize, text: &str) {
    if text.is_empty() {
        return;
    }
    app.resync_history_revisions();
    let Some(previous_revision) = app.history_revisions.get(index).copied() else {
        return;
    };
    let chained_from_revision = app
        .streaming_source_receipt
        .filter(|receipt| receipt.cell_index == index && receipt.to_revision == previous_revision)
        .map_or(previous_revision, |receipt| receipt.from_revision);
    let mut content_len = None;
    if let Some(HistoryCell::Assistant { content, .. }) = app.history.get_mut(index) {
        content.push_str(text);
        content_len = Some(content.len());
        // Bump only the streaming cell's per-cell revision so the transcript
        // cache re-renders just this cell. Without this, the cache would
        // either skip the update entirely (now that the global
        // history_version is no longer fanned out across every cell) or fall
        // back to a full re-wrap of the entire transcript every chunk.
        app.bump_history_cell(index);
    }
    let Some(content_len) = content_len else {
        return;
    };
    if let Some(to_revision) = app.history_revisions.get(index).copied() {
        app.streaming_source_receipt = Some(crate::tui::transcript::StreamingSourceReceipt {
            cell_index: index,
            from_revision: chained_from_revision,
            to_revision,
            content_len,
        });
    }
}

pub(crate) fn accrue_streaming_token_estimate(app: &mut App, visible_text: &str) {
    if visible_text.is_empty() {
        return;
    }
    app.streaming_output_token_estimate = app
        .streaming_output_token_estimate
        .saturating_add(estimate_output_tokens_from_text(visible_text));
}

pub(crate) fn commit_streaming_display_tick(
    app: &mut App,
    stream_display_clock: &mut StreamDisplayClock,
    now: Instant,
) -> bool {
    if !stream_display_clock.take_due(now) {
        return false;
    }

    // Reveal a bounded slice per beat rather than everything received. The
    // budget is sized from the beat and the backlog, so the displayed pace is a
    // function of the clock instead of the provider's chunking.
    let interval = stream_display_clock.interval();
    let mut updated = false;
    if let Some(index) = app.streaming_message_index {
        let budget =
            crate::tui::streaming::reveal_budget(interval, app.streaming_state.pending_len(0));
        let committed = app.streaming_state.commit_text(0, budget);
        if !committed.is_empty() {
            append_streaming_text(app, index, &committed);
            accrue_streaming_token_estimate(app, &committed);
            updated = true;
        }
    } else if let Some(entry_idx) = app.streaming_thinking_active_entry {
        let budget =
            crate::tui::streaming::reveal_budget(interval, app.streaming_state.pending_len(0));
        let committed = app.streaming_state.commit_text(0, budget);
        if !committed.is_empty() {
            if app.translation_enabled {
                streaming_thinking::set_placeholder(app, entry_idx);
            } else {
                streaming_thinking::append(app, entry_idx, &committed);
            }
            updated = true;
        }
    }

    if app.streaming_state.has_pending_stream_text(0) {
        stream_display_clock.note_delta(now);
    }

    updated
}

/// Build the pending-input preview widget from current `App` state.
///
/// v0.6.6 (#122) wires the live buckets:
/// - `pending_steers` — typed during a running turn + Esc; held until the
///   abort lands and gets resubmitted as a fresh merged turn.
/// - `queued_messages` — Enter while busy; drained at end-of-turn. An
///   unaccepted steer also lands here (#6297) so it is never lost. In Operate,
///   the foreground operator dispatches these as additional background tasks.
pub(crate) fn build_pending_input_preview(app: &App) -> PendingInputPreview {
    let mut preview = PendingInputPreview::new();
    preview.locale = app.ui_locale;
    let selected_attachment = app.selected_composer_attachment_index();
    let mut attachment_index = 0usize;
    preview.context_items = crate::tui::file_mention::pending_context_previews(&app.input)
        .into_iter()
        .map(|item| {
            let selected = if item.removable {
                let selected = selected_attachment == Some(attachment_index);
                attachment_index += 1;
                selected
            } else {
                false
            };
            ContextPreviewItem {
                kind: item.kind,
                label: item.label,
                detail: item.detail,
                included: item.included,
                removable: item.removable,
                selected,
            }
        })
        .collect();
    // #6190: a steer the engine has not recorded yet is exactly what this
    // bucket's "sending into turn" label describes, so it shares it rather
    // than growing a fourth bucket and a fifteenth locale string.
    preview.pending_steers = app
        .pending_steers
        .iter()
        .chain(app.inflight_steers.iter().map(|steer| &steer.message))
        .map(|m| m.display.clone())
        .collect();
    preview.queued_messages = app
        .queued_messages
        .iter()
        .map(|m| m.display.clone())
        .collect();
    preview.pending_approvals = crate::tui::pending_requests::footer_rows(app);
    preview.editing_queued_message = app.queued_draft.as_ref().map(|draft| {
        if app.input.trim().is_empty() {
            draft.display.clone()
        } else {
            app.input.clone()
        }
    });
    preview
}

pub(crate) fn render(f: &mut Frame, app: &mut App, _config: &Config) -> Option<(u16, u16)> {
    app.viewport.ocean_semantic_surfaces.clear();
    let size = f.area();
    // Hover targets belong to the whole composed frame. Resetting inside the
    // transcript erased targets registered later by the composer and modals.
    crate::tui::hover_layer::begin_frame();
    app.pet_watch.prepare_frame();
    let shell_area = session_shell_area(size);
    // Keep the view stack's focus-context texture prototype (#4823) in step
    // with the parsed setting each frame: a plain enum/theme copy, no
    // allocation. `Off` leaves the render byte-identical to before.
    app.view_stack
        .set_focus_texture(app.focus_texture, app.ui_theme);
    app.sidebar_hover = crate::tui::app::SidebarHoverState::default();
    app.viewport.last_prompt_area = None;
    app.viewport.interaction_targets.clear();
    // Keep the OSC-0 whale title truthful to the current shell phase so
    // alt-tabbed sessions communicate state without a second in-app spinner.
    crate::tui::underwater::sync_title_activity(app);

    // Clear entire area with the configured app background.
    let background = Block::default().style(Style::default().bg(app.ui_theme.surface_bg));
    f.render_widget(background, size);

    // Show onboarding screen if needed
    if app.onboarding != OnboardingState::None {
        onboarding::render(f, size, app);
        // Onboarding is a backdrop, not a separate screen manager. Render any
        // native view above every onboarding step so shared pickers and the
        // first-run privacy disclosure cannot become invisible outside the
        // Provider step.
        if !app.view_stack.is_empty() {
            let buf = f.buffer_mut();
            app.view_stack.render(size, buf);
        }
        return None;
    }

    // The opening screen is no longer a separate surface. Founder ruling:
    // "we don't have to have a different look for the opening screen ... we
    // can make it an asset that exists there instead". The launch card is now
    // the idle transcript's own empty state (`underwater::launch_empty_state`),
    // so the composer below it is the real one, the footer and info line are
    // the ones every other screen wears, and Tab means what it means
    // everywhere else — there is no second input authority left to arbitrate.

    // The `[redaction] model_bound` opt-out gate owns the first screen too:
    // it must be answered before any session starts, and it renders above the
    // launch surface.
    if app.redaction_gate {
        crate::tui::redaction_gate::render(f, size, app);
        return None;
    }
    if app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::PetHabitat) {
        crate::tui::pet_watch::render_full(f, app);
        return None;
    }

    let pet_main = crate::tui::pet_watch::main_view(app);

    // Mini-window mode: when the host terminal window is pinned into its
    // small always-on-top form, hide the shell chrome and keep only what the
    // user opted to keep (`[mini_window]` in config.toml, or mutated live by
    // `/config mini_window.keep_*`). The message stream takes the rest.
    let mini = crate::tui::window_control::pinned();
    let mini_cfg = app.mini_window.clone();
    // The info line owns the shell's last row as exactly one row (spec §5b:
    // `Constraint::Length(1)`). It used to be the header; the founder moved
    // it to the bottom (SHELL-DESIGN-20260901 §2.0) so scrolling up reads as
    // intentional. `keep_header` still governs it in mini mode — the row it
    // names moved, not the preference.
    // Evaluate the fully-idle predicate exactly once per frame. It decides
    // how many rows the rail may reserve and whether the idle ocean draws
    // its brand mark (in ChatWidget); calling it twice would let the
    // reservation and the render disagree inside a single frame.
    let idle_empty = crate::tui::widgets::should_render_empty_state(app);
    // `tui.metrics_line = "hidden"` gives the row to the transcript (#5950).
    // The empty shell keeps route identity visible; render_info_row omits
    // session readings until a conversation exists.
    let info_height = if pet_main
        || (mini && !mini_cfg.keep_header)
        || app.metrics_line == crate::config::ChromeRowPreset::Hidden
    {
        0
    } else {
        info_row_height_for(size.height)
    };
    // The merged Tideline footer is the single bottom row (spec §3: slots
    // 6+8 collapsed; §5b `Constraint::Length(1)`): phase·cost·posture on the
    // left, depth·keys on the right. It hides with the rest of the footer
    // chrome in mini mode, never with the composer.
    // `tui.posture_bar = "hidden"` likewise (#5950).
    let footer_height = if pet_main {
        crate::tui::phase_strip::height()
    } else if (mini && !mini_cfg.keep_footer)
        || app.posture_bar == crate::config::ChromeRowPreset::Hidden
    {
        0
    } else {
        crate::tui::phase_strip::height()
    };
    let slash_menu_entries = visible_slash_menu_entries(app, SLASH_MENU_LIMIT);
    let mention_menu_limit = app.mention_menu_limit;
    let mention_menu_entries =
        crate::tui::file_mention::visible_mention_menu_entries(app, mention_menu_limit);
    if !mention_menu_entries.is_empty() && app.mention_menu_selected >= mention_menu_entries.len() {
        app.mention_menu_selected = mention_menu_entries.len().saturating_sub(1);
    }
    let rail_budget = rail_row_budget(app, shell_area.width, shell_area.height, idle_empty);
    let top_work_strip_height = if pet_main {
        crate::tui::work_surface::collapse_strip(app);
        0
    } else if mini && !mini_cfg.keep_todo {
        // Mini mode hides the strip; when the side rail is also hidden (the
        // default), drop the work-surface interaction state so stale
        // hitboxes from the pre-pin layout cannot swallow transcript clicks
        // or trigger phantom strip actions (review M1). A visible rail/strip
        // refreshes that state during its own render.
        if !mini_cfg.keep_sidebar {
            crate::tui::work_surface::collapse_strip(app);
        }
        0
    } else {
        crate::tui::work_surface::height(app, shell_area.width, shell_area.height, rail_budget)
    };

    // Nothing paints above the stage any more, so the body is the whole
    // shell area. The old two-pass split existed only to pin a header to row
    // zero against ratatui's Flex defaults (#1834); with no header there is
    // nothing to pin.
    let body_area = shell_area;

    let body_height = body_area.height;
    let composer_max_height = body_height
        .saturating_sub(
            MIN_CHAT_HEIGHT
                .saturating_add(footer_height)
                .saturating_add(info_height)
                .saturating_add(top_work_strip_height),
        )
        .max(MIN_COMPOSER_HEIGHT);
    let composer_height = if !pet_main && mini && !mini_cfg.keep_input {
        0
    } else {
        let composer_widget = ComposerWidget::new(
            app,
            composer_max_height,
            &slash_menu_entries,
            &mention_menu_entries,
        );
        composer_widget.desired_height(shell_area.width)
    };

    // Pending-input preview (queued / steered messages). Empty when nothing's
    // queued, so zero height when idle. Phase 2 of #85 — solves the
    // "messages typed during a running turn vanish" complaint by giving the
    // user immediate visible feedback above the composer.
    let pending_preview = build_pending_input_preview(app);
    let desired_preview_height = if !pet_main && mini {
        0
    } else {
        pending_preview.desired_height(shell_area.width)
    };

    // The background-work chip (#5286) that used to pin a row above the
    // composer is gone: the posture bar's live counts own "what is in
    // flight" (one owner per fact), and nothing sits between the transcript
    // and the composer that is not a queued draft or an expanded panel.

    // The workbar (#4121): one row per workflow run, directly under the
    // posture bar, so live progress sits beside the controls that act on it
    // and never between the transcript and the composer. Zero rows when no
    // run is showing.
    let desired_workbar_height = if pet_main || mini {
        0
    } else {
        crate::tui::widgets::workbar::desired_rows(app.workflow_runs.len())
    };
    let plugin_cta_height = if !pet_main && mini && !mini_cfg.keep_input {
        0
    } else {
        app.plugin_cta_row_height()
    };
    let auxiliary_budget = body_height.saturating_sub(
        top_work_strip_height
            .saturating_add(MIN_CHAT_HEIGHT)
            .saturating_add(composer_height)
            .saturating_add(footer_height)
            .saturating_add(info_height)
            .saturating_add(plugin_cta_height),
    );
    // Queued-only previews author the direct controls in row two (and fall
    // back to controls-only when just one row remains). Mixed previews retain
    // up to three compact rows at the release floor.
    let preview_cap = if size.height >= 20 { 4 } else { 3 };
    let preview_height = desired_preview_height.min(auxiliary_budget.min(preview_cap));
    let workbar_height =
        desired_workbar_height.min(auxiliary_budget.saturating_sub(preview_height));

    // Two pinned rows bracket the composer from below (SHELL-DESIGN-20260901
    // §2.0 item 3, §2.3b): the posture bar — permission · mode · live counts
    // · the one hint that applies now, with the remote-control state or a
    // live notice pinned right — then the metrics line — model · ctx · cost
    // · ttft · tok/s · ↓ tokens, with the help hint pinned right. Both rows
    // are reserved in every phase, so a turn moving between idle, thinking,
    // tool use, approval, completion, failure, and cancellation rewrites
    // text inside fixed rows — the composer is never displaced.
    // The work surface (roster, to-do) lives BELOW those two rows by default
    // and only when it has content — scrolling up is intentional history —
    // while `top` placement keeps the strip above the transcript. The strip
    // owns a slot at each end and only one has height, so every other slot
    // keeps its index in both placements (the stage and preview are
    // addressed by position below).
    // Bottom never falls back (only side rails do), so the configured
    // placement is the effective one here.
    let strip_below =
        app.work_surface.placement == crate::tui::work_surface::WorkSurfacePlacement::Bottom;
    let (strip_above_height, strip_below_height) = if strip_below {
        (0, top_work_strip_height)
    } else {
        (top_work_strip_height, 0)
    };
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .flex(ratatui::layout::Flex::Start)
        .constraints([
            Constraint::Length(strip_above_height), // Tasks + To-do above transcript (`top`)
            Constraint::Min(1),                     // Chat area
            Constraint::Length(preview_height),     // Pending input preview (0 if empty)
            Constraint::Length(plugin_cta_height),  // Live plugin CTA (0 unless matched)
            Constraint::Length(composer_height),    // Composer
            Constraint::Length(footer_height),      // Posture bar
            Constraint::Length(workbar_height),     // Workbar: one row per workflow run
            Constraint::Length(info_height),        // Metrics line
            Constraint::Length(strip_below_height), // Roster + To-do under the chrome (`bottom`)
        ])
        .split(body_area);
    let strip_slot = if strip_below { 8 } else { 0 };
    let preview_slot = 2;
    let plugin_cta_slot = 3;
    let composer_slot = 4;
    let footer_slot = 5;
    let workbar_slot = 6;
    let info_slot = 7;

    if matches!(
        app.view_stack.top_kind(),
        Some(ModalKind::Approval | ModalKind::UserInput)
    ) {
        app.viewport.last_prompt_area = app.view_stack.top_occupied_region(size);
    }
    // Bottom prompts cover part of the ordinary chat slot. Resolve scrolling
    // against the rows that remain visible, or End leaves the newest content
    // underneath the prompt and PageUp counts rows the user cannot see.
    let mut visible_chat_area = body_chunks[1];
    if let Some(prompt) = app.viewport.last_prompt_area {
        visible_chat_area.height = visible_chat_area
            .height
            .min(prompt.y.saturating_sub(visible_chat_area.y));
    }
    let (work_chat_area, side_work_area) = if pet_main || (mini && !mini_cfg.keep_sidebar) {
        // Mini mode without the side rail: the transcript takes the whole
        // chat row. split_chat is skipped so the rail never reserves columns.
        (visible_chat_area, None)
    } else {
        crate::tui::work_surface::split_chat(
            app,
            visible_chat_area,
            rail_min_chat_width(idle_empty),
        )
    };

    if top_work_strip_height > 0 {
        crate::tui::work_surface::render(f, body_chunks[strip_slot], app);
    } else if let Some(work_area) = side_work_area {
        crate::tui::work_surface::render(f, work_area, app);
    }

    // Render the transcript and optional file-tree sidecar. The underwater
    // default deliberately has no legacy right sidebar: Tasks and To-do own
    // the strip above, Fleet owns `/fleet`, and dense context owns its
    // inspector. Keeping the sidebar here was the architectural reason the
    // rejected build still read as the old TUI under a gradient.
    let shell_ocean;
    {
        // Defensive backstop (#400): fill the entire body area with ink
        // background before any sub-widgets render, so cells that end up
        // uncovered by layout splits (e.g. after file-tree toggle or
        // resize) don't retain stale content from a previous frame.
        Block::default()
            .style(Style::default().bg(app.ui_theme.surface_bg))
            .render(work_chat_area, f.buffer_mut());

        // When the file-tree pane is visible and the terminal is wide
        // enough, reserve the left ~25% for the file tree.
        let chat_area =
            if app.file_tree.is_some() && work_chat_area.width >= FILE_TREE_MIN_HOST_WIDTH {
                app.file_tree_visible = true;
                let split = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
                    .split(work_chat_area);
                let tree_area = split[0];
                let remaining = split[1];

                // Render the file-tree pane.
                if let Some(ref mut state) = app.file_tree {
                    crate::tui::file_tree::render_file_tree(f, tree_area, state, app.ui_theme.mode);
                }

                remaining
            } else {
                app.file_tree_visible = false;
                work_chat_area
            };
        app.sidebar_hover_tooltip = None;

        if pet_main {
            // Hidden shell surfaces must relinquish their pointer targets.
            // The common composer below still publishes its exact native plan.
            app.viewport.last_transcript_area = None;
            app.viewport.pinned_prompt_area = None;
            app.viewport.pinned_prompt_message = None;
            app.launch.row_hitboxes.clear();
            crate::tui::pet_watch::render_main(f, chat_area, app);
            shell_ocean = None;
        } else if app.agent_focus.is_some() && !app.launch.return_to_session {
            // A focused worker's full transcript owns the conversation area;
            // the ocean column and every other shell surface stay as they are.
            //
            // The widget below is built only to sample the ocean column, but
            // its constructor also consumes `pending_scroll_delta` into the
            // (invisible) main-transcript scroll state — which would starve
            // the focused transcript of every PageUp/PageDown and wheel
            // event. Park the delta across the sample so `render_focus`
            // receives it and the focused pane scrolls exactly like the main
            // transcript.
            let parked_scroll_delta = app.viewport.pending_scroll_delta;
            app.viewport.pending_scroll_delta = 0;
            {
                let chat_widget = ChatWidget::new(app, chat_area).with_ocean_viewport(size);
                shell_ocean = chat_widget.ocean_column();
            }
            app.viewport.pending_scroll_delta = parked_scroll_delta;
            // The constructor above sampled an invisible main transcript;
            // only the actual focused painter may publish semantic regions.
            app.viewport.ocean_semantic_surfaces.clear();
            // The sampling constructor above records the pinned prompt header's
            // hit box from the main session's transcript, but the focus pane
            // never paints that header — its first row is the agent banner.
            // Drop the stale box so the banner cannot answer a click meant for
            // the (hidden) main transcript.
            app.viewport.pinned_prompt_area = None;
            app.viewport.pinned_prompt_message = None;
            crate::tui::agent_focus::refresh_focus(app);
            let buf = f.buffer_mut();
            crate::tui::agent_focus::render_focus(app, chat_area, buf);
        } else {
            if app.launch.visible
                && !app.launch.return_to_session
                && app.onboarding == crate::tui::app::OnboardingState::None
            {
                app.launch
                    .mark_reveal_started_at
                    .get_or_insert_with(std::time::Instant::now);
            }
            let chat_widget = ChatWidget::new(app, chat_area).with_ocean_viewport(size);
            shell_ocean = chat_widget.ocean_column();
            let buf = f.buffer_mut();
            chat_widget.render(chat_area, buf);
        }
        // The launch card's rows are clickable where they painted. The row
        // offsets come from the same builder that produced the lines, so a
        // hitbox cannot describe a row the transcript did not draw — and a
        // fully dissolved card painted nothing this frame, so it owns no
        // rows either.
        if !pet_main
            && app.launch.card_paintable(
                app.ambient_clock_ms,
                app.motion_policy().allows_decorative(),
            )
        {
            crate::tui::underwater::refresh_launch_row_hitboxes(app, chat_area);
        } else if !app.launch.row_hitboxes.is_empty() {
            app.launch.row_hitboxes.clear();
        }
    }

    // Render pending-input preview (queued/steered messages, if any).
    if preview_height > 0 {
        let buf = f.buffer_mut();
        pending_preview.render(body_chunks[preview_slot], buf);
    }

    if plugin_cta_height > 0 {
        let buf = f.buffer_mut();
        crate::tui::plugin_suggestions::draw_plugin_cta(app, body_chunks[plugin_cta_slot], buf);
    } else {
        app.viewport.last_plugin_cta_area = None;
        app.viewport.last_plugin_cta_review_area = None;
        app.viewport.last_plugin_cta_dismiss_area = None;
    }

    // Render once and retain that exact plan for caret and pointer projection.
    let composer_plan = {
        let composer_widget = ComposerWidget::new(
            app,
            composer_max_height,
            &slash_menu_entries,
            &mention_menu_entries,
        );
        composer_widget.render_plan(body_chunks[composer_slot], f.buffer_mut())
    };
    let cursor_pos = composer_plan.cursor.map(|pos| (pos.x, pos.y));
    app.viewport.last_composer_area = Some(body_chunks[composer_slot]);
    app.viewport.last_composer_content = Some(composer_plan.geometry.inner);
    app.viewport.last_composer_scroll_offset = composer_plan.scroll_offset;
    app.viewport.last_composer_top_padding = composer_plan.top_padding;
    // The posture bar is the first row under the composer: permission chip
    // (never sheds), mode, live counts, the one hint that applies now, with
    // the remote-control state or a live notice pinned right.
    if footer_height > 0 {
        let area = body_chunks[footer_slot];
        let mut facts = crate::tui::phase_strip::tideline_footer_from_app(app, area.width);
        if pet_main {
            // The pet header owns phase and notices. This row retains the
            // existing permission/mode controls, live counts and cap warning.
            facts.turn_clock = None;
            facts.session_clock = None;
            facts.right = None;
            if !app.is_loading {
                facts.hint = None;
            }
        }
        let footer = facts
            .widget(
                &app.ui_theme,
                crate::tui::color_compat::ascii_safe_enabled(),
            )
            .compact(app.posture_bar == crate::config::ChromeRowPreset::Compact);
        let buf = f.buffer_mut();
        Block::default()
            .style(Style::default().bg(app.ui_theme.footer_bg))
            .render(area, buf);
        let count_rects = crate::tui::phase_strip::render_tideline_footer(area, buf, &footer);
        register_footer_count_targets(app, &facts, &count_rects);
    }

    if workbar_height > 0 {
        let area = body_chunks[workbar_slot];
        render_workbar(f, app, area);
        app.viewport.last_workbar_area = Some(area);
    } else {
        app.viewport.last_workbar_area = None;
    }

    // The metrics line sits directly under the posture bar: model · ctx ·
    // cost · ttft · tok/s · ↓ tokens, with the help hint pinned right.
    let mut info_interactions = InfoLineInteractionHitboxes::default();
    if info_height > 0 {
        info_interactions = render_info_row(f, app, body_chunks[info_slot], idle_empty);
    } else {
        app.viewport.last_infoline_hitboxes.clear();
    }
    register_info_interaction_targets(app, info_interactions);

    // The native plan keeps its selected source rows private. If a custom
    // theme aliases selection and base grounds, preserve the whole mounted
    // composer while selecting rather than infer selected cells from RGB.
    if app.selection_range().is_some() && app.ui_theme.selection_bg == app.ui_theme.composer_bg {
        app.viewport
            .ocean_semantic_surfaces
            .push(body_chunks[composer_slot]);
    }

    // The underwater shell is one water column, not a stack of independently
    // shaded panels. Continue the transcript's absolute-row ramp through each
    // ordinary shell surface after its foreground has rendered. Semantic
    // backgrounds remain exact through different-ground guards and the
    // frame-derived explicit styled-surface mask, including aliased colors.
    if let Some(column) = shell_ocean {
        // The working canvas may keep a small responsive gutter, but the water
        // does not stop at that content edge. One full-frame `surface_bg` pass
        // paints the cleared terminal floor and every ordinary surface band
        // so wide layouts read as one ocean rather than a blue card floating
        // between black banks. `paint_matching` leaves every semantic widget
        // background untouched.
        column.paint_matching_native(
            size,
            f.buffer_mut(),
            app.ui_theme.surface_bg,
            &app.ui_theme,
            &app.viewport.ocean_semantic_surfaces,
        );
        if plugin_cta_height > 0 {
            column.paint_matching_native(
                body_chunks[plugin_cta_slot],
                f.buffer_mut(),
                app.ui_theme.composer_bg,
                &app.ui_theme,
                &app.viewport.ocean_semantic_surfaces,
            );
        }
        column.paint_matching_native(
            body_chunks[composer_slot],
            f.buffer_mut(),
            app.ui_theme.composer_bg,
            &app.ui_theme,
            &app.viewport.ocean_semantic_surfaces,
        );
        if footer_height > 0 {
            column.paint_matching_native(
                body_chunks[footer_slot],
                f.buffer_mut(),
                app.ui_theme.footer_bg,
                &app.ui_theme,
                &app.viewport.ocean_semantic_surfaces,
            );
        }
        if workbar_height > 0 {
            column.paint_matching_native(
                body_chunks[workbar_slot],
                f.buffer_mut(),
                app.ui_theme.footer_bg,
                &app.ui_theme,
                &app.viewport.ocean_semantic_surfaces,
            );
        }
    }
    register_clickable_chrome_for_hover(app);
    crate::tui::hover_layer::apply_resolved_effects(
        f.buffer_mut(),
        app.effective_low_motion_for_status(),
        &app.ui_theme,
    );
    if !app.view_stack.is_empty() {
        // The live transcript overlay snapshots the app's history + active
        // cell on each render so streaming mutations propagate. Other views
        // are static and skip this refresh.
        if app.view_stack.top_kind() == Some(ModalKind::LiveTranscript) {
            refresh_live_transcript_overlay(app);
        } else if app.view_stack.top_kind() == Some(ModalKind::ContextInspector) {
            refresh_context_inspector_overlay(app);
        }
        let buf = f.buffer_mut();
        app.view_stack.render(size, buf);
        // Any view on the stack owns the keyboard and paints over the
        // composer, and no view draws its own text caret, so the composer's
        // caret must not surface through the modal (#6545).
        return None;
    }

    cursor_pos
}

/// Hide the real terminal caret before ratatui applies a frame diff.
///
/// A diff moves the terminal cursor through every changed run. Electron/xterm
/// IME bridges (notably Tabby on Windows, #5023) can observe those transient
/// positions even though the final frame is correct, which makes the native
/// candidate window jump around the screen. Keep the caret hidden for the
/// whole diff and pair this with [`finish_frame_cursor`] after the draw.
pub(super) fn prepare_frame_cursor<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
) -> std::result::Result<(), B::Error> {
    terminal.hide_cursor()
}

/// Restore the composer caret in IME-safe order: position first, reveal last.
///
/// Ratatui's `Frame::set_cursor_position` path currently calls `show_cursor`
/// before `set_cursor_position`. That briefly exposes the stale or last-diff
/// position to the terminal's IME bridge. Owning the final two operations here
/// preserves ratatui's internal cursor tracking while ensuring there is only
/// one visible caret position per completed frame (#5023).
pub(super) fn finish_frame_cursor<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    cursor_pos: Option<(u16, u16)>,
) -> std::result::Result<(), B::Error> {
    if let Some(cursor_pos) = cursor_pos {
        terminal.set_cursor_position(cursor_pos)?;
        terminal.show_cursor()?;
    }
    Ok(())
}

/// Draw a complete application frame, optionally with a full viewport reset.
///
/// When `full_repaint` is true, the terminal scroll margins and origin mode
/// are reset, the screen is cleared, ratatui's buffer is emptied, and then
/// the full UI is drawn — all within a single DEC 2026 synchronized-update
/// batch so GPU-accelerated terminals (Ghostty, VS Code, Kitty) render one
/// complete frame instead of a blank intermediate frame followed by the UI.
///
/// When `full_repaint` is false, only the diff from the previous draw is
/// written (normal incremental update path).
pub(crate) fn draw_app_frame_inner(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &Config,
    full_repaint: bool,
) -> Result<()> {
    terminal.backend_mut().set_palette_mode(app.ui_theme.mode);
    terminal.backend_mut().set_theme(app.theme_id, app.ui_theme);
    app.viewport.ocean_caps = Some(terminal.backend().native_ocean_caps());
    // DEC 2026 wrapping is on by default but can be turned off for
    // terminals that mishandle it (Ptyxis 50.x + VTE 0.84.x flashes the
    // whole viewport on every wrapped frame instead of deferring as the
    // standard requires). Settings::synchronized_output_enabled resolves
    // the user's setting against the Ptyxis env auto-detect.
    let resized = app.viewport.pending_terminal_size.take();
    let result = synchronized_frame(terminal, app.synchronized_output_enabled, |terminal| {
        if let Some(size) = resized {
            // Keep ratatui's resize clear in the same DEC 2026 transaction as
            // the repaint. Inline mode rebuilds its fixed-height viewport.
            let refit = if app.screen_mode == ScreenMode::Inline {
                refit_inline_viewport(terminal, size)
            } else {
                terminal.resize(Rect::new(0, 0, size.width, size.height))
            };
            if let Err(err) = refit {
                tracing::warn!(?err, "terminal resize failed; falling back to clear+draw");
            }
            // ConHost and Terminal.app can briefly report the previous size.
            terminal.backend_mut().force_size(size);
            terminal.backend_mut().set_terminal_size(size);
        }
        // The terminal cursor itself is also input-method geometry. Hide it
        // before clear/diff operations move it, then restore the one composer
        // position after ratatui finishes drawing (#5023).
        prepare_frame_cursor(terminal)?;
        if full_repaint || resized.is_some() {
            terminal.backend_mut().write_all(TERMINAL_ORIGIN_RESET)?;
            terminal.clear()?;
        }
        let mut cursor_pos = None;
        terminal.draw(|f| cursor_pos = render(f, app, config))?;
        app.pet_watch.present(terminal.backend_mut())?;
        finish_frame_cursor(terminal, cursor_pos)?;
        Ok(())
    });
    if resized.is_some() {
        terminal.backend_mut().clear_forced_size();
    }
    result
}

/// End and flush the synchronized frame even when resizing or drawing fails.
pub(super) fn synchronized_frame<B, T>(
    terminal: &mut Terminal<B>,
    enabled: bool,
    draw: impl FnOnce(&mut Terminal<B>) -> Result<T>,
) -> Result<T>
where
    B: ratatui::backend::Backend + Write,
{
    if enabled {
        let _ = terminal.backend_mut().write_all(BEGIN_SYNC_UPDATE);
    }
    let result = draw(terminal);
    if enabled {
        let _ = terminal.backend_mut().write_all(END_SYNC_UPDATE);
    }
    let _ = std::io::Write::flush(terminal.backend_mut());
    result
}

/// Count how many `HistoryCell::User` entries currently live in the
/// transcript. Used by the backtrack state machine to decide whether
/// there's anything to rewind to. Walks `app.history` directly so it
/// stays accurate even mid-stream (the streaming Assistant cell never
/// counts as a user turn).
pub(crate) fn count_user_history_cells(app: &App) -> usize {
    app.history
        .iter()
        .filter(|cell| matches!(cell, HistoryCell::User { .. }))
        .count()
}

/// Find the absolute index of the Nth-from-tail `HistoryCell::User` in
/// `app.history`. `depth` of 0 selects the most recent user cell.
/// Returns `None` if `depth` is out of range.
pub(crate) fn find_user_cell_index_from_tail(app: &App, depth: usize) -> Option<usize> {
    let mut count = 0usize;
    for (idx, cell) in app.history.iter().enumerate().rev() {
        if matches!(cell, HistoryCell::User { .. }) {
            if count == depth {
                return Some(idx);
            }
            count += 1;
        }
    }
    None
}

/// Truncate `text` to at most `max_chars` characters, cutting at the last
/// natural phrase boundary (`.`, `,`, `:`, `;`, `—`, `-`, or whitespace)
/// so words are never split. Appends `…` only when text was actually cut.
pub(crate) fn short_title_truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    // Find the boundary as a character index. `str::rfind` returns a byte
    // offset, which mis-counts multi-byte UTF-8 text when fed back into
    // `chars().take()`, so operate on `Vec<char>` instead.
    let candidate: Vec<char> = text.chars().take(max_chars).collect();
    let boundary = candidate
        .iter()
        .rposition(|&c| matches!(c, '.' | ',' | ':' | ';' | '—' | '-'))
        .or_else(|| candidate.iter().rposition(|&c| c == ' '))
        .unwrap_or(max_chars.min(candidate.len()).saturating_sub(1));
    let cut: String = text.chars().take(boundary.max(1)).collect();
    format!("{cut}…")
}

pub(crate) fn compact_user_context_display(content: &str) -> String {
    content
        .split("\n\n---\n\nLocal context from @mentions:")
        .next()
        .unwrap_or(content)
        .to_string()
}

#[cfg(test)]
pub(crate) fn transcript_scroll_percent(top: usize, visible: usize, total: usize) -> Option<u16> {
    if total <= visible {
        return None;
    }

    let max_top = total.saturating_sub(visible);
    if max_top == 0 {
        return None;
    }

    let clamped_top = top.min(max_top);
    let percent = ((clamped_top as f64 / max_top as f64) * 100.0).round() as u16;
    Some(percent.min(100))
}

pub(crate) fn estimated_context_tokens(app: &App) -> Option<i64> {
    // ONE estimator: this is `compaction::estimate_input_tokens_for_pressure`
    // over the same message list (per-message cache, framing included) —
    // deliberately not the 1.5x conservative variant. The meter, the >=80%
    // depth warning, and the auto-compact gate must agree about where the
    // threshold is: the inflated estimate used to show "ctx 82%" while the
    // gate read ~55% and correctly refused to compact (#6297). The 1.5x
    // inflation stays where it belongs — request-overflow protection
    // (`estimate_input_tokens_conservative`).
    let message_count = app.api_messages.len();
    let mut cache = app.context_token_cache.borrow_mut();
    if cache.message_tokens.len() > message_count {
        cache.message_tokens.truncate(message_count);
    }
    while cache.message_tokens.len() < message_count {
        let index = cache.message_tokens.len();
        cache
            .message_tokens
            .push(estimate_tokens(&app.api_messages[index..=index]));
    }
    // The final assistant/tool message may grow while streaming. Recompute
    // only that tail entry; historical messages remain O(1) on steady frames.
    if message_count > 0 {
        let last = message_count - 1;
        cache.message_tokens[last] = estimate_tokens(&app.api_messages[last..=last]);
    }
    let message_tokens = cache.message_tokens.iter().copied().sum::<usize>();
    let system_tokens =
        estimate_input_tokens_conservative(&[], app.system_prompt.as_ref()).saturating_sub(48);
    let estimated = message_tokens
        .saturating_add(system_tokens)
        .saturating_add(message_count.saturating_mul(12))
        .saturating_add(48);
    i64::try_from(estimated).ok()
}

pub(crate) fn context_usage_snapshot(app: &App) -> Option<(i64, u32, f64)> {
    let max = crate::route_budget::route_context_window_tokens(
        app.api_provider,
        app.effective_model_for_budget(),
        app.active_route_limits,
    );
    context_usage_snapshot_for_window(app, max)
}

pub(crate) fn context_usage_snapshot_for_window(app: &App, max: u32) -> Option<(i64, u32, f64)> {
    // Before a conversation starts, the assembled startup prompt alone is not
    // conversation usage, and compacting an empty session cannot reclaim it.
    // A submitted first turn has started the conversation even before the
    // engine mirrors its messages back, and so has any provider usage; those
    // keep the real pressure reading.
    let conversation_started = !app.api_messages.is_empty()
        || app.is_loading
        || app
            .history
            .iter()
            .any(|cell| matches!(cell, HistoryCell::User { .. }));
    if !conversation_started
        && app.session.last_prompt_tokens.unwrap_or(0) == 0
        && app.last_billed_input_tokens.unwrap_or(0) == 0
    {
        return Some((0, max, 0.0));
    }
    let max_i64 = i64::from(max);
    let reported = app
        .session
        .last_prompt_tokens
        .map(i64::from)
        .map(|tokens| tokens.max(0));
    // Lift to the provider-billed prompt exactly as the auto-compaction gate,
    // the context inspector and the `/context` headline do (#5577): a provider
    // billing above the local estimate must not leave the footer under-showing
    // the pressure those surfaces report.
    let billed = app.last_billed_input_tokens.map_or(0, i64::from);
    let estimated = estimated_context_tokens(app).map(|tokens| tokens.max(0).max(billed));

    // Always prefer the estimated current-context size (computed from
    // `app.api_messages`) when we have it. Reported `last_prompt_tokens`
    // comes from `Event::TurnComplete.usage`, which the engine builds with
    // `turn.add_usage` — that SUMS input_tokens across every round in the
    // turn, so a multi-round tool-call turn reports a value much larger
    // than the actual context window state, then the next single-round
    // turn drops back to a single round's input_tokens. User-visible %
    // was bouncing 31% → 9% (#115) because of this. The estimate is
    // monotonic wrt conversation growth, which is what a "context filling
    // up" indicator should show. We still consult `reported` only as a
    // fallback when no estimate is available (e.g., immediately after a
    // session restore before the api_messages are populated).
    let used = match (estimated, reported) {
        // No messages yet (a restore before the projection lands): the
        // estimate is only the system prompt, so the reported prompt is the
        // better reading and must not be dropped to ~0%.
        (Some(estimated), Some(reported)) if app.api_messages.is_empty() => {
            estimated.max(reported).min(max_i64)
        }
        (Some(estimated), _) => estimated.min(max_i64),
        (None, Some(reported)) => reported.min(max_i64),
        (None, None) => return None,
    };

    let max_f64 = f64::from(max);
    let used_f64 = used as f64;
    let percent = ((used_f64 / max_f64) * 100.0).clamp(0.0, 100.0);
    Some((used, max, percent))
}

/// True while a `workflow` tool is executing in the foreground (active cell)
/// or still shown as running in history. Used to keep per-subagent completion
/// notifications quiet during a workflow run under `final-only`.
pub(crate) fn workflow_tool_is_running(app: &App) -> bool {
    fn is_running_workflow(cell: &HistoryCell) -> bool {
        matches!(
            cell,
            HistoryCell::Tool(ToolCell::Generic(tool))
                if tool.name == "workflow" && tool.status == ToolStatus::Running
        )
    }
    app.history.iter().any(is_running_workflow)
        || app
            .active_cell
            .as_ref()
            .is_some_and(|active| active.entries().iter().any(is_running_workflow))
}

#[cfg(test)]
mod tests {
    use super::{register_info_interaction_targets, render_info_row, short_title_truncate};
    use ratatui::{Terminal, backend::TestBackend};

    /// Chrome that answers a click must also answer the pointer, or the app
    /// teaches people that pointing at things does not work here.
    #[test]
    fn clickable_chrome_registers_a_hover_target() {
        let _guard = crate::tui::hover_layer::HOVER_TEST_LOCK.lock().unwrap();
        crate::tui::hover_layer::begin_frame();
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        let button = ratatui::layout::Rect::new(70, 10, 3, 3);
        app.viewport.jump_to_latest_button_area = Some(button);

        super::register_clickable_chrome_for_hover(&app);

        let registered = crate::tui::hover_layer::registered_targets();
        assert!(
            registered.iter().any(|hit| hit.area == button),
            "the jump-to-latest button handles a click in mouse_ui and must \
             light up under the pointer; registered: {registered:?}"
        );
    }

    /// The pinned prompt header answers a click in `mouse_ui`; it must light
    /// up under the pointer like every other clickable chrome.
    #[test]
    fn pinned_prompt_header_registers_a_hover_target() {
        let _guard = crate::tui::hover_layer::HOVER_TEST_LOCK.lock().unwrap();
        crate::tui::hover_layer::begin_frame();
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        let header = ratatui::layout::Rect::new(4, 3, 40, 1);
        app.viewport.pinned_prompt_area = Some(header);

        super::register_clickable_chrome_for_hover(&app);

        let registered = crate::tui::hover_layer::registered_targets();
        assert!(
            registered.iter().any(|hit| hit.area == header),
            "the pinned prompt header handles a click in mouse_ui and must \
             light up under the pointer; registered: {registered:?}"
        );
    }

    /// The composer's `[↵]` answered clicks and showed nothing under the
    /// pointer — the last of the clickable-but-dark controls. It lights up
    /// only when a click there would actually send.
    #[test]
    fn composer_send_target_lights_up_only_when_it_would_send() {
        let _guard = crate::tui::hover_layer::HOVER_TEST_LOCK.lock().unwrap();
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.launch.visible = false;
        app.composer_border = true;
        let area = ratatui::layout::Rect::new(0, 20, 80, 4);
        app.viewport.last_composer_area = Some(area);
        app.viewport.last_composer_content = Some(ratatui::layout::Rect::new(1, 21, 73, 2));
        let submit = crate::tui::widgets::active_composer_submit_rect(&app, area)
            .expect("enclosed composer submit");

        // Empty draft: the click path refuses, so the pointer must not promise.
        app.input.clear();
        app.cursor_position = 0;
        crate::tui::hover_layer::begin_frame();
        super::register_clickable_chrome_for_hover(&app);
        assert!(
            !crate::tui::hover_layer::registered_targets()
                .iter()
                .any(|hit| hit.area == submit),
            "an inert send target must not advertise itself"
        );

        app.input = "ship it".to_string();
        app.cursor_position = app.input.chars().count();
        crate::tui::hover_layer::begin_frame();
        super::register_clickable_chrome_for_hover(&app);
        assert!(
            crate::tui::hover_layer::registered_targets()
                .iter()
                .any(|hit| hit.area == submit),
            "a live send target must light up under the pointer"
        );
    }

    #[test]
    fn infoline_route_segment_registers_interaction_target() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.onboarding_needs_api_key = false;
        let mut terminal =
            Terminal::new(TestBackend::new(160, 1)).expect("info-line test terminal should build");

        terminal
            .draw(|frame| {
                let area = frame.area();
                let hitboxes = render_info_row(frame, &mut app, area, false);
                register_info_interaction_targets(&mut app, hitboxes);
            })
            .expect("info line should render");

        let segment = app
            .viewport
            .last_infoline_hitboxes
            .iter()
            .find(|hitbox| hitbox.id == crate::tui::infoline::InfoSegmentId::Model)
            .expect("a wide info line should paint its model segment");
        let target_for = |id| {
            app.viewport
                .interaction_targets
                .iter()
                .find(|target| target.id == id)
                .cloned()
                .unwrap_or_else(|| panic!("painted route segment should register {id:?}"))
        };
        // The segment reads `provider · model · effort`. Pointing at the
        // provider is a different request from pointing at the model, so the
        // one target became two: the whole span used to open `/provider` no
        // matter which name was under the pointer.
        let provider = target_for(crate::tui::tideline::InteractionTargetId::HEADER_ROUTE);
        let model = target_for(crate::tui::tideline::InteractionTargetId::HEADER_MODEL);

        assert_eq!(provider.area.x, segment.area.x);
        assert!(
            provider.area.right() < model.area.x,
            "provider {:?} and model {:?} must not overlap",
            provider.area,
            model.area
        );
        assert_eq!(model.area.right(), segment.area.right());
        assert_eq!(
            provider.keyboard_action,
            Some(crate::tui::tideline::InteractionAction::OpenProviderPicker)
        );
        assert_eq!(
            model.keyboard_action,
            Some(crate::tui::tideline::InteractionAction::OpenModelPicker)
        );
        for target in [&provider, &model] {
            assert_eq!(target.mouse_action, target.keyboard_action);
            assert_eq!(
                target.inspect_detail,
                crate::tui::tideline::InspectDetail::Route
            );
        }
    }

    /// U3: a keyless first run keeps a default model id, but nothing can
    /// answer it. The route chip says "not connected" instead of naming that
    /// route, and it is not a route control until a model is connected.
    #[test]
    fn keyless_launch_route_chip_says_not_connected() {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.ui_locale = codewhale_localization::Locale::En;
        app.onboarding_needs_api_key = true;
        let (_, model) = app.effective_route_identity_display();
        assert!(!model.is_empty(), "the fixture carries a default model id");
        let mut terminal =
            Terminal::new(TestBackend::new(160, 1)).expect("info-line test terminal should build");
        let mut hitboxes = super::InfoLineInteractionHitboxes::default();
        terminal
            .draw(|frame| {
                let area = frame.area();
                hitboxes = render_info_row(frame, &mut app, area, false);
            })
            .expect("info line should render");
        let row: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(row.contains("not connected"), "{row:?}");
        assert!(!row.contains(&model), "a dead route is not named: {row:?}");
        assert!(hitboxes.route.is_none(), "no provider control: {row:?}");

        // Once a key lands the same row names the route again.
        app.onboarding_needs_api_key = false;
        let segments = super::info_segments(&app, 160);
        assert!(
            segments.iter().any(
                |segment| segment.id == crate::tui::infoline::InfoSegmentId::Model
                    && segment.value.contains(&model)
            ),
            "{segments:?}"
        );
    }

    /// "Where did the github info go?" — the workspace segment names the
    /// repository when `origin` resolves to a forge slug, and only falls back
    /// to the folder basename when it does not. The basename rides along as
    /// the segment's shorter form so a long slug never costs the row a whole
    /// fact.
    #[test]
    fn truncates_at_ascii_word_boundary() {
        assert_eq!(short_title_truncate("hello world foo", 10), "hello…");
    }

    #[test]
    fn truncates_non_ascii_titles_by_char_count_not_bytes() {
        // `str::rfind` returns a byte offset; using it as a char count used to
        // cut past the limit and mid-word on multi-byte input.
        assert_eq!(
            short_title_truncate("你好 world and more", 10),
            "你好 world…"
        );
    }

    #[test]
    fn truncates_at_punctuation_boundary() {
        assert_eq!(short_title_truncate("hello, world", 8), "hello…");
    }

    #[test]
    fn truncates_mid_word_when_no_boundary_exists() {
        assert_eq!(short_title_truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn leaves_short_titles_untouched() {
        assert_eq!(short_title_truncate("short", 10), "short");
    }

    // ── #5950: the bottom chrome is the user's to compose ─────────────

    use crate::config::StatusItem;
    use crate::tui::app::App;
    use crate::tui::infoline::{InfoLine, InfoSegmentId};

    /// A session whose context is `pct` full, by pinning the route's window
    /// to a multiple of what this conversation actually estimates. Nothing
    /// here fakes the reading itself — it goes through
    /// `context_usage_snapshot` like the live shell does.
    fn app_with_context_percent(pct: u8) -> App {
        use codewhale_models::{ContentBlock, Message};
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        // A keyless test config would paint "not connected" (U3); these
        // readings are about a connected route.
        app.onboarding_needs_api_key = false;
        app.api_messages = std::sync::Arc::new(vec![Message {
            role: codewhale_models::Role::User,
            content: vec![ContentBlock::Text {
                text: "context ".repeat(400),
                cache_control: None,
            }],
        }]);
        let (used, _, _) =
            super::context_usage_snapshot(&app).expect("a conversation has a context reading");
        let window = (used as f64 * 100.0 / f64::from(pct)).round().max(1.0);
        app.active_route_limits = Some(codewhale_config::route::RouteLimits {
            context_tokens: Some(window as u64),
            ..Default::default()
        });
        assert_eq!(
            super::info_context_percent(&app),
            pct,
            "fixture should land exactly on {pct}%"
        );
        app
    }

    /// The metrics line as the user reads it, at `width`.
    fn metrics_row(app: &App, width: u16) -> String {
        let segments = super::info_segments(app, width);
        let hint = crate::tui::shell_key_routing::info_help_hint(app.ui_locale);
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).expect("metrics-line terminal");
        terminal
            .draw(|frame| {
                use ratatui::widgets::Widget as _;
                let area = frame.area();
                InfoLine::new(&app.ui_theme, &hint, &segments).render(area, frame.buffer_mut());
            })
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn footer_keeps_reasoning_label_for_every_effort_tier() {
        use crate::reasoning_preference::ReasoningEffort;

        let mut app = app_with_context_percent(1);
        app.api_provider = crate::config::ProviderKind::Openai;
        app.active_route_base_url = "https://api.openai.com/v1".to_string();
        app.model = "gpt-5.6".to_string();
        app.auto_model = false;
        app.ui_locale = codewhale_localization::Locale::En;

        let mut missing = Vec::new();
        for effort in [
            ReasoningEffort::Off,
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Ultra,
            ReasoningEffort::Auto,
            ReasoningEffort::Max,
        ] {
            app.reasoning_effort = effort;
            let label = app.reasoning_effort_display_label();
            assert!(!label.is_empty(), "{effort:?} must have a label");
            let row = metrics_row(&app, 80);
            if !row.contains(&format!("thinking: {label}")) {
                missing.push(format!("{effort:?}: {row:?}"));
            }
        }
        assert!(missing.is_empty(), "missing footer labels: {missing:#?}");
    }

    /// The reading used to go silent below 50% fullness, which is most of a
    /// session (#5950). It is a reading, not an alarm: it states 10% as
    /// readily as 60%, and only the ink changes at the thresholds.
    #[test]
    fn context_reading_paints_at_every_fullness() {
        for pct in [10u8, 60] {
            let app = app_with_context_percent(pct);
            let segment = super::info_segments(&app, 160)
                .into_iter()
                .find(|segment| segment.id == InfoSegmentId::Context)
                .unwrap_or_else(|| panic!("{pct}%: the context reading must be on the row"));
            assert_eq!(segment.value, format!("{pct}%"));
            assert_eq!(
                segment.ink,
                codewhale_palette::ChromeInk::Info,
                "{pct}%: below the cap the reading is a status, not a failure"
            );
            // Narrow rows keep it too: the reading is the row's floor and
            // sheds after everything else, including the help hint.
            for width in [40u16, 80, 160] {
                let row = metrics_row(&app, width);
                assert!(
                    row.contains(&format!("context {pct}%")),
                    "{pct}% at {width} columns: {row:?}"
                );
            }
        }
    }

    /// The warning ink still belongs to the thresholds it always used: the
    /// error token from 80% up, and not one percent earlier.
    #[test]
    fn context_reading_keeps_its_warning_threshold() {
        for (pct, expected) in [
            (10u8, codewhale_palette::ChromeInk::Info),
            (79, codewhale_palette::ChromeInk::Info),
            (80, codewhale_palette::ChromeInk::Attention),
        ] {
            let app = app_with_context_percent(pct);
            let segment = super::info_segments(&app, 160)
                .into_iter()
                .find(|segment| segment.id == InfoSegmentId::Context)
                .expect("context reading");
            assert_eq!(segment.ink, expected, "{pct}%");
        }
    }

    /// `/statusline` drives this row. Between 0.9.12 and #5950 the picker
    /// persisted a list nothing read, so every toggle in it was a lie.
    #[test]
    fn statusline_toggle_removes_its_segment_on_the_next_frame() {
        use crate::tui::views::{ModalView, ViewAction, ViewEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = app_with_context_percent(10);
        assert!(
            metrics_row(&app, 160).contains("context 10%"),
            "the reading starts on the row"
        );

        let mut picker = crate::tui::views::status_picker::StatusPickerView::new(
            &app.status_items,
            app.api_provider,
            app.ui_locale,
        );
        // Walk to the context row the way a user does, then uncheck it.
        let context_row = StatusItem::all()
            .iter()
            .filter(|item| item.is_available_for(app.api_provider))
            .position(|item| *item == StatusItem::ContextPercent)
            .expect("the picker offers the context reading");
        for _ in 0..context_row {
            picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let action = picker.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let ViewAction::Emit(ViewEvent::StatusItemsUpdated { items, .. }) = action else {
            panic!("space should emit a live preview: {action:?}");
        };
        assert!(!items.contains(&StatusItem::ContextPercent));

        // What the handler does with the event, and then the next frame.
        app.status_items = items;
        let row = metrics_row(&app, 160);
        assert!(
            !row.contains("context "),
            "the toggle must take it off: {row:?}"
        );
        assert!(
            row.contains("deepseek"),
            "and must take nothing else with it: {row:?}"
        );
    }

    /// A custom OpenAI-compatible route without an endpoint receipt cannot
    /// prove its effective tier. The route segment used to print
    /// `high→effective unavailable` — a placeholder that could never resolve
    /// (#5950). It now states no effort field at all, while a first-party
    /// route keeps its tier label.
    #[test]
    fn unprovable_effort_states_no_field_instead_of_a_placeholder() {
        use crate::tui::phase_strip::{RouteFieldKind, route_identity_fields};
        use crate::tui::underwater::ShellTier;

        let mut app = app_with_context_percent(10);
        app.set_provider_identity(crate::config::ProviderKind::Custom, "my-gateway");
        app.auto_model = false;
        app.active_route_base_url = "https://gateway.example/v1".to_string();
        app.model = "vendor-model-x".to_string();
        app.reasoning_effort = crate::reasoning_preference::ReasoningEffort::High;
        assert_eq!(
            app.reasoning_effort_display_label(),
            "high→effective unavailable",
            "the full label still tells /status the truth"
        );
        assert_eq!(app.provable_reasoning_effort_label(), None);
        let fields = route_identity_fields(&app, ShellTier::Wide, 200).expect("route fields");
        assert!(
            fields
                .iter()
                .all(|field| field.kind != RouteFieldKind::Effort),
            "no effort field on an unprovable route: {fields:?}"
        );
        let row = metrics_row(&app, 200);
        assert!(row.contains("vendor-model-x"), "{row:?}");
        // The unresolvable effort placeholder stays out; the localized
        // missing-cost explanation ("rate unavailable") is a separate,
        // legitimate reading.
        assert!(!row.contains("high→effective unavailable"), "{row:?}");
        assert!(!row.contains("high"), "{row:?}");

        // First-party routes are unchanged: the tier label stays.
        let app = app_with_context_percent(10);
        let label = app
            .provable_reasoning_effort_label()
            .expect("a first-party route proves its tier");
        assert_eq!(label, app.reasoning_effort_display_label());
        let fields = route_identity_fields(&app, ShellTier::Wide, 200).expect("route fields");
        assert!(
            fields
                .iter()
                .any(|field| field.kind == RouteFieldKind::Effort
                    && field.text == format!("thinking: {label}")),
            "{fields:?}"
        );
    }

    /// DeepSeek's clock-tiered routes show which tier the next turn buys,
    /// beside the cost; flat routes and other vendors show nothing.
    #[test]
    fn deepseek_tiered_routes_paint_the_billing_tier_beside_the_cost() {
        use crate::config::ProviderKind;
        use chrono::TimeZone as _;
        let mut app = app_with_context_percent(10);
        app.auto_model = false;
        app.api_provider = ProviderKind::Deepseek;
        app.model = "deepseek-v4-flash".to_string();
        // Wednesday 2026-09-16: 02:00Z is inside the 01:00-04:00 peak
        // window, 12:00Z outside every window.
        let peak = chrono::Utc.with_ymd_and_hms(2026, 9, 16, 2, 0, 0).unwrap();
        let off = chrono::Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        assert_eq!(
            super::billing_tier_label(&app, peak).as_deref(),
            Some("peak")
        );
        assert_eq!(
            super::billing_tier_label(&app, off).as_deref(),
            Some("off-peak")
        );
        let ids: Vec<InfoSegmentId> = super::info_segments(&app, 200)
            .iter()
            .map(|segment| segment.id)
            .collect();
        assert!(ids.contains(&InfoSegmentId::BillingTier), "{ids:?}");
        let row = metrics_row(&app, 200);
        assert!(row.contains("peak"), "the tier reads in the row: {row:?}");

        // A flat-priced DeepSeek model has no tier to show.
        app.model = "deepseek-chat".to_string();
        assert_eq!(super::billing_tier_label(&app, peak), None);
        let ids: Vec<InfoSegmentId> = super::info_segments(&app, 200)
            .iter()
            .map(|segment| segment.id)
            .collect();
        assert!(!ids.contains(&InfoSegmentId::BillingTier), "{ids:?}");

        // Another vendor serving a DeepSeek id is priced on its own terms.
        app.model = "deepseek-v4-flash".to_string();
        app.api_provider = ProviderKind::Openai;
        assert_eq!(super::billing_tier_label(&app, peak), None);

        // Auto routing has not pinned a model, so there is nothing to claim.
        app.api_provider = ProviderKind::Deepseek;
        app.auto_model = true;
        assert_eq!(super::billing_tier_label(&app, peak), None);
    }

    /// A provider switch must not hide missing historical coverage.
    #[test]
    fn cost_unknown_preserves_saved_coverage_across_route_changes() {
        use crate::route_billing::BillingPresentation;
        let mut app = app_with_context_percent(10);
        app.session.cost_coverage_unknown_legacy = true;

        app.billing_presentation = BillingPresentation::Metered;
        assert!(matches!(
            app.cumulative_usage_chip(),
            crate::route_billing::UsageChip::Unknown(_)
        ));
        assert_eq!(
            super::session_cost_label(&app),
            "cost: unknown (saved coverage unavailable)"
        );
        let row = metrics_row(&app, 200);
        assert!(
            row.contains("cost: unknown"),
            "a priceable route keeps the honesty: {row:?}"
        );

        app.billing_presentation = BillingPresentation::Unknown;
        assert!(matches!(
            app.cumulative_usage_chip(),
            crate::route_billing::UsageChip::Unknown(_)
        ));
        assert_eq!(
            super::session_cost_label(&app),
            "cost: unknown (saved coverage unavailable)"
        );
        let ids: Vec<InfoSegmentId> = super::info_segments(&app, 200)
            .iter()
            .map(|segment| segment.id)
            .collect();
        assert!(ids.contains(&InfoSegmentId::Cost), "{ids:?}");
        let row = metrics_row(&app, 200);
        assert!(
            row.contains("saved coverage unavailable"),
            "an unclassified route preserves the reason: {row:?}"
        );
        assert!(
            row.contains("context 10%"),
            "and nothing else moves: {row:?}"
        );

        // A real price on an otherwise unclassified route still prints.
        app.session.cost_coverage_unknown_legacy = false;
        app.session.cost_priced_turns = 1;
        app.session.session_cost = 0.42;
        assert!(
            matches!(
                app.cumulative_usage_chip(),
                crate::route_billing::UsageChip::Money(_)
            ),
            "{:?}",
            app.cumulative_usage_chip()
        );
        assert!(!super::session_cost_label(&app).is_empty());
    }

    #[test]
    fn metrics_line_uses_measured_request_average_during_tool_waits_and_live_text() {
        use crate::tui::session_metrics::{full_text, snapshot_from_app};

        let mut app = app_with_context_percent(60);
        app.ui_locale = codewhale_localization::Locale::En;
        app.status_items = vec![StatusItem::SessionMetrics, StatusItem::Tokens];
        app.is_loading = true;
        app.turn_started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(120));
        app.streaming_output_token_estimate = 60_000;
        assert!(
            super::info_segments(&app, 200)
                .iter()
                .all(|segment| segment.id != InfoSegmentId::Rate),
            "live text estimates do not invent measured request throughput"
        );
        app.session_metrics
            .record_model_call(120, 4_800, Some(1_000), Some(5_000));
        let rate = |app: &App| {
            super::info_segments(app, 200)
                .into_iter()
                .find(|segment| segment.id == InfoSegmentId::Rate)
                .map(|segment| segment.value)
        };
        assert_eq!(rate(&app).as_deref(), Some("24 avg tok/s"));
        let detailed = full_text(snapshot_from_app(&app), app.ui_locale, false);
        assert!(detailed.contains("24 avg tok/s"), "{detailed}");

        // Finishing a long turn or replacing the displayed token receipt must
        // not switch the rate to the turn timer (which includes tool waits).
        app.is_loading = false;
        app.session.last_completion_tokens = Some(9_000);
        assert_eq!(rate(&app).as_deref(), Some("24 avg tok/s"));
        app.status_items = vec![StatusItem::Tokens];
        assert_eq!(rate(&app), None, "the existing status toggle still owns it");
    }

    #[test]
    fn default_compact_footer_keeps_measured_performance_at_working_widths() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = app_with_context_percent(60);
        app.ui_locale = codewhale_localization::Locale::En;
        app.status_items = StatusItem::default_footer();
        app.metrics_line = crate::config::ChromeRowPreset::Compact;
        app.session_metrics
            .record_model_call(120, 4_800, Some(1_000), Some(5_000));
        for width in [80, 100, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
            terminal
                .draw(|frame| {
                    super::render_info_row(frame, &mut app, frame.area(), false);
                })
                .unwrap();
            let row: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(row.contains("ttft 1.0s"), "{width}: {row}");
            assert!(row.contains("24 avg tok/s"), "{width}: {row}");
            assert!(!row.contains("/help"), "{width}: {row}");
        }
    }

    #[test]
    fn performance_readings_can_be_selected_independently() {
        let mut app = app_with_context_percent(60);
        app.session_metrics
            .record_model_call(120, 4_800, Some(1_000), Some(5_000));
        for (item, expected) in [
            (StatusItem::Ttft, InfoSegmentId::Ttft),
            (StatusItem::OutputRate, InfoSegmentId::Rate),
        ] {
            app.status_items = vec![item];
            let ids: Vec<_> = super::info_segments(&app, 80)
                .into_iter()
                .map(|s| s.id)
                .collect();
            assert_eq!(ids, vec![expected]);
        }
    }

    /// Every remaining status item owns a segment, and an empty list leaves
    /// the row with nothing but the help hint — no toggle in `/statusline`
    /// paints something no toggle can remove.
    #[test]
    fn every_metrics_segment_answers_to_a_status_item() {
        let mut app = app_with_context_percent(60);
        app.session.last_prompt_tokens = Some(1_000);
        app.session_metrics
            .record_model_call(1_200, 30_000, Some(400), Some(30_400));
        app.streaming_output_token_estimate = 1_200;
        app.is_loading = true;
        app.turn_started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
        *app.balance_cell.lock().expect("balance cell") = Some(crate::pricing::BalanceInfo {
            currency: "USD".to_string(),
            total_balance: "4.32".to_string(),
            topped_up_balance: String::new(),
            granted_balance: String::new(),
        });
        app.status_items = StatusItem::all().to_vec();
        app.workspace_context = Some("main | clean".to_string());

        let ids: Vec<InfoSegmentId> = super::info_segments(&app, 200)
            .iter()
            .map(|segment| segment.id)
            .collect();
        for expected in [
            InfoSegmentId::Model,
            InfoSegmentId::Context,
            InfoSegmentId::Balance,
            InfoSegmentId::Ttft,
            InfoSegmentId::Rate,
            InfoSegmentId::OutputTokens,
            InfoSegmentId::Workspace,
            InfoSegmentId::GitBranch,
        ] {
            assert!(ids.contains(&expected), "{expected:?} missing from {ids:?}");
        }

        app.status_items = Vec::new();
        assert!(
            super::info_segments(&app, 200).is_empty(),
            "an empty status list leaves the metrics line empty"
        );
    }

    #[test]
    fn empty_session_keeps_opted_in_workspace_identity_visible() {
        let mut app = app_with_context_percent(0);
        app.workspace = std::path::PathBuf::from("/fixture/checkout");
        app.workspace_context = Some("feature-6112 | clean".to_string());
        app.status_items = vec![StatusItem::Workspace, StatusItem::GitBranch];
        app.metrics_line = crate::config::ChromeRowPreset::Compact;
        let backend = ratatui::backend::TestBackend::new(100, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::render_info_row(frame, &mut app, area, true);
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("checkout"), "{rendered}");
        assert!(rendered.contains("feature-6112"), "{rendered}");
    }

    /// #6112: the opt-in workspace and branch chips read cached state only —
    /// the workspace path and the TTL-refreshed `workspace_context` string —
    /// so neither costs IO per frame. Outside a repository the branch chip
    /// degrades to absent rather than pinning a placeholder dash.
    #[test]
    fn workspace_and_git_branch_chips_follow_cached_workspace_context() {
        let mut app = app_with_context_percent(60);
        app.status_items = vec![StatusItem::Workspace, StatusItem::GitBranch];

        let segments = super::info_segments(&app, 200);
        let workspace = segments
            .iter()
            .find(|segment| segment.id == InfoSegmentId::Workspace)
            .expect("workspace chip renders from the workspace path alone");
        assert_eq!(
            workspace.value,
            crate::tui::workspace_context::workspace_basename(&app.workspace)
        );
        assert!(
            segments
                .iter()
                .all(|segment| segment.id != InfoSegmentId::GitBranch),
            "outside a repository the branch chip is absent"
        );

        // A detached HEAD reads in its recorded short-SHA form.
        app.workspace_context = Some("detached:abc1234 | clean".to_string());
        let branch = super::info_segments(&app, 200)
            .into_iter()
            .find(|segment| segment.id == InfoSegmentId::GitBranch)
            .expect("branch chip renders from cached context");
        assert_eq!(branch.value, "detached:abc1234");
        app.workspace_is_linked_worktree = true;
        let linked = super::info_segments(&app, 200)
            .into_iter()
            .find(|segment| segment.id == InfoSegmentId::GitBranch)
            .unwrap();
        assert_eq!(linked.value, "detached:abc1234 (wt)");
        assert!(!StatusItem::default_footer().contains(&StatusItem::Workspace));
        assert!(!StatusItem::default_footer().contains(&StatusItem::GitBranch));

        // Off means off.
        app.status_items = Vec::new();
        assert!(super::info_segments(&app, 200).is_empty());
    }
}

#[cfg(test)]
mod one_owner_tests;

#[cfg(test)]
mod pet_main_tests {
    use super::*;
    use crate::tui::{
        approval::{ApprovalRequest, ApprovalView},
        pet_watch,
        shell_key_routing::Focus,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn app() -> App {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.onboarding = OnboardingState::None;
        app.redaction_gate = false;
        app.ui_locale = codewhale_localization::Locale::En;
        app.pet_watch.detach_for_test();
        pet_watch::set_enabled(&mut app, true);
        app
    }
    fn paint(
        app: &mut App,
        width: u16,
        height: u16,
    ) -> (ratatui::buffer::Buffer, Option<(u16, u16)>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut cursor = None;
        terminal
            .draw(|frame| cursor = render(frame, app, &Config::default()))
            .unwrap();
        (terminal.backend().buffer().clone(), cursor)
    }
    fn text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }
    fn capture(app: &App, name: &str, buf: &ratatui::buffer::Buffer, light: bool) {
        // Use the actual output backend's color projection, including light
        // contrast and terminal depth, before exporting the terminal cells.
        let mut buf = buf.clone();
        for cell in &mut buf.content {
            crate::tui::color_compat::adapt_cell_colors(
                cell,
                codewhale_palette::ColorDepth::detect(),
                app.ui_theme.mode,
                app.theme_id,
                &app.ui_theme,
                Some(app.ui_theme.surface_bg),
            );
        }
        if let Some(dir) = std::env::var_os("CODEWHALE_PET_CAPTURE_DIR") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let profile = if light {
                codewhale_ratatui::testing::Profile::LightTrue
            } else {
                codewhale_ratatui::testing::Profile::DarkTrue
            };
            std::fs::write(
                dir.join(format!("{name}.svg")),
                codewhale_ratatui::testing::svg(&buf, &profile.theme()),
            )
            .unwrap();
        }
    }
    #[test]
    fn pet_main_retains_native_composer_caret_pointer_and_paste_at_every_size() {
        for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32), (140, 40)] {
            let mut app = app();
            app.composer_border = true;
            app.input = "Help me build 中文".into();
            app.cursor_position = app.input.chars().count();
            let (buf, cursor) = paint(&mut app, width, height);
            assert_eq!(app.focus(), Focus::Composer);
            let cursor = cursor.expect("pet mode keeps the actual composer caret");
            let composer = app.viewport.last_composer_area.unwrap();
            assert!(composer.contains(cursor.into()), "{width}x{height}");
            assert!(app.viewport.last_transcript_area.is_none());
            assert!(app.work_surface.last_area.is_none());
            assert!(app.viewport.last_infoline_hitboxes.is_empty());
            assert!(text(&buf).contains("F5"));
            capture(&app, &format!("main-{width}x{height}"), &buf, false);
            assert!(handle_composer_mouse(
                &mut app,
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left
                    ),
                    column: cursor.0,
                    row: cursor.1,
                    modifiers: KeyModifiers::NONE,
                }
            ));
            assert_eq!(app.cursor_position, app.input.chars().count());
            handle_bracketed_paste(&mut app, " pasted");
            assert!(app.input.ends_with(" pasted"));
            assert!(app.view_stack.is_empty());
        }
    }
    #[test]
    fn pet_main_inspection_uses_the_same_key_and_pointer_route_and_returns_to_the_draft() {
        let mut app = app();
        app.input = "kept draft".into();
        app.cursor_position = app.input.chars().count();
        app.add_message(HistoryCell::Assistant {
            content: "INSPECTION_RECEIPT".into(),
            streaming: true,
        });
        let session = app.current_session_id.clone();
        let (buf, _) = paint(&mut app, 100, 32);
        assert!(!text(&buf).contains("INSPECTION_RECEIPT"));
        assert!(!pet_watch::handle_key(
            &mut app,
            &KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        ));
        assert!(pet_watch::handle_key(
            &mut app,
            &KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)
        ));
        let (buf, cursor) = paint(&mut app, 100, 32);
        assert!(text(&buf).contains("INSPECTION_RECEIPT"));
        assert!(cursor.is_none());
        app.view_stack
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.focus(), Focus::Composer);
        assert!(pet_watch::main_view(&app));
        paint(&mut app, 100, 32);
        let hint = app.pet_watch.inspect_area.unwrap();
        crate::tui::mouse_ui::handle_mouse_event(
            &mut app,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: hint.x,
                row: hint.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::PetHabitat));
        app.view_stack
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input, "kept draft");
        assert_eq!(app.current_session_id, session);
        pet_watch::set_enabled(&mut app, false);
        let (buf, cursor) = paint(&mut app, 100, 32);
        assert!(text(&buf).contains("INSPECTION_RECEIPT"));
        assert!(cursor.is_some());
    }
    #[test]
    fn pet_main_keeps_queue_feedback_and_cannot_take_approval_focus() {
        let mut app = app();
        app.is_loading = true;
        app.queue_message(crate::tui::app::QueuedMessage::new(
            "QUEUED_FIXTURE_MESSAGE".into(),
            None,
        ));
        let (buf, _) = paint(&mut app, 80, 24);
        assert!(text(&buf).contains("QUEUED_FIXTURE_MESSAGE"));
        app.view_stack.push(ApprovalView::new(ApprovalRequest::new(
            "pet-approval",
            "exec_shell",
            "Review command",
            &serde_json::json!({"command":"git status"}),
            "pet-approval-key",
        )));
        assert!(!pet_watch::handle_key(
            &mut app,
            &KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)
        ));
        let (_, cursor) = paint(&mut app, 80, 24);
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Approval));
        assert_eq!(app.focus(), Focus::Modal(ModalKind::Approval));
        assert!(cursor.is_none());
        assert_eq!(app.queued_messages.len(), 1);
    }
    #[test]
    fn pet_main_light_render_keeps_the_same_live_composer() {
        let mut app = app();
        app.theme_id = codewhale_palette::ThemeId::ShorelineLight;
        app.ui_theme = app.theme_id.ui_theme();
        app.input = "What should we work on?".into();
        app.cursor_position = app.input.chars().count();
        let (buf, cursor) = paint(&mut app, 100, 32);
        assert!(cursor.is_some());
        assert_eq!(app.focus(), Focus::Composer);
        capture(&app, "main-light-100x32", &buf, true);
    }
    #[test]
    fn pet_inspector_keeps_the_copy_receipt_visible_and_returns_to_the_draft() {
        let _env = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
        let _state = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        for light in [false, true] {
            let mut app = app();
            if light {
                app.theme_id = codewhale_palette::ThemeId::ShorelineLight;
                app.ui_theme = app.theme_id.ui_theme();
            }
            app.clipboard = crate::tui::clipboard::ClipboardHandler::for_test(false, false);
            app.input = "My next message".into();
            let reply = "Pet mode\n\nYour view is remembered when Codewhale starts.\n\n- F5 opens replies and agents.\n- Escape returns to your draft.\n- c copies the last finished reply.";
            let index = app.history.len();
            app.add_message(HistoryCell::Assistant {
                content: reply.into(),
                streaming: false,
            });
            app.record_completed_assistant_output(index, reply);
            assert!(pet_watch::handle_key(
                &mut app,
                &KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)
            ));
            let result =
                crate::commands::execute_with_config("/copy", &mut app, &Config::default());
            assert!(!result.is_error);
            let (buf, cursor) = paint(&mut app, 100, 32);
            let output = text(&buf);
            assert!(
                output.contains("Accepted the last completed assistant response"),
                "copy feedback must be painted above the hidden transcript: {output}"
            );
            assert!(output.contains("c copy finished reply"));
            assert!(cursor.is_none());
            capture(
                &app,
                if light {
                    "inspector-copy-light-100x32"
                } else {
                    "inspector-copy-dark-100x32"
                },
                &buf,
                light,
            );
            app.view_stack
                .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert_eq!(app.input, "My next message");
            assert_eq!(app.focus(), Focus::Composer);
        }
    }
}
