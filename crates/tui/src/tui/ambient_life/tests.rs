use super::*;
use ratatui::text::Span;

fn dot_cameo(area: Rect, age: u128, activity: AmbientActivity) -> (Buffer, AmbientFrameStats) {
    let mut buf = Buffer::empty(area);
    let mut stats = AmbientFrameStats::default();
    pet_cameo::paint(
        area,
        &mut buf,
        Color::Cyan,
        &[],
        1.0,
        WhaleCameo {
            elapsed_ms: Some(age),
            anchor_x: area.x + area.width / 2,
            anchor_y: area.y + area.height / 2,
        },
        activity,
        &mut stats,
    );
    (buf, stats)
}

#[test]
fn dot_cameo_is_bounded_and_independent_of_draw_history() {
    let area = Rect::new(7, 11, 80, 24);
    let (first, stats) = dot_cameo(area, 500, AmbientActivity::Baseline);
    assert!(stats.marks_painted > 0);
    dot_cameo(area, 1_900, AmbientActivity::Subagents);
    dot_cameo(Rect::new(0, 0, 140, 40), 100, AmbientActivity::Baseline);
    assert_eq!(first, dot_cameo(area, 500, AmbientActivity::Baseline).0);
    assert_ne!(first, dot_cameo(area, 900, AmbientActivity::Baseline).0);
    assert_eq!(
        dot_cameo(area, 2_400, AmbientActivity::Baseline).1,
        AmbientFrameStats::default()
    );
    assert_eq!(
        dot_cameo(area, u128::MAX, AmbientActivity::Subagents).1,
        AmbientFrameStats::default()
    );
}

#[test]
fn dot_cameo_withholds_whole_body_when_label_touches_wide_text() {
    let area = Rect::new(7, 11, 80, 24);
    let (clear, clear_stats) = dot_cameo(area, 500, AmbientActivity::Baseline);
    let label_cell = clear
        .content
        .iter()
        .position(|cell| cell.symbol() == "o")
        .unwrap();
    let (x, y) = (
        label_cell % area.width as usize,
        label_cell / area.width as usize,
    );
    let mut lines = vec![Line::from(""); area.height as usize];
    lines[y] = Line::from(format!("{}鲸", " ".repeat(x)));
    let mut buf = Buffer::empty(area);
    buf.set_line(area.x, area.y + y as u16, &lines[y], area.width);
    let before = buf.clone();
    let mut stats = AmbientFrameStats::default();
    pet_cameo::paint(
        area,
        &mut buf,
        Color::Cyan,
        &lines,
        1.0,
        WhaleCameo {
            elapsed_ms: Some(500),
            anchor_x: area.x + 40,
            anchor_y: area.y + 12,
        },
        AmbientActivity::Baseline,
        &mut stats,
    );
    assert_eq!(
        before, buf,
        "a label collision must withhold every particle too"
    );
    assert_eq!(stats.marks_skipped_text, clear_stats.marks_built);
    assert_eq!(stats.marks_painted, 0);
}

#[test]
fn narrow_pod_members_are_withheld_instead_of_stacked() {
    let area = Rect::new(0, 0, 40, 12);
    let (buf, stats) = dot_cameo(area, 500, AmbientActivity::Subagents);
    let text: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(text.matches("agent · pod").count(), 1);
    assert!(stats.marks_clipped > 0);
    assert_eq!(stats.marks_built, stats.marks_painted + stats.marks_clipped);
}

#[test]
fn pet_settle_clock_does_not_extend_the_ocean_light_pulse() {
    let area = Rect::new(0, 0, 80, 24);
    let ramp = ocean::OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).unwrap();
    let column = |age| {
        OceanColumn::new(
            ramp,
            area,
            900,
            age,
            crate::tui::underwater::ShellPhase::Done,
            false,
            500,
            40,
        )
    };
    let settled = column(None);
    let pet_tail = column(Some(900));
    assert_eq!(pet_tail.completion_elapsed_ms(), Some(900));
    assert_eq!(pet_tail.ramp_fingerprint(), settled.ramp_fingerprint());
    for y in 0..area.height {
        assert_eq!(pet_tail.color_at_y(y), settled.color_at_y(y));
    }
}

