use super::*;
use ratatui::style::Modifier;

fn seeded(area: Rect) -> Buffer {
    let mut buffer = Buffer::empty(area);
    for cell in &mut buffer.content {
        cell.set_symbol("~").set_style(
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::Rgb(9, 23, 41))
                .add_modifier(Modifier::ITALIC | Modifier::REVERSED),
        );
    }
    buffer
}

#[test]
fn packed_native_grid_clips_without_reflow_or_changing_transparent_cells() {
    let full_area = Rect::new(3, 2, 18, 6);
    let clipped = Rect::new(13, 2, 6, 6);
    let grid = (0..90)
        .map(|i| if i % 3 == 0 { 0 } else { (i + 1) as u8 })
        .collect::<Vec<_>>();
    let style = Style::default()
        .fg(Color::Rgb(33, 87, 142))
        .add_modifier(Modifier::BOLD);
    let mut full = seeded(Rect::new(0, 0, 25, 10));
    render_grid(full_area, &mut full, &grid, "m · 6", style);
    let mut actual = seeded(clipped);
    render_grid(full_area, &mut actual, &grid, "m · 6", style);
    for y in clipped.y..clipped.bottom() {
        for x in clipped.x..clipped.right() {
            assert_eq!(actual[(x, y)], full[(x, y)], "cell=({x},{y})");
        }
    }
}
