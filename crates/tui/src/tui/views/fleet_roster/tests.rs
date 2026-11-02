use super::*;
use crate::tui::views::ViewStack;
use crossterm::event::KeyModifiers;
use std::collections::BTreeMap;
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mouse(kind: MouseEventKind, area: Rect) -> MouseEvent {
    MouseEvent {
        kind,
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    }
}

fn setup_member_id(action: ViewAction) -> Option<String> {
    match action {
        ViewAction::Emit(ViewEvent::FleetRosterOpenSetupRequested { member_id }) => Some(member_id),
        _ => None,
    }
}

fn operator() -> OperatorInfo {
    OperatorInfo {
        provider: "DeepSeek".to_string(),
        provider_id: "deepseek".to_string(),
        model: "deepseek-v4-pro".to_string(),
        reasoning: "Auto".to_string(),
    }
}

fn built_in_view() -> FleetRosterView {
    FleetRosterView::from_parts(operator(), FleetRoster::built_ins_only(), None)
}

fn view_with_overrides() -> FleetRosterView {
    let mut members = FleetRoster::built_ins_only()
        .members()
        .iter()
        .filter(|m| !m.id.trim().eq_ignore_ascii_case("operator"))
        .cloned()
        .collect::<Vec<_>>();
    // A project override of the built-in reviewer with a pinned model and
    // an instruction overlay.
    if let Some(reviewer) = members.iter_mut().find(|m| m.id == "reviewer") {
        reviewer.origin = ProfileOrigin::Workspace;
        reviewer.source = PathBuf::from(".codewhale/agents/reviewer.toml");
        reviewer.profile.model = Some("glm-5.2".to_string());
        reviewer.profile.role.instructions = Some("Review hard.".to_string());
        reviewer.profile.delegation.max_spawn_depth = Some(1);
    }
    FleetRosterView {
        operator: operator(),
        members,
        shadowed: Vec::new(),
        selected_fleet: None,
        load_error: None,
        selected: 0,
        detail_scroll: 0,
        row_hitboxes: RefCell::new(Vec::new()),
        last_mouse_selected: None,
        hovered_row: Cell::new(None),
        workers_hitbox: Cell::new(None),
        hovered_workers: Cell::new(false),
        surface_bg: palette::UI_THEME.surface_bg,
        locale: Locale::En,
    }
}

fn selected_fleet_view() -> FleetRosterView {
    let mut view = view_with_overrides();
    view.selected_fleet = Some(SelectedFleetSummary {
        name: "Default".to_string(),
        scope: codewhale_engine::fleet::store::FleetScope::Personal,
    });
    view
}

#[test]
fn session_subagent_tab_is_named_workers_not_durable_runs() {
    assert_eq!(
        tr(Locale::En, MessageId::FleetRosterWorkers),
        "workers",
        "the w tab opens current-session sub-agents; durable runs belong to /fleet runs"
    );
}

fn render_through_stack(make: impl Fn() -> FleetRosterView, w: u16, h: u16) -> Vec<String> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    for y in 0..h {
        for x in 0..w {
            buf[(x, y)].set_symbol("X");
        }
    }
    let mut stack = ViewStack::new();
    stack.push(make());
    stack.render(area, &mut buf);
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// #4208: every role mark and control glyph on the roster — operator,
/// role shapes, selection arrows, scroll rails — must narrow to an
/// ASCII-safe alternative.
#[test]
fn fleet_roster_glyphs_all_have_ascii_alternatives() {
    let rows = render_through_stack(view_with_overrides, 100, 30);
    for ch in rows.join("\n").chars().filter(|ch| !ch.is_ascii()) {
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_symbol(&ch.to_string());
        crate::tui::color_compat::adapt_cell_symbol_for_ascii(&mut cell);
        assert!(
            cell.symbol().is_ascii(),
            "fleet glyph {ch:?} (U+{:04X}) lacks an ASCII-safe alternative",
            ch as u32
        );
    }
}

