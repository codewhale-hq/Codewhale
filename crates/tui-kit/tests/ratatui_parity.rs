use std::time::Duration;

use codewhale_ratatui::{
    CountBar, InstrumentSurface, KeyHint, MotionMode, Ombre, Paint, Role, Segmented,
    SegmentedState, TranscriptScrollFacts, TranscriptViewport, WaterPalette,
    ocean::OceanRamp,
    testing::{Profile, render, text},
};
use ratatui::{buffer::Buffer, layout::Rect, style::Modifier, text::Line};

#[test]
fn long_action_hints_wrap_without_losing_the_label() {
    let theme = Profile::DarkTrue.theme();
    let surface = InstrumentSurface::new("Actions")
        .actions(vec![KeyHint::new("Enter", "Open selected session")]);
    let area = Rect::new(0, 0, 18, 12);
    let layout = surface.areas(area, &theme);
    assert!(layout.footer.height > 1);
    let buf = render(18, 12, |area, buf| surface.paint(area, buf, &theme));
    let shown = text(&buf).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        shown.replace(' ', "").contains("Openselectedsession"),
        "{shown}"
    );
}

#[test]
fn progress_uses_fractional_cells_only_when_unicode_is_available() {
    let bar = CountBar::new(1, 7);
    let unicode = render(60, 1, |area, buf| {
        bar.paint(area, buf, &Profile::DarkTrue.theme())
    });
    let shown = text(&unicode);
    assert!(shown.chars().any(|c| "▏▎▍▌▋▊▉".contains(c)), "{shown}");
    let ascii = render(60, 1, |area, buf| {
        bar.paint(area, buf, &Profile::Ascii.theme())
    });
    assert!(text(&ascii).is_ascii());
}

#[test]
fn selected_segment_covers_brackets_and_narrow_focus_is_visible() {
    let theme = Profile::DarkTrue.theme();
    let control = Segmented::new(["First", "Second"], SegmentedState::new(0)).focused(true);
    let wide = render(30, 1, |area, buf| control.paint(area, buf, &theme));
    for x in 0..7 {
        assert_eq!(wide[(x, 0)].bg, theme.bg(Role::Selected).bg.unwrap());
    }
    let narrow = render(4, 1, |area, buf| control.paint(area, buf, &theme));
    assert!(narrow[(0, 0)].modifier.contains(Modifier::UNDERLINED));
    assert_eq!(narrow[(0, 0)].fg, theme.fg(Role::Accent).fg.unwrap());
}

#[test]
fn fitting_transcript_has_no_scrollbar_and_final_position_reaches_bottom() {
    let rows = vec![Line::from("row"); 5];
    let theme = Profile::DarkTrue.theme();
    let mut viewport = TranscriptViewport::new(&rows);
    viewport.scrollbar = Some(TranscriptScrollFacts {
        top: 0,
        visible: 5,
        total: 5,
    });
    let fits = render(20, 5, |area, buf| viewport.paint(area, buf, &theme));
    assert!((0..5).all(|y| fits[(19, y)].symbol() == " "));
    viewport.scrollbar = Some(TranscriptScrollFacts {
        top: 0,
        visible: 5,
        total: 6,
    });
    let start = render(20, 5, |area, buf| viewport.paint(area, buf, &theme));
    assert_eq!(start[(19, 0)].symbol(), "┃");
    assert_eq!(start[(19, 4)].symbol(), "│");
    viewport.scrollbar.as_mut().unwrap().top = 1;
    let end = render(20, 5, |area, buf| viewport.paint(area, buf, &theme));
    assert_eq!(end[(19, 0)].symbol(), "│");
    assert_eq!(end[(19, 4)].symbol(), "┃");
}

#[test]
fn ocean_supports_measured_rgb_themes_and_preserves_fallbacks() {
    for profile in [Profile::DarkTrue, Profile::DarkGraphite, Profile::LightTrue] {
        assert!(
            OceanRamp::for_theme(&profile.theme()).is_some(),
            "{profile:?}"
        );
    }
    for profile in [
        Profile::Dark256,
        Profile::Light256,
        Profile::Ansi16,
        Profile::UnknownGround,
        Profile::NoColor,
        Profile::Ascii,
    ] {
        assert!(
            OceanRamp::for_theme(&profile.theme()).is_none(),
            "{profile:?}"
        );
    }
}

#[test]
fn water_drift_keeps_reduced_motion_static_and_repeats_the_caller_clock() {
    let theme = Profile::DarkTrue.theme();
    let area = Rect::new(0, 0, 40, 10);
    let mut source = Buffer::empty(area);
    source.set_style(area, theme.bg(Role::Background));
    let pass = Ombre::new(WaterPalette::Lagoon);
    let mut still = source.clone();
    pass.apply(area, &mut still, &theme);
    for mode in [MotionMode::Reduced, MotionMode::Still] {
        let mut frame = source.clone();
        pass.apply_at(area, &mut frame, &theme, Duration::from_secs(6), mode);
        assert_eq!(frame, still);
    }
    let mut moving = source.clone();
    pass.apply_at(
        area,
        &mut moving,
        &theme,
        Duration::from_secs(6),
        MotionMode::Full,
    );
    assert_ne!(moving, still);
    let mut repeated = source;
    pass.apply_at(
        area,
        &mut repeated,
        &theme,
        Duration::from_secs(30),
        MotionMode::Full,
    );
    assert_eq!(moving, repeated);
    assert_eq!(moving[(0, 0)], still[(0, 0)]);
    assert_eq!(moving[(39, 9)], still[(39, 9)]);
}
