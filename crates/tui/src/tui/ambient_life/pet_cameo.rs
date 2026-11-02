//! Replaces the `≈≈>` cameo with the shared dot-whale widget. The existing
//! ocean completion clock is the only trigger. This is an authored completion
//! gesture, not an event-v1 bucketer or an estimate of hidden agent activity.

use super::pet_sim::{ChannelId, PetSim, PetState};
use super::pet_widget::PetWidget;
use super::{
    AmbientActivity, AmbientFrameStats, WhaleCameo, is_open_water, occupied_text_bounds, ocean,
};
use ratatui::{buffer::Buffer, layout::Rect, style::Color, text::Line, widgets::Widget};
use std::sync::LazyLock;

const WIDTH: u16 = 18;
const HEIGHT: u16 = 6;
pub(super) const DURATION_MS: u128 = 2_400;
const FPS: u128 = 30;
const FRAMES: usize = (DURATION_MS * FPS / 1_000) as usize;
#[cfg(test)]
pub(super) const MAX_MARKS: u32 = 3 * WIDTH as u32 * HEIGHT as u32;

struct CameoFrame {
    buffer: Buffer,
    marks: u32,
}

// A bounded raster cache, not mutable simulation history. Sampling backwards,
// skipping draws, or rendering another session cannot change a frame. Two
// fixed 72-frame tapes; no RNG beyond the core's existing particle stream.
static SOLO: LazyLock<Vec<CameoFrame>> = LazyLock::new(|| frames(ChannelId::Other));
static POD: LazyLock<Vec<CameoFrame>> = LazyLock::new(|| frames(ChannelId::Agent));

fn frames(channel: ChannelId) -> Vec<CameoFrame> {
    let mut sim = PetSim::whale();
    // A completion is observed, but it does not assert a model/tool category.
    // The pod has three persistent slots here; it is not three more swarms.
    let state = PetState {
        channel,
        activity: 0.12,
        coherence: 0.95,
        ..PetState::rest()
    };
    (0..FRAMES)
        .map(|_| {
            sim.step(1.0 / FPS as f64, &state, true, 1.0);
            let mut buffer = Buffer::empty(Rect::new(0, 0, WIDTH, HEIGHT));
            PetWidget {
                sim: &sim,
                state: &state,
            }
            .render(buffer.area, &mut buffer);
            let marks = buffer.content.iter().filter(|c| c.symbol() != " ").count() as u32;
            CameoFrame { buffer, marks }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn paint(
    area: Rect,
    buf: &mut Buffer,
    ink: Color,
    lines: &[Line<'_>],
    presence: f32,
    whale: WhaleCameo,
    activity: AmbientActivity,
    stats: &mut AmbientFrameStats,
) {
    let Some(age) = whale.elapsed_ms.filter(|age| *age < DURATION_MS) else {
        return;
    };
    if presence <= 0.0 {
        return;
    }
    let transcript_has_text = lines
        .iter()
        .any(|line| occupied_text_bounds(line).is_some());
    let pod = activity == AmbientActivity::Subagents;
    let tape = if pod { &*POD } else { &*SOLO };
    let slots: &[i32] = if pod { &[-1, 0, 1] } else { &[0] };
    for (index, slot) in slots.iter().enumerate() {
        let age = age + index as u128 * 240;
        if age >= DURATION_MS {
            continue;
        }
        let frame = &tape[(age * FPS / 1_000) as usize];
        let progress = age as f64 / DURATION_MS as f64;
        let x = i32::from(whale.anchor_x) - i32::from(area.x) - i32::from(WIDTH / 2)
            + slot * i32::from(WIDTH)
            + (progress * 3.0).round() as i32;
        let y = i32::from(whale.anchor_y) - i32::from(area.y) - i32::from(HEIGHT / 2);
        stats.marks_built += frame.marks;
        // Do not clamp off-screen pod members into one overlapping creature.
        if x < 0
            || y < 0
            || x + i32::from(WIDTH) > i32::from(area.width)
            || y + i32::from(HEIGHT) > i32::from(area.height)
        {
            stats.marks_clipped += frame.marks;
            continue;
        }
        let (x, y) = (x as u16, y as u16);
        let marks = || {
            frame
                .buffer
                .content
                .iter()
                .enumerate()
                .filter(|(_, c)| c.symbol() != " ")
        };
        // Atomically withhold body and label, and keep the cameo out of the
        // transcript column entirely while any transcript text is on screen.
        if transcript_has_text
            || marks()
                .any(|(i, _)| !is_open_water(lines, x + i as u16 % WIDTH, y + i as u16 / WIDTH, 1))
        {
            stats.marks_skipped_text += frame.marks;
            continue;
        }
        let glow = (0.65 + 0.35 * (progress * std::f64::consts::PI).sin()) as f32;
        for (i, source) in marks() {
            let point = (area.x + x + i as u16 % WIDTH, area.y + y + i as u16 / WIDTH);
            let Some(cell) = buf.cell_mut(point) else {
                stats.marks_clipped += 1;
                continue;
            };
            // The existing theme owns all habitat ink. No category colour,
            // especially Failure red, is introduced into completion chrome.
            // Ambient ink is already calibrated against the ocean. Applying
            // the standalone canvas alpha again would dim it twice.
            let alpha = (glow * presence).clamp(0.0, 1.0);
            let fg = ocean::mix_colors(cell.bg, ink, alpha);
            cell.set_symbol(source.symbol()).set_fg(fg);
            stats.marks_painted += 1;
            stats.cells_written += 1;
        }
    }
}