#[test]
fn operator_row_is_pinned_first_with_the_session_model() {
    let rows = render_through_stack(built_in_view, 100, 30);
    let text = rows.join("\n");
    // The operator row leads the list and the detail pane (row 0 is
    // selected on open) shows the live session route.
    let operator_row = rows
        .iter()
        .position(|row| row.contains("Coordinator"))
        .expect("operator row rendered");
    let first_member_row = rows
        .iter()
        .position(|row| row.contains("manager"))
        .expect("first member rendered");
    assert!(
        operator_row < first_member_row,
        "operator must render above the first member"
    );
    assert!(
        text.contains("▸ @ Coordinator"),
        "operator selected on open"
    );
    assert!(text.contains("deepseek-v4-pro"), "session model shown");
    assert!(text.contains("full session access"), "{text}");
    // Inline field labels can wrap the role at this width. Read just the
    // inspector columns, excluding the independently positioned member list.
    let detail_start = rows
        .iter()
        .find_map(|row| row.find("Role  ").map(|index| row[..index].chars().count()))
        .expect("role field rendered");
    let detail = rows
        .iter()
        .map(|row| {
            row.chars()
                .skip(detail_start)
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(detail.contains("leads the Fleet"), "{text}");
}

#[test]
fn arrows_move_selection_and_wrap() {
    let mut view = built_in_view();
    let last = view.members.len();
    assert_eq!(view.selected, 0);

    view.handle_key(key(KeyCode::Up));
    assert_eq!(
        view.selected, last,
        "up from the operator wraps to the last member (#4755)"
    );

    view.handle_key(key(KeyCode::Down));
    assert_eq!(
        view.selected, 0,
        "down from the last member wraps to the operator"
    );

    view.handle_key(key(KeyCode::Down));
    assert_eq!(view.selected, 1, "first member follows the operator");

    // A full cycle of the roster returns to where it started.
    for _ in 0..=last {
        view.handle_key(key(KeyCode::Down));
    }
    assert_eq!(view.selected, 1, "one full cycle is the identity");
}

#[test]
fn selection_change_resets_detail_scroll() {
    let mut view = built_in_view();
    // Bare paging drives the row list; Shift-modified paging scrolls the
    // detail pane (#6290, #6014-style split).
    view.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::SHIFT));
    assert_eq!(view.detail_scroll, 8);
    view.handle_key(key(KeyCode::Down));
    assert_eq!(view.detail_scroll, 0);
}

#[test]
fn bare_paging_drives_rows_not_the_detail_pane() {
    let mut view = built_in_view();
    let last = view.members.len();
    view.handle_key(key(KeyCode::PageDown));
    assert_eq!(view.detail_scroll, 0);
    assert_eq!(view.selected, 10.min(last));
    view.handle_key(key(KeyCode::Home));
    assert_eq!(view.selected, 0);
}

#[test]
fn enter_opens_role_assignment_and_keeps_the_roster_underneath() {
    // Coordinator changes the current session through the shared picker.
    let mut view = built_in_view();
    assert!(view.operator_selected());
    assert!(
        matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::Emit(ViewEvent::FleetRosterOpenCoordinatorRequested)
        ),
        "Enter opens the Coordinator model picker"
    );

    // Member assignment retains this roster on the view stack.
    view.handle_key(key(KeyCode::Down));
    let action = view.handle_key(key(KeyCode::Enter));
    let ViewAction::Emit(ViewEvent::FleetRosterOpenSetupRequested { member_id }) = action else {
        panic!("Enter should hand off to the setup wizard");
    };
    assert_eq!(member_id, "manager");
}