#[test]
fn pet_widget_keeps_unknown_distinct_from_sleep_and_keeps_its_label() {
    use pet_sim::{ChannelId, PetSim, PetState};
    use ratatui::widgets::Widget;
    let mut sim = PetSim::whale();
    let mut state = PetState {
        channel: ChannelId::Other,
        lit: 0.3,
        ..PetState::rest()
    };
    sim.step(1.0 / 30.0, &state, false, 1.0);
    assert!(!sim.frame.hollow, "sleep is still observed");
    state.observed = 0.15;
    sim.step(1.0 / 30.0, &state, false, 1.0);
    assert!(sim.frame.hollow);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    pet_widget::PetWidget {
        sim: &sim,
        state: &state,
    }
    .render(buf.area, &mut buf);
    let text: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("other · drift · resting"));
    let mut narrow = Buffer::empty(Rect::new(0, 0, 18, 6));
    let before = narrow.clone();
    pet_widget::PetWidget {
        sim: &sim,
        state: &state,
    }
    .render(narrow.area, &mut narrow);
    assert_eq!(narrow, before, "never truncate away the uncertainty cue");
}

#[test]
fn ambient_min_dimensions_allow_small_windows() {
    const {
        assert!(AMBIENT_MIN_WIDTH < 68);
        assert!(AMBIENT_MIN_HEIGHT < 15);
    }
}

#[test]
fn occupied_text_bounds_skips_string_join() {
    let line = Line::from(vec![Span::raw("  hello  "), Span::raw("world  ")]);
    let (start, end) = occupied_text_bounds(&line).expect("bounds");
    assert_eq!(start, 2);
    assert!(end > start);
}

fn habitat_theme() -> codewhale_ratatui::Theme {
    let caps = crate::tui::color_compat::ColorCompatBackend::new(
        std::io::sink(),
        codewhale_palette::ColorDepth::TrueColor,
        codewhale_palette::PaletteMode::Dark,
    )
    .native_ocean_caps();
    codewhale_ratatui::Theme::new(caps)
        .ground(codewhale_ratatui::Ground::Ocean)
        .tui_palette(codewhale_ratatui::TuiPalette::Underwater)
}

fn ocean_frame(
    area: Rect,
    elapsed_ms: u128,
    presence: f32,
    whale: WhaleCameo,
    activity: AmbientActivity,
) -> (Buffer, AmbientFrameStats) {
    let mut buf = Buffer::empty(area);
    buf.set_style(
        area,
        ratatui::style::Style::default().bg(Color::Rgb(9, 34, 65)),
    );
    let stats = render_ambient_life(
        area,
        &mut buf,
        &habitat_theme(),
        Color::Rgb(106, 174, 242),
        &[],
        elapsed_ms,
        presence,
        whale,
        activity,
        &[],
    );
    (buf, stats)
}

#[test]
fn reduced_motion_builds_and_paints_no_ambient_creatures() {
    let area = Rect::new(0, 0, 80, 24);
    let whale = WhaleCameo {
        elapsed_ms: Some(500),
        anchor_x: 40,
        anchor_y: 16,
    };
    let (still, stats) = ocean_frame(area, 900, 0.0, whale, AmbientActivity::Subagents);
    let mut water = Buffer::empty(area);
    water.set_style(
        area,
        ratatui::style::Style::default().bg(Color::Rgb(9, 34, 65)),
    );
    assert_eq!(still, water);
    assert_eq!(stats, AmbientFrameStats::default());
    let (alive, _) = ocean_frame(
        area,
        900,
        1.0,
        WhaleCameo::default(),
        AmbientActivity::Baseline,
    );
    assert_ne!(alive, water, "full presence paints the kit habitat");
}

#[test]
fn subagent_activity_surfaces_a_whale_pod() {
    let whale = WhaleCameo {
        elapsed_ms: Some(500),
        anchor_x: 50,
        anchor_y: 20,
    };
    let area = Rect::new(0, 0, 100, 30);
    let baseline = ocean_frame(area, 12_000, 1.0, whale, AmbientActivity::Baseline).1;
    let pod = ocean_frame(area, 12_000, 1.0, whale, AmbientActivity::Subagents).1;
    assert!(
        pod.marks_painted > baseline.marks_painted,
        "a subagent pod must surface more whale marks ({}) than the single cameo ({})",
        pod.marks_painted,
        baseline.marks_painted,
    );
}
