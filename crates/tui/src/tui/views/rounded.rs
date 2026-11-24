use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::views::{ActionHint, action_footer_lines};
use codewhale_palette as palette;

pub(crate) const ASCII_BORDER_SET: symbols::border::Set = symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

pub(crate) fn resolved_surface_bg(theme: &palette::UiTheme) -> Color {
    if theme.composer_bg != Color::Reset {
        theme.composer_bg
    } else {
        palette::COMPOSER_BG
    }
}

pub(crate) fn rounded_block_with_colors(
    border_color: Color,
    text_color: Color,
    bg: Color,
) -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color).bg(bg))
        .style(Style::default().fg(text_color).bg(bg));
    if crate::tui::color_compat::ascii_safe_enabled() {
        block.border_set(ASCII_BORDER_SET)
    } else {
        block.border_type(BorderType::Rounded)
    }
}

pub(crate) fn rounded_titled_block(
    title: &str,
    right_meta: Option<&str>,
    active: bool,
    bg: Color,
    max_width: u16,
) -> Block<'static> {
    let border_color = if active {
        palette::WHALE_ACTION
    } else {
        palette::BORDER_COLOR
    };
    rounded_titled_block_with_colors(
        title,
        right_meta,
        border_color,
        palette::WHALE_ACTION,
        palette::TEXT_PRIMARY,
        palette::TEXT_MUTED,
        bg,
        max_width,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn rounded_titled_block_with_colors(
    title: &str,
    right_meta: Option<&str>,
    border_color: Color,
    title_color: Color,
    text_color: Color,
    meta_color: Color,
    bg: Color,
    max_width: u16,
) -> Block<'static> {
    let ascii = crate::tui::color_compat::ascii_safe_enabled();
    let rule = if ascii { "-" } else { "─" };
    let border_style = Style::default().fg(border_color).bg(bg);
    let title_style = Style::default()
        .fg(title_color)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let meta_style = Style::default().fg(meta_color).bg(bg);
    let title_budget = usize::from(max_width.saturating_sub(6));
    let clipped_title = codewhale_tui_kit::ui_text::semantic_truncate(title, title_budget);
    let mut block =
        rounded_block_with_colors(border_color, text_color, bg).title_top(Line::from(vec![
            Span::styled(rule, border_style),
            Span::styled(format!(" {clipped_title} "), title_style),
        ]));
    if let Some(meta) = right_meta
        && !meta.is_empty()
        && clipped_title.width() + meta.width() + 9 <= usize::from(max_width)
    {
        block = block.title_top(
            Line::from(vec![
                Span::styled(format!(" {meta} "), meta_style),
                Span::styled(rule, border_style),
            ])
            .right_aligned(),
        );
    }
    block
}

pub(crate) fn render_rounded_surface(
    area: Rect,
    buf: &mut Buffer,
    title: impl Into<String>,
    right_meta: Option<String>,
    active: bool,
) -> Rect {
    render_rounded_surface_themed(area, buf, title, right_meta, active, None)
}

pub(crate) fn render_rounded_surface_themed(
    area: Rect,
    buf: &mut Buffer,
    title: impl Into<String>,
    right_meta: Option<String>,
    active: bool,
    theme: Option<&palette::UiTheme>,
) -> Rect {
    let backdrop_bg = theme
        .map(|t| t.surface_bg)
        .filter(|c| *c != Color::Reset)
        .unwrap_or(palette::WHALE_BG);
    let bg = theme
        .map(resolved_surface_bg)
        .unwrap_or(palette::COMPOSER_BG);
    let text_fg = theme
        .map(|t| t.text_body)
        .filter(|c| *c != Color::Reset)
        .unwrap_or(palette::TEXT_PRIMARY);
    let accent = theme
        .map(|t| t.accent_primary)
        .filter(|c| *c != Color::Reset)
        .unwrap_or(palette::WHALE_ACTION);
    let quiet_border = theme
        .map(|t| t.border)
        .filter(|c| *c != Color::Reset)
        .unwrap_or(palette::BORDER_COLOR);
    let muted = theme
        .map(|t| t.text_muted)
        .filter(|c| *c != Color::Reset)
        .unwrap_or(palette::TEXT_MUTED);
    let border_color = if active { accent } else { quiet_border };

    let margin_x = u16::from(area.width >= 96);
    let margin_y = u16::from(area.height >= 28);
    let surface = Rect {
        x: area.x.saturating_add(margin_x),
        y: area.y.saturating_add(margin_y),
        width: area.width.saturating_sub(margin_x.saturating_mul(2)),
        height: area.height.saturating_sub(margin_y.saturating_mul(2)),
    };

    Clear.render(area, buf);
    Block::default()
        .style(Style::default().fg(text_fg).bg(backdrop_bg))
        .render(area, buf);

    let title = title.into();
    let pad_x = u16::from(surface.width >= 24);
    let block = rounded_titled_block_with_colors(
        &title,
        right_meta.as_deref(),
        border_color,
        accent,
        text_fg,
        muted,
        bg,
        surface.width,
    )
    .padding(Padding::new(pad_x, pad_x, 0, 0));
    let inner = block.inner(surface);
    block.render(surface, buf);
    inner
}