#[test]
fn mouse_selection_reveals_details_then_activates_the_same_member_as_enter() {
    let area = Rect::new(0, 0, 100, 30);
    let mut view = built_in_view();
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    let manager_row = view
        .row_hitboxes
        .borrow()
        .iter()
        .find_map(|(rect, action)| (action.row() == 1).then_some(*rect))
        .expect("manager row hitbox");
    let click = mouse(MouseEventKind::Down(MouseButton::Left), manager_row);

    view.detail_scroll = 8;
    assert!(matches!(view.handle_mouse(click), ViewAction::None));
    assert_eq!(view.selected, 1);
    assert_eq!(view.detail_scroll, 0);

    // Selection owns the existing detail pane; clicking does not invent a
    // second details route.
    let mut selected_buf = Buffer::empty(area);
    view.render(area, &mut selected_buf);
    let selected_text = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .map(|(x, y)| selected_buf[(x, y)].symbol())
        .collect::<String>();
    assert!(selected_text.contains("Role"), "{selected_text}");
    assert!(selected_text.contains("manager"), "{selected_text}");

    let mouse_member = setup_member_id(view.handle_mouse(click)).expect("second click activates");
    let mut keyboard = built_in_view();
    keyboard.handle_key(key(KeyCode::Down));
    let keyboard_member =
        setup_member_id(keyboard.handle_key(key(KeyCode::Enter))).expect("Enter activates");
    assert_eq!(mouse_member, keyboard_member);
}

#[test]
fn hover_tints_roster_row_without_moving_selection() {
    let area = Rect::new(0, 0, 100, 30);
    let mut view = built_in_view();
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    assert_eq!(view.selected, 0);
    let manager_row = view
        .row_hitboxes
        .borrow()
        .iter()
        .find_map(|(rect, action)| (action.row() == 1).then_some(*rect))
        .expect("manager row hitbox");

    assert!(matches!(
        view.handle_mouse(mouse(MouseEventKind::Moved, manager_row)),
        ViewAction::None
    ));
    assert_eq!(view.hovered_row.get(), Some(1));
    assert_eq!(view.selected, 0);

    // Repaint: the hovered row wears the shared hover band on its text.
    let mut hovered_buf = Buffer::empty(area);
    view.render(area, &mut hovered_buf);
    assert_eq!(
        hovered_buf[(manager_row.x, manager_row.y)].bg,
        codewhale_palette::SURFACE_ELEVATED,
        "hovered roster row must show the shared hover band"
    );

    // Keyboard motion clears the tint so a stale row never glows.
    view.handle_key(key(KeyCode::Down));
    assert_eq!(view.hovered_row.get(), None);
}

#[test]
fn mouse_wheel_and_arrow_keys_share_roster_selection_semantics() {
    let mut mouse_view = built_in_view();
    let mut keyboard_view = built_in_view();
    let anywhere = Rect::new(0, 0, 1, 1);

    mouse_view.handle_mouse(mouse(MouseEventKind::ScrollDown, anywhere));
    keyboard_view.handle_key(key(KeyCode::Down));
    assert_eq!(mouse_view.selected, keyboard_view.selected);
    assert_eq!(mouse_view.detail_scroll, keyboard_view.detail_scroll);

    mouse_view.handle_mouse(mouse(MouseEventKind::ScrollUp, anywhere));
    keyboard_view.handle_key(key(KeyCode::Up));
    assert_eq!(mouse_view.selected, keyboard_view.selected);
    assert_eq!(mouse_view.detail_scroll, keyboard_view.detail_scroll);
}

#[test]
fn saved_profile_rows_keep_view_owned_typed_actions() {
    let area = Rect::new(0, 0, 160, 40);
    let view = built_in_view();
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    let hitboxes = view.row_hitboxes.borrow();
    assert_eq!(hitboxes.len(), view.row_count());
    for (_, action) in hitboxes.iter() {
        assert!(matches!(
            action,
            FleetRosterRowAction::SelectOrActivate { .. }
        ));
    }
}

#[test]
fn match_terminal_roster_surface_uses_reset_background() {
    let area = Rect::new(0, 0, 100, 30);
    let mut view = built_in_view();
    view.surface_bg = palette::TERMINAL_UI_THEME.surface_bg;
    let mut buf = Buffer::empty(area);
    for cell in &mut buf.content {
        cell.set_bg(Color::Red);
    }

    view.render(area, &mut buf);

    assert_eq!(view.surface_bg, Color::Reset);
    assert_eq!(buf[(0, 0)].bg, Color::Reset);
}

#[test]
fn selected_named_fleet_member_shows_edit_affordance() {
    let rows = render_through_stack(
        || {
            let mut view = selected_fleet_view();
            view.selected = 1;
            view
        },
        100,
        30,
    );
    let text = rows.join("\n");
    assert!(text.contains("Thinking"), "{text}");
    assert!(text.contains("Team `Default`"), "{text}");
    assert!(text.contains("Enter model & thinking"), "{text}");
    assert!(text.contains("Tab workers"), "{text}");
    assert!(text.contains("saved teams"), "{text}");
    // The letter-key wall is gone: one grammar, no `s`, `m`, `w`, PgUp hints.
    assert!(!text.contains("m model"), "{text}");
    assert!(!text.contains("PgUp"), "{text}");
}

/// #5954: the forward jump `Emit`s and keeps the roster on the stack, so the
/// view it opens is pushed on top and `Esc` there pops back here.
#[test]
fn tab_and_w_open_the_live_workers_tab() {
    for code in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Char('w')] {
        let mut view = built_in_view();
        assert!(
            matches!(
                view.handle_key(key(code)),
                ViewAction::Emit(ViewEvent::FleetRosterOpenWorkersRequested)
            ),
            "{code:?}"
        );
    }
}

/// #5954: same for the saved-teams jump.
#[test]
fn f_opens_saved_teams_without_closing_the_roster() {
    let mut view = built_in_view();
    assert!(matches!(
        view.handle_key(key(KeyCode::Char('f'))),
        ViewAction::Emit(ViewEvent::FleetRosterOpenFleetsRequested)
    ));
}

#[test]
fn esc_closes() {
    let mut view = built_in_view();
    assert!(matches!(
        view.handle_key(key(KeyCode::Esc)),
        ViewAction::Close
    ));
}

/// #5954, end to end on the real stack: `Tab` leaves the roster in place,
/// the host pushes workers on top, and `Esc` there returns to the roster
/// rather than closing the window.
#[test]
fn roster_tab_workers_esc_returns_to_the_roster() {
    let mut stack = ViewStack::new();
    stack.push(built_in_view());

    let events = stack.handle_key(key(KeyCode::Tab));
    assert!(
        matches!(
            events.as_slice(),
            [ViewEvent::FleetRosterOpenWorkersRequested]
        ),
        "{events:?}"
    );
    assert_eq!(
        stack.top_kind(),
        Some(ModalKind::FleetRoster),
        "the roster must survive the forward jump so it can be popped back to"
    );

    // What ui/handlers.rs does with that event.
    stack.push(crate::tui::views::SubAgentsView::new(Vec::new()).over_fleet_roster());
    assert_eq!(stack.top_kind(), Some(ModalKind::SubAgents));

    stack.handle_key(key(KeyCode::Esc));
    assert_eq!(
        stack.top_kind(),
        Some(ModalKind::FleetRoster),
        "Esc in workers must return to the roster, not close the window"
    );
}

/// #5954: `Esc` at the root of the Fleet stack still closes the window.
#[test]
fn esc_at_the_roster_root_closes_the_window() {
    let mut stack = ViewStack::new();
    stack.push(built_in_view());
    stack.handle_key(key(KeyCode::Esc));
    assert!(stack.is_empty(), "Esc at the roster must close the window");
}