pub(crate) fn render_rounded_modal(
    area: Rect,
    popup_area: Rect,
    buf: &mut Buffer,
    title: impl Into<String>,
    right_meta: Option<String>,
    active: bool,
) -> Rect {
    let shadow_x = popup_area.x.saturating_add(1);
    let shadow_y = popup_area.y.saturating_add(1);
    let shadow_right = area.x.saturating_add(area.width);
    let shadow_bottom = area.y.saturating_add(area.height);
    let shadow_width = popup_area.width.min(shadow_right.saturating_sub(shadow_x));
    let shadow_height = popup_area
        .height
        .min(shadow_bottom.saturating_sub(shadow_y));

    if shadow_width > 0 && shadow_height > 0 {
        Block::default()
            .style(Style::default().bg(palette::SURFACE_ELEVATED))
            .render(
                Rect {
                    x: shadow_x,
                    y: shadow_y,
                    width: shadow_width,
                    height: shadow_height,
                },
                buf,
            );
    }

    Clear.render(popup_area, buf);
    let title = title.into();
    let pad_x = u16::from(popup_area.width >= 20);
    let block = rounded_titled_block(
        &title,
        right_meta.as_deref(),
        active,
        palette::COMPOSER_BG,
        popup_area.width,
    )
    .padding(Padding::new(pad_x, pad_x, 0, 0));
    let inner = block.inner(popup_area);
    block.render(popup_area, buf);
    inner
}

pub(crate) fn render_hairline(area: Rect, buf: &mut Buffer, border_color: Color, bg: Color) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let ch = if crate::tui::color_compat::ascii_safe_enabled() {
        "-"
    } else {
        "─"
    };
    Paragraph::new(Line::from(Span::styled(
        ch.repeat(usize::from(area.width)),
        Style::default().fg(border_color).bg(bg),
    )))
    .render(Rect::new(area.x, area.y, area.width, 1), buf);
}

pub(crate) fn render_rounded_footer(inner: Rect, buf: &mut Buffer, hints: &[ActionHint]) -> Rect {
    render_rounded_footer_themed(
        inner,
        buf,
        hints,
        palette::COMPOSER_BG,
        palette::BORDER_COLOR,
    )
}

pub(crate) fn render_rounded_footer_themed(
    inner: Rect,
    buf: &mut Buffer,
    hints: &[ActionHint],
    bg: Color,
    border_color: Color,
) -> Rect {
    let lines = action_footer_lines(hints, inner.width);
    if lines.is_empty() || inner.height == 0 {
        return inner;
    }
    let footer_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(inner.height);
    let show_divider = inner.height >= footer_height.saturating_add(5);
    let divider_height = u16::from(show_divider);
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - footer_height,
        width: inner.width,
        height: footer_height,
    };
    Paragraph::new(lines)
        .style(Style::default().bg(bg))
        .render(footer_area, buf);
    if show_divider {
        render_hairline(
            Rect::new(inner.x, footer_area.y.saturating_sub(1), inner.width, 1),
            buf,
            border_color,
            bg,
        );
    }
    Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner
            .height
            .saturating_sub(footer_height.saturating_add(divider_height)),
    }
}

pub(crate) fn pad_selected_line(
    mut line: Line<'static>,
    width: usize,
    style: Style,
) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if used < width {
        line.spans
            .push(Span::styled(" ".repeat(width - used), style));
    }
    line
}