#[test]
fn built_in_party_lists_all_members_in_canonical_order() {
    let view = built_in_view();
    let ids: Vec<&str> = view.members.iter().map(|m| m.id.as_str()).collect();
    // The operator is rendered as the pinned session row, not a member
    // (#dogfood 0.8.67), so it is intentionally absent from this list. There is
    // no built-in `general` member any more (#6244); `worker` is the posture
    // and the selector resolves the legacy name onto it.
    assert_eq!(
        ids,
        [
            "manager",
            "scout",
            "builder",
            "reviewer",
            "verifier",
            "consultant",
            "synthesizer",
            "worker",
            "planner",
            "custom"
        ]
    );
}

/// #5888 folded the legacy built-in `general` alias out of presentation while
/// keeping it in the roster. #6244 removed the member instead: two built-ins
/// that canonicalize to the same role make `role:general` permanently
/// `Ambiguous`. The lineup is unchanged — `general` was never presented — and
/// the name still resolves, through the identity selector rather than a second
/// member (proven in `fleet::roster`'s
/// `general_still_resolves_to_the_worker_member_without_its_own_built_in`).
#[test]
fn the_legacy_general_alias_has_no_built_in_member() {
    let view = built_in_view();
    let ids: Vec<&str> = view.members.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&"general"),
        "the alias must not be presented: {ids:?}"
    );
    assert!(ids.contains(&"worker"), "the posture's primary name stays");
    assert_eq!(view.row_count(), 11, "operator plus ten members");

    let roster = FleetRoster::built_ins_only();
    assert!(
        roster.get("general").is_none(),
        "the duplicate built-in is gone"
    );
    assert!(roster.get("worker").is_some());
}

/// A `general` the user actually defined — a saved-team member carries
/// Personal/Workspace origin, like `roster_from_fleet` produces — is the
/// user's own member and stays visible. Since #6244 there is no built-in
/// `general` to harvest, so the fixture derives one from `worker`, which is
/// exactly what a migrated fleet file looks like.
#[test]
fn user_defined_general_member_stays_visible() {
    let worker: AgentProfile = FleetRoster::built_ins_only()
        .members()
        .iter()
        .find(|m| m.id == "worker")
        .cloned()
        .expect("worker built-in");
    let mut user_general = worker.clone();
    user_general.id = "general".to_string();
    let mut members = vec![worker, user_general];
    for member in members.iter_mut().filter(|m| m.id == "general") {
        member.origin = ProfileOrigin::Personal;
        member.source = PathBuf::from("fleets/Default.toml");
    }
    let view = FleetRosterView::from_parts(operator(), FleetRoster::from_members(members), None);
    let ids: Vec<&str> = view.members.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&"general") && ids.contains(&"worker"),
        "a user-defined general is presented: {ids:?}"
    );
    assert_eq!(view.row_count(), 3, "operator plus both members");
}

#[test]
fn detail_shows_access_model_and_saved_for() {
    // Built-in reviewer: read-only files, full shell for its bounded
    // verification surface, network on. Inherits the session model.
    let reviewer = FleetRoster::built_ins_only()
        .get("reviewer")
        .unwrap()
        .clone();
    assert!(member_access_summary(&reviewer).contains("read-only files"));
    assert_eq!(
        member_routing_with_session(&reviewer, None),
        "same model as this session"
    );

    // Built-in scout: same Access shape as reviewer.
    let scout = FleetRoster::built_ins_only().get("scout").unwrap().clone();
    assert!(member_access_summary(&scout).contains("read-only files"));

    // Builder can edit files and run commands.
    let builder = FleetRoster::built_ins_only()
        .get("builder")
        .unwrap()
        .clone();
    assert!(member_access_summary(&builder).contains("can edit files"));

    // An explicit model beats the saved-set label, with no "(pinned)" jargon.
    let mut pinned = reviewer.clone();
    pinned.profile.model = Some("glm-5.2".to_string());
    assert_eq!(member_routing_with_session(&pinned, None), "model glm-5.2");
}

include!("../fleet_roster_capability_tests.rs");

#[test]
fn detail_lines_carry_overlay_source_for_project_members() {
    let view = view_with_overrides();
    let reviewer = view.members.iter().find(|m| m.id == "reviewer").unwrap();
    let text = member_detail_lines_with_session(reviewer, None, &view.shadowed, view.locale)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone().into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("project"), "{text}");
    assert!(
        text.contains("custom overlay (.codewhale/agents/reviewer.toml)"),
        "{text}"
    );
    assert!(text.contains("model glm-5.2"), "{text}");
    assert!(text.contains("spawn depth 1"), "{text}");
}

#[test]
fn roster_loads_config_members_through_the_shared_merge() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "docs-writer".to_string(),
        codewhale_config::FleetProfile {
            slot: codewhale_config::FleetSlot::from_name("scout"),
            role: codewhale_config::FleetRole {
                name: "scout".to_string(),
                description: Some("Writes docs.".to_string()),
                instructions: None,
            },
            loadout: codewhale_config::FleetLoadout::Fast,
            model: None,
            provider: None,
            reasoning_effort: None,
            permissions: codewhale_config::FleetProfilePermissions::default(),
            delegation: codewhale_config::FleetDelegationHints::default(),
        },
    );
    let config = codewhale_config::FleetConfigToml {
        profiles,
        ..codewhale_config::FleetConfigToml::default()
    };
    let view =
        FleetRosterView::from_parts(operator(), FleetRoster::load(&config, tmp.path()), None);
    let extra = view.members.iter().find(|m| m.id == "docs-writer").unwrap();
    assert_eq!(extra.origin, ProfileOrigin::Config);
    assert_eq!(
        member_routing_with_session(extra, None),
        "fast model, picked at launch"
    );
}

#[test]
fn detail_pane_reports_shadowed_lower_layers() {
    // #5098: a member whose winning layer ignores a personal file must
    // say so in the detail pane — the shadowed edit is no longer dropped
    // from every surface.
    let mut view = view_with_overrides();
    view.shadowed
        .push(codewhale_engine::fleet::roster::ShadowedProfile {
            id: "reviewer".to_string(),
            shadowed_origin: ProfileOrigin::Personal,
            shadowed_source: PathBuf::from("/home/op/.codewhale/agents/reviewer.toml"),
            winner_origin: ProfileOrigin::Workspace,
            winner_source: PathBuf::from(".codewhale/agents/reviewer.toml"),
        });
    let reviewer = view.members.iter().find(|m| m.id == "reviewer").unwrap();
    let text = member_detail_lines_with_session(reviewer, None, &view.shadowed, view.locale)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone().into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("Saved for"),
        "detail lists every layer for the id: {text}"
    );
    assert!(
        text.contains("project · .codewhale/agents/reviewer.toml (active)"),
        "detail names the winning layer: {text}"
    );
    assert!(
        text.contains("personal · /home/op/.codewhale/agents/reviewer.toml (ignored copy)"),
        "detail names the ignored file: {text}"
    );
}

#[test]
fn roster_row_badges_personal_copy_ignored() {
    // #5098: the list row itself must say the personal file is ignored,
    // not only the detail pane.
    let rows = render_through_stack(
        || {
            let mut view = view_with_overrides();
            view.shadowed
                .push(codewhale_engine::fleet::roster::ShadowedProfile {
                    id: "reviewer".to_string(),
                    shadowed_origin: ProfileOrigin::Personal,
                    shadowed_source: PathBuf::from("/home/op/.codewhale/agents/reviewer.toml"),
                    winner_origin: ProfileOrigin::Workspace,
                    winner_source: PathBuf::from(".codewhale/agents/reviewer.toml"),
                });
            view
        },
        120,
        32,
    );
    let text = rows.join("\n");
    assert!(
        text.contains("saved copy ignored"),
        "shadowed reviewer row is badged: {text}"
    );
}

#[test]
fn fleet_roster_is_usable_and_opaque_at_blocker_sizes() {
    type Builder = (&'static str, fn() -> FleetRosterView);
    let builders: [Builder; 3] = [
        ("built-ins", built_in_view),
        ("overrides", view_with_overrides),
        ("last-selected", || {
            let mut v = built_in_view();
            v.selected = v.row_count() - 1;
            v
        }),
    ];

    for (label, make) in builders {
        for (w, h) in BLOCKER_SIZES {
            let rows = render_through_stack(make, w, h);
            let text = rows.join("\n");

            // No bleed-through anywhere in the composited frame.
            assert!(
                !text.contains('X'),
                "{label} {w}x{h}: background bleed-through"
            );
            // Some action label is always visible.
            assert!(text.contains("close"), "{label} {w}x{h}: missing footer");
            // The first impression names the team as the worker/orchestration surface.
            assert!(
                text.contains("team") && text.contains("workers"),
                "{label} {w}x{h}: missing framing"
            );
            // The selected row's detail is on screen.
            assert!(
                text.contains("Access"),
                "{label} {w}x{h}: missing detail pane"
            );
            // No row overflows the frame width.
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{label} {w}x{h}: row {y} overflows: {row:?}"
                );
            }
        }
    }
}

/// Whale Teams: the selected member's detail pane carries the identity
/// block (badge plus `Name · species · job`) with no
/// caption labels and no state claim. The hand-drawn portrait art was
/// deleted per the 2026-08-29 founder directive, so no tier ever draws it.
#[test]
fn roster_detail_carries_whale_identity_without_claiming_state() {
    let wide = render_through_stack(
        || {
            let mut v = built_in_view();
            v.selected = 2; // scout
            v
        },
        120,
        32,
    )
    .join("\n");
    assert!(wide.contains("◂▰ Scout"), "{wide}");
    assert!(wide.contains("Scout · beaked whale · research"), "{wide}");
    assert!(
        !wide.contains("Whale identity"),
        "no caption labels: {wide}"
    );
    assert!(!wide.contains("identity only"), "no caption labels: {wide}");
    for word in ["Working", "Waiting for you", "Blocked", "Offline"] {
        assert!(!wide.contains(word), "roster must not claim {word}: {wide}");
    }

    let narrow = render_through_stack(
        || {
            let mut v = built_in_view();
            v.selected = 2;
            v
        },
        56,
        20,
    )
    .join("\n");
    assert!(narrow.contains("◂▰ Scout · beaked whale"), "{narrow}");
    assert!(
        !narrow.contains("▚△▞"),
        "the deleted portrait fluke must stay deleted, even at wide tiers: {narrow}"
    );
    assert!(
        !wide.contains("▚△▞"),
        "the deleted portrait fluke must stay deleted: {wide}"
    );
}

#[test]
fn selection_stays_visible_when_list_scrolls() {
    // Select the last member and render short: the pointer row must be
    // in the frame.
    let rows = render_through_stack(
        || {
            let mut v = built_in_view();
            v.selected = v.row_count() - 1;
            v
        },
        80,
        24,
    );
    let text = rows.join("\n");
    assert!(text.contains("▸ · custom"), "{text}");
}

#[test]
fn workbench_workers_header_is_clickable_without_moving_selection() {
    for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32), (140, 40)] {
        let mut view = built_in_view();
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let hit = view
            .workers_hitbox
            .get()
            .expect("visible Workers destination");
        assert!(area.contains((hit.x, hit.y).into()));
        view.handle_mouse(mouse(MouseEventKind::Moved, hit));
        assert!(view.hovered_workers.get());
        assert_eq!(view.selected, 0);
        assert!(matches!(
            view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), hit)),
            ViewAction::Emit(ViewEvent::FleetRosterOpenWorkersRequested)
        ));
    }
}
