// Frozen mounted composer source fragments; private test counterpart only.
use super::*;

const COMPOSER_PROMPT_GUTTER_WIDTH: u16 = 2;
const COMPOSER_PANEL_MIN_WIDTH: u16 = 12;

/// Whether the active composer should use its full rounded enclosure.
///
/// `composer_border` is a legacy configuration name, but its compatibility
/// policy is deliberate: the default `true` means the Tideline enclosure;
/// `false` is an explicit compact/quiet opt-out. Keep every layout consumer
/// behind this helper so the reserved floor, measured height, and rendered
/// geometry cannot drift apart.
#[must_use]
pub(crate) fn composer_enclosure_enabled(app: &App) -> bool {
    app.composer_border
}

/// Shared `[↵]` submit rect for the live composer, or `None` when the
/// enclosure cannot host the three-cell affordance.
///
/// The gate is the same `enclosed_composer_panel_fits` predicate the painter
/// uses: a hitbox without the painted panel would be an invisible click
/// target (widths 6–11 rendered a borderless rule while still accepting
/// clicks).
#[must_use]
pub(crate) fn active_composer_submit_rect(app: &App, area: Rect) -> Option<Rect> {
    if !enclosed_composer_panel_fits(composer_enclosure_enabled(app), area.width, area.height) {
        return None;
    }
    Some(crate::tui::composer_chrome::tideline_composer_geometry(area).submit)
}

/// Restore rounded corners after the title-bearing top/bottom passes.
///
/// Ratatui renders a `TOP`-only (or `BOTTOM`-only) block through the corner
/// cells as horizontal line glyphs. The live composer needs those passes for
/// its localized titles and shared focus outline, so put
/// the four rounded joins back afterward rather than replacing its mature
/// input widget with the unfinished translation scaffold.
fn render_composer_panel_corners(
    area: Rect,
    buf: &mut Buffer,
    background: Style,
    permission_color: Color,
    mode_color: Color,
) {
    let top_style = background.fg(permission_color);
    let bottom_style = background.fg(mode_color);
    let left = area.left();
    let right = area.right().saturating_sub(1);
    let top = area.top();
    let bottom = area.bottom().saturating_sub(1);

    buf[(left, top)].set_symbol("╭").set_style(top_style);
    buf[(right, top)].set_symbol("╮").set_style(top_style);
    buf[(left, bottom)].set_symbol("╰").set_style(bottom_style);
    buf[(right, bottom)].set_symbol("╯").set_style(bottom_style);
}

/// Whether the outer composer rect can carry both semantic border rows.
///
/// Keep this policy in outer-area coordinates. Input wrapping subtracts the
/// prompt gutter later; using that narrower text width here made 12- and
/// 13-column composers render as panels after reserving only the quiet rule.
fn enclosed_composer_panel_fits(show_panel: bool, area_width: u16, area_height: u16) -> bool {
    show_panel && area_height >= 3 && area_width >= COMPOSER_PANEL_MIN_WIDTH
}

/// Border-aware input plane for the active composer.
///
/// The shared shell's `[↵]` control occupies three cells on the inner row.
/// Keep the text plane to its left, with one blank cell in between, so input
/// wrapping, cursor placement, and pointer mapping cannot claim painted send
/// cells. The outer block still owns the trailing breathing cell before its
/// right rail.
fn composer_inner_area(area: Rect, has_panel: bool) -> Rect {
    let inner = if has_panel {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .inner(area)
    } else if area.height >= 2 {
        Block::default().borders(Borders::TOP).inner(area)
    } else {
        area
    };
    if !has_panel {
        return inner;
    }

    let shell = crate::tui::composer_chrome::tideline_composer_geometry(area);
    Rect {
        width: shell.content.right().saturating_sub(inner.x),
        ..inner
    }
}

/// Canonical horizontal geometry for composer input text.
///
/// The prompt glyph occupies the first gutter column and the second column is
/// breathing room. Every consumer that wraps or maps input must use
/// `text_area`: rendering and cursor placement, viewport scroll bookkeeping,
/// and mouse hit-to-character conversion. Keeping the inset here prevents the
/// first typed character and exact wrap boundaries from using different
/// effective widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ComposerContentGeometry {
    pub(crate) text_area: Rect,
    pub(crate) prompt_inset: u16,
}

impl ComposerContentGeometry {
    #[must_use]
    pub(crate) fn text_width(self) -> usize {
        usize::from(self.text_area.width.max(1))
    }

    #[must_use]
    fn prompt_padding(self) -> &'static str {
        if self.prompt_inset == COMPOSER_PROMPT_GUTTER_WIDTH {
            "  "
        } else {
            ""
        }
    }

    #[must_use]
    fn prompt_x(self) -> Option<u16> {
        (self.prompt_inset > 0).then(|| self.text_area.x.saturating_sub(self.prompt_inset))
    }
}

#[must_use]
pub(crate) fn composer_content_geometry(
    inner_area: Rect,
    history_search_active: bool,
) -> ComposerContentGeometry {
    let prompt_inset = if !history_search_active
        && inner_area.width >= COMPOSER_PROMPT_GUTTER_WIDTH.saturating_add(1)
    {
        COMPOSER_PROMPT_GUTTER_WIDTH
    } else {
        0
    };
    ComposerContentGeometry {
        text_area: Rect {
            x: inner_area.x.saturating_add(prompt_inset),
            y: inner_area.y,
            width: inner_area.width.saturating_sub(prompt_inset),
            height: inner_area.height,
        },
        prompt_inset,
    }
}

pub struct ComposerWidget<'a> {
    app: &'a App,
    max_height: u16,
    slash_menu_entries: &'a [SlashMenuEntry],
    mention_menu_entries: &'a [String],
}

impl<'a> ComposerWidget<'a> {
    pub fn new(
        app: &'a App,
        max_height: u16,
        slash_menu_entries: &'a [SlashMenuEntry],
        mention_menu_entries: &'a [String],
    ) -> Self {
        Self {
            app,
            max_height,
            slash_menu_entries,
            mention_menu_entries,
        }
    }

    /// Number of popup rows below the input. Mention and slash menus are
    /// mutually exclusive — the cursor can only sit inside an `@token` OR
    /// a `/cmd` token, not both at once. Mention takes precedence because
    /// the partial-mention check is positional and stricter than slash's
    /// "starts-with-/" check.
    fn active_menu_row_count(&self) -> usize {
        if self.app.is_history_search_active() {
            self.app.history_search_matches().len().max(1)
        } else if !self.mention_menu_entries.is_empty() {
            self.mention_menu_entries.len()
        } else {
            self.slash_menu_entries.len()
        }
    }

    /// Row reservation passed to `composer_height`. When the slash- or
    /// mention-menu is active we lock the composer to its worst-case
    /// envelope so the chat area above doesn't repaint every keystroke
    /// as the matched-entry count shrinks. Pure cosmetic: the menu
    /// itself still renders its actual entries — the extra rows are
    /// just panel padding inside the same Rect.
    ///
    /// Reported on Windows 10 PowerShell + WSL where the console
    /// backend's per-cell write cost makes the layout jitter visible
    /// even though the work is tiny on Unix terminals. See user
    /// feedback in v0.8.8 polish thread.
    pub fn active_menu_reserved_rows(&self) -> usize {
        let actual = self.active_menu_row_count();
        if actual == 0 {
            return 0;
        }
        if self.app.is_history_search_active() {
            return actual;
        }
        // Slash- and mention-menu are the cases that grow/shrink mid-typing.
        // Reserve the composer's panel-max so the layout stays stable
        // for the lifetime of the menu session.
        actual.max(usize::from(self.max_height_cap()))
    }

    fn wants_enclosed_panel(&self) -> bool {
        composer_enclosure_enabled(self.app)
    }

    pub(crate) fn has_panel(&self, area: Rect) -> bool {
        enclosed_composer_panel_fits(self.wants_enclosed_panel(), area.width, area.height)
    }

    /// The border- and submit-aware input rectangle shared by rendering,
    /// cursor mapping, and the frame's persistent mouse geometry.
    pub(crate) fn inner_area(&self, area: Rect) -> Rect {
        composer_inner_area(area, self.has_panel(area))
    }

    fn focus_color(&self) -> Color {
        use crate::tui::shell_key_routing::Focus;
        let editing = match self.app.focus() {
            Focus::Composer => true,
            Focus::Launch => self.app.launch.menu_selected.is_none(),
            _ => false,
        };
        if editing {
            self.app.ui_theme.accent_primary
        } else {
            self.app.ui_theme.border
        }
    }

    fn max_height_cap(&self) -> u16 {
        composer_max_height(self.app.composer_density)
    }
}

impl Renderable for ComposerWidget<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        // Slash rows are re-recorded below; clear first so a closed or
        // resized menu cannot keep stale hitboxes from the prior frame.
        self.app
            .viewport
            .last_slash_menu_hitboxes
            .borrow_mut()
            .clear();
        let background = Style::default().bg(self.app.ui_theme.composer_bg);
        let has_panel = self.has_panel(area);
        let inner_area = self.inner_area(area);
        let input_text = self.app.composer_display_input();
        let input_cursor = self.app.composer_display_cursor();
        let history_search_matches = if self.app.is_history_search_active() {
            self.app.history_search_matches()
        } else {
            Vec::new()
        };
        let menu_lines = self.active_menu_row_count();
        // For the layout-budget calculation, treat the menu as if it were
        // already at its locked, worst-case height (see
        // `active_menu_reserved_rows`). Without this, when the matched-entry
        // count drops mid-typing, `top_padding` grows and the input visually
        // jumps down inside the panel even though the panel rect stayed put.
        let menu_lines_for_budget = self.active_menu_reserved_rows().max(menu_lines);
        let input_rows_budget =
            composer_input_rows_budget(inner_area.height, menu_lines_for_budget);
        // Menu rows span the full inner panel. Input text alone uses the
        // prompt-adjusted geometry below.
        let content_width = usize::from(inner_area.width.max(1));
        let content_geometry =
            composer_content_geometry(inner_area, self.app.is_history_search_active());
        let input_content_width = content_geometry.text_width();

        // Use the extended version that also returns character indices to avoid
        // redundant wrapping when rendering text selections (issue #3909).
        let (visible_lines, _cursor_row, _cursor_col, _scroll_offset, visible_char_indices) =
            layout_input_with_scroll_and_char_indices(
                input_text,
                input_cursor,
                input_content_width,
                input_rows_budget,
            );
        let clipped =
            wrap_input_lines_for_mouse(input_text, input_content_width).len() > input_rows_budget;
        if has_panel {
            let hint_line = if self.app.is_history_search_active() {
                Some(Line::from(vec![
                    Span::styled(
                        format!(
                            " {}  ",
                            self.app
                                .tr(codewhale_localization::MessageId::HistoryHintMove)
                        ),
                        Style::default().fg(palette::TEXT_MUTED),
                    ),
                    Span::styled(
                        format!(
                            "{}  ",
                            self.app
                                .tr(codewhale_localization::MessageId::HistoryHintAccept)
                        ),
                        Style::default().fg(palette::TEXT_MUTED),
                    ),
                    Span::styled(
                        self.app
                            .tr(codewhale_localization::MessageId::HistoryHintRestore),
                        Style::default().fg(palette::TEXT_MUTED),
                    ),
                ]))
            } else if !self.slash_menu_entries.is_empty() {
                Some(Line::from(Span::styled(
                    self.app
                        .tr(codewhale_localization::MessageId::ComposerSlashMenuHint),
                    Style::default().fg(self.app.ui_theme.text_hint),
                )))
            } else if !input_text.trim().is_empty() {
                composer_submit_hint(
                    self.app,
                    clipped
                        && !self.app.is_history_search_active()
                        && self.slash_menu_entries.is_empty(),
                )
                .map(|hint| {
                    Line::from(vec![Span::styled(
                        format!(" {} ", hint.text),
                        Style::default().fg(hint.color),
                    )])
                })
            } else {
                None
            };

            // Focus has one outline. Permission and mode remain explicit in
            // their footer; repeating both around the input competes with it.
            let focus_color = self.focus_color();
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(focus_color))
                .style(background)
                .render(area, buf);
            let mut top_border = Block::default()
                .borders(Borders::TOP)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(focus_color))
                .style(background);
            if self.app.is_history_search_active() {
                top_border = top_border.title(Line::from(Span::styled(
                    format!(
                        " {} ",
                        self.app
                            .tr(codewhale_localization::MessageId::HistorySearchTitle)
                    ),
                    Style::default().fg(palette::TEXT_MUTED),
                )));
            }
            // Agent focus chip: the composer names the fork it addresses so
            // a message never goes to a worker by surprise.
            if let Some(chip) = crate::tui::agent_focus::composer_chip_text(self.app) {
                top_border = top_border.title_top(
                    Line::from(Span::styled(
                        format!(" {chip} "),
                        Style::default()
                            .fg(self.app.ui_theme.accent_action)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .right_aligned(),
                );
            }
            top_border.render(area, buf);

            let mut bottom_border = Block::default()
                .borders(Borders::BOTTOM)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(focus_color))
                .style(background);
            if let Some(hint_line) = hint_line {
                bottom_border = bottom_border.title_bottom(hint_line);
            }
            bottom_border.render(area, buf);
            render_composer_panel_corners(area, buf, background, focus_color, focus_color);
        } else if area.height >= 2 {
            let mut block = Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(self.app.ui_theme.border))
                .style(background);
            if !input_text.trim().is_empty()
                && let Some(hint) = composer_submit_hint(
                    self.app,
                    clipped
                        && !self.app.is_history_search_active()
                        && self.slash_menu_entries.is_empty(),
                )
            {
                block = block.title(Line::from(Span::styled(
                    format!(" {} ", hint.text),
                    Style::default().fg(hint.color),
                )));
            }
            if let Some(chip) = crate::tui::agent_focus::composer_chip_text(self.app) {
                block = block.title_top(
                    Line::from(Span::styled(
                        format!(" {chip} "),
                        Style::default()
                            .fg(self.app.ui_theme.accent_action)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .right_aligned(),
                );
            }
            block.render(area, buf);
        } else {
            Block::default().style(background).render(area, buf);
        }

        let mut input_lines = Vec::new();
        if input_text.is_empty() {
            let (placeholder, style): (Cow<'_, str>, Style) = if let Some(ref suggestion) =
                self.app.prompt_suggestion
                && !self.app.is_history_search_active()
            {
                (
                    Cow::Borrowed(suggestion.as_str()),
                    Style::default().fg(palette::TEXT_HINT),
                )
            } else {
                (
                    composer_empty_hint_text(self.app),
                    Style::default().fg(self.app.ui_theme.text_soft),
                )
            };
            input_lines.push(Line::from(vec![
                Span::raw(content_geometry.prompt_padding()),
                Span::styled(placeholder, style),
            ]));
        } else if let Some((sel_start, sel_end)) = self.app.selection_range() {
            // Use the character indices we already computed during layout
            // to avoid redundant wrapping (issue #3909).
            let line_ranges: Vec<(usize, usize)> = visible_char_indices
                .iter()
                .map(|(start, text)| (*start, *start + text.chars().count()))
                .collect();
            for (line_text, (line_start, line_end)) in visible_lines.iter().zip(line_ranges.iter())
            {
                let mut spans = line_spans_with_selection(
                    line_text,
                    *line_start,
                    *line_end,
                    sel_start,
                    sel_end,
                    self.app.ui_theme.selection_bg,
                );
                if content_geometry.prompt_inset > 0 {
                    spans.insert(0, Span::raw(content_geometry.prompt_padding()));
                }
                input_lines.push(Line::from(spans));
            }
        } else {
            for line in &visible_lines {
                let mut spans = Vec::new();
                if content_geometry.prompt_inset > 0 {
                    spans.push(Span::raw(content_geometry.prompt_padding()));
                }
                spans.push(Span::styled(
                    line.clone(),
                    Style::default().fg(palette::TEXT_PRIMARY),
                ));
                input_lines.push(Line::from(spans));
            }
        }

        // For non-empty input, input_lines.len() already reflects wrapping via
        // layout_input. For empty input, keep the first row reserved for the
        // real terminal cursor so IME preedit text has a clean surface.
        let visual_rows = if input_text.is_empty() {
            let hint: Option<Cow<'_, str>> = if let Some(ref suggestion) =
                self.app.prompt_suggestion
                && !self.app.is_history_search_active()
            {
                Some(Cow::Borrowed(suggestion.as_str()))
            } else {
                Some(composer_empty_hint_text(self.app))
            };
            empty_composer_visual_rows(hint.as_deref(), input_content_width, input_rows_budget)
        } else {
            input_lines.len()
        };
        let top_padding = composer_top_padding(visual_rows, input_rows_budget);
        let mut lines = Vec::new();
        for _ in 0..top_padding {
            lines.push(Line::from(""));
        }
        lines.extend(input_lines);

        if self.app.is_history_search_active() {
            if history_search_matches.is_empty() {
                lines.push(Line::from(Span::styled(
                    self.app
                        .tr(codewhale_localization::MessageId::HistoryNoMatches),
                    Style::default().fg(palette::TEXT_MUTED),
                )));
            } else {
                let selected = self
                    .app
                    .history_search_selected_index()
                    .min(history_search_matches.len().saturating_sub(1));
                let menu_visible_rows = inner_area
                    .height
                    .saturating_sub(visual_rows as u16)
                    .saturating_sub(top_padding as u16)
                    .saturating_sub(1)
                    .max(1) as usize;
                let menu_total = history_search_matches.len();
                let menu_top = if menu_total <= menu_visible_rows {
                    0
                } else {
                    let half = menu_visible_rows / 2;
                    if selected <= half {
                        0
                    } else if selected + half >= menu_total {
                        menu_total.saturating_sub(menu_visible_rows)
                    } else {
                        selected.saturating_sub(half)
                    }
                };
                let menu_bottom = (menu_top + menu_visible_rows).min(menu_total);

                for (idx, entry) in history_search_matches
                    .iter()
                    .enumerate()
                    .take(menu_bottom)
                    .skip(menu_top)
                {
                    let is_selected = idx == selected;
                    let style = if is_selected {
                        menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
                    } else {
                        Style::default().fg(palette::TEXT_MUTED)
                    };
                    let marker = codewhale_tui_kit::glyphs::selection_marker(is_selected);
                    lines.push(Line::from(vec![
                        Span::styled(" ", Style::default()),
                        Span::styled(marker, style),
                        Span::styled(" ", style),
                        Span::styled(entry.clone(), style),
                    ]));
                }
            }
        } else if !self.mention_menu_entries.is_empty() {
            let selected = self
                .app
                .mention_menu_selected
                .min(self.mention_menu_entries.len().saturating_sub(1));
            let menu_visible_rows = inner_area
                .height
                .saturating_sub(visual_rows as u16)
                .saturating_sub(top_padding as u16)
                .saturating_sub(1)
                .max(1) as usize;
            let menu_total = self.mention_menu_entries.len();
            let menu_top = if menu_total <= menu_visible_rows {
                0
            } else {
                let half = menu_visible_rows / 2;
                if selected <= half {
                    0
                } else if selected + half >= menu_total {
                    menu_total.saturating_sub(menu_visible_rows)
                } else {
                    selected.saturating_sub(half)
                }
            };
            let menu_bottom = (menu_top + menu_visible_rows).min(menu_total);

            for (idx, entry) in self
                .mention_menu_entries
                .iter()
                .enumerate()
                .take(menu_bottom)
                .skip(menu_top)
            {
                let is_selected = idx == selected;
                let style = if is_selected {
                    menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
                } else {
                    Style::default().fg(palette::TEXT_MUTED)
                };
                let marker = codewhale_tui_kit::glyphs::selection_marker(is_selected);
                lines.push(Line::from(vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(marker, style),
                    Span::styled(" ", style),
                    Span::styled(format!("@{entry}"), style),
                ]));
            }
        } else if !self.slash_menu_entries.is_empty() {
            let selected = self
                .app
                .slash_menu_selected
                .min(self.slash_menu_entries.len().saturating_sub(1));
            let menu_visible_rows = inner_area
                .height
                .saturating_sub(visual_rows as u16)
                .saturating_sub(top_padding as u16)
                .saturating_sub(1)
                .max(1) as usize;
            let menu_total = self.slash_menu_entries.len();
            let menu_top = if menu_total <= menu_visible_rows {
                0
            } else {
                let half = menu_visible_rows / 2;
                if selected <= half {
                    0
                } else if selected + half >= menu_total {
                    menu_total.saturating_sub(menu_visible_rows)
                } else {
                    selected.saturating_sub(half)
                }
            };
            let menu_bottom = (menu_top + menu_visible_rows).min(menu_total);

            // Label column width — grows to fit the widest visible name
            // (including alias hint like " or /bangzhu") but stays bounded.
            let label_width = self
                .slash_menu_entries
                .iter()
                .take(menu_bottom)
                .skip(menu_top)
                .map(|e| {
                    if let Some(ref hint) = e.alias_hint {
                        format!("{} or /{}", e.name, hint).width()
                    } else {
                        e.name.width()
                    }
                })
                .max()
                .unwrap_or(22)
                .min(content_width.saturating_sub(4))
                .max(8);
            for (idx, entry) in self
                .slash_menu_entries
                .iter()
                .enumerate()
                .take(menu_bottom)
                .skip(menu_top)
            {
                let is_selected = idx == selected;
                let sel_style = if is_selected {
                    menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
                } else {
                    Style::default().fg(palette::TEXT_MUTED)
                };
                let marker = codewhale_tui_kit::glyphs::selection_marker(is_selected);

                // Name column
                let name_style = if entry.is_skill && !is_selected {
                    Style::default().fg(palette::WHALE_ACTION)
                } else {
                    sel_style
                };

                // Description column (muted when not selected, secondary when selected)
                let desc_style = if is_selected {
                    menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
                } else {
                    Style::default().fg(palette::TEXT_DIM)
                };

                // Build display name: canonical name, with "or /alias" hint
                // when the user typed via a pinyin alias.
                let display_name = if let Some(ref hint) = entry.alias_hint {
                    format!("{} or /{}", entry.name, hint)
                } else {
                    entry.name.clone()
                };

                let name_was_truncated = display_name.width() > label_width;
                let mut name_display =
                    codewhale_tui_kit::ui_text::truncate_line_to_width(&display_name, label_width);
                while name_display.width() < label_width {
                    name_display.push(' ');
                }

                // Skill marker prefix
                let skill_prefix = if entry.is_skill { "✦" } else { " " };

                // Compute exact prefix display width to avoid Paragraph wrap:
                // 1(" ") + 1(marker) + skill_prefix.width() + label_width + 2("  ")
                let prefix_display_width = 1 + 1 + skill_prefix.width() + label_width + 2;
                let desc_capacity = content_width.saturating_sub(prefix_display_width);
                let description_was_truncated = entry.description.width() > desc_capacity;
                let desc_display = codewhale_tui_kit::ui_text::truncate_line_to_width(
                    &entry.description,
                    desc_capacity,
                );

                let row_line_index = lines.len();
                lines.push(Line::from(vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(marker, sel_style),
                    Span::styled(skill_prefix, name_style),
                    Span::styled(name_display, name_style),
                    Span::styled("  ", desc_style),
                    Span::styled(desc_display, desc_style),
                ]));

                let row_y = inner_area
                    .y
                    .saturating_add(u16::try_from(row_line_index).unwrap_or(u16::MAX));
                if row_y < inner_area.bottom() && inner_area.width > 0 {
                    self.app
                        .viewport
                        .last_slash_menu_hitboxes
                        .borrow_mut()
                        .push((idx, Rect::new(inner_area.x, row_y, inner_area.width, 1)));
                }

                if name_was_truncated || description_was_truncated {
                    let full_text = if entry.description.trim().is_empty() {
                        display_name
                    } else {
                        format!("{display_name}  {}", entry.description)
                    };
                    if row_y < inner_area.bottom() {
                        crate::tui::hover_layer::register_rect(
                            crate::tui::hover_hit::HoverTargetKind::TruncatedText,
                            Rect::new(inner_area.x, row_y, inner_area.width, 1),
                            full_text,
                            false,
                        );
                    }
                }
            }
        }

        let paragraph = Paragraph::new(lines)
            .style(background)
            .wrap(Wrap { trim: false });
        paragraph.render(inner_area, buf);

        // The prompt is a persistent focus anchor, not empty-state chrome.
        // Rendering it on every input row keeps the first character from
        // causing a visible leftward jump.
        if let Some(prompt_x) = content_geometry.prompt_x()
            && let Some((cursor_x, cursor_y)) = self.cursor_pos(area)
        {
            debug_assert!(cursor_x >= content_geometry.text_area.x);
            buf[(prompt_x, cursor_y)]
                .set_symbol("❯")
                .set_style(Style::default().fg(self.app.ui_theme.accent_primary));
        }

        // Restore the shared `[↵]` after caller-owned input so a long draft
        // cannot erase the one cell target the mouse handler also uses.
        if has_panel {
            crate::tui::composer_chrome::render_tideline_composer_submit(
                area,
                buf,
                &self.app.ui_theme,
                // Display state, not key-routing state: the paste-burst
                // window reopens on every fast keystroke, so drawing from
                // `composer_enter_would_submit` strobed the chip while
                // typing (#6397).
                self.app.composer_draft_is_submittable(),
                crate::tui::color_compat::ascii_safe_enabled(),
            );
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        composer_height(
            self.app.composer_display_input(),
            width,
            self.max_height,
            self.active_menu_reserved_rows(),
            self.app.composer_density,
            self.wants_enclosed_panel(),
        )
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let inner_area = self.inner_area(area);
        let input_text = self.app.composer_display_input();
        let input_cursor = self.app.composer_display_cursor();
        let content_geometry =
            composer_content_geometry(inner_area, self.app.is_history_search_active());
        let input_content_width = content_geometry.text_width();
        // Match the render path's locked-budget calculation so the cursor
        // lands on the same row the input is drawn on.
        let input_rows_budget =
            composer_input_rows_budget(inner_area.height, self.active_menu_reserved_rows());

        let (visible_lines, cursor_row, cursor_col) = layout_input(
            input_text,
            input_cursor,
            input_content_width,
            input_rows_budget,
        );
        let visual_rows = if input_text.is_empty() {
            let hint: Option<Cow<'_, str>> = if let Some(ref suggestion) =
                self.app.prompt_suggestion
                && !self.app.is_history_search_active()
            {
                Some(Cow::Borrowed(suggestion.as_str()))
            } else {
                Some(composer_empty_hint_text(self.app))
            };
            empty_composer_visual_rows(hint.as_deref(), input_content_width, input_rows_budget)
        } else {
            visible_lines.len()
        };
        let top_padding = composer_top_padding(visual_rows, input_rows_budget);

        let cursor_x = content_geometry
            .text_area
            .x
            .saturating_add(u16::try_from(cursor_col).unwrap_or(u16::MAX));
        let cursor_y = inner_area
            .y
            .saturating_add(u16::try_from(top_padding + cursor_row).unwrap_or(u16::MAX));
        if cursor_x < area.x + area.width && cursor_y < area.y + area.height {
            Some((cursor_x, cursor_y))
        } else {
            None
        }
    }
}

pub fn composer_input_rows_budget(inner_height: u16, extra_lines: usize) -> usize {
    usize::from(inner_height).saturating_sub(extra_lines).max(1)
}

fn composer_top_padding(content_lines: usize, rows_budget: usize) -> usize {
    crate::tui::composer_chrome::top_padding(content_lines, rows_budget)
}

pub(crate) fn empty_composer_visual_rows(
    _hint: Option<&str>,
    _content_width: usize,
    _rows_budget: usize,
) -> usize {
    1
}

fn composer_max_height(density: ComposerDensity) -> u16 {
    crate::tui::composer_chrome::ComposerChrome::for_density(density, false).max_total_rows
}

fn composer_height(
    input: &str,
    area_width: u16,
    available_height: u16,
    extra_lines: usize,
    density: ComposerDensity,
    show_panel: bool,
) -> u16 {
    let has_panel = enclosed_composer_panel_fits(show_panel, area_width, available_height);
    // Measure through the same border- and submit-aware plane that rendering,
    // cursor placement, the frame viewport, and mouse mapping use. A draft
    // that wraps here therefore cannot consume the painted `[↵]` cells later.
    let measurement_area = Rect::new(0, 0, area_width, if has_panel { 3 } else { 1 });
    let content_width =
        composer_content_geometry(composer_inner_area(measurement_area, has_panel), false)
            .text_width();
    let mut line_count = wrap_input_lines(input, content_width).len();
    if line_count == 0 {
        line_count = 1;
    }
    crate::tui::composer_chrome::desired_height(
        line_count,
        extra_lines,
        available_height,
        density,
        has_panel,
    )
}

fn layout_input(
    input: &str,
    cursor: usize,
    width: usize,
    max_height: usize,
) -> (Vec<String>, usize, usize) {
    let (visible, visible_cursor_row, visible_cursor_col, _) =
        layout_input_with_scroll(input, cursor, width, max_height);
    (visible, visible_cursor_row, visible_cursor_col)
}

pub fn layout_input_with_scroll(
    input: &str,
    cursor: usize,
    width: usize,
    max_height: usize,
) -> (Vec<String>, usize, usize, usize) {
    let mut lines = wrap_input_lines(input, width);
    if lines.is_empty() {
        lines.push(String::new());
    }
    let (cursor_row, cursor_col) = cursor_row_col(input, cursor, width.max(1));

    let max_height = max_height.max(1);
    let mut start = 0usize;
    if cursor_row >= max_height {
        start = cursor_row + 1 - max_height;
    }
    if start + max_height > lines.len() {
        start = lines.len().saturating_sub(max_height);
    }
    let visible = lines
        .into_iter()
        .skip(start)
        .take(max_height)
        .collect::<Vec<_>>();
    let visible_cursor_row = cursor_row.saturating_sub(start);

    (
        visible,
        visible_cursor_row,
        cursor_col.min(width.saturating_sub(1)),
        start,
    )
}

/// Extended version of `layout_input_with_scroll` that also returns character
/// indices for each wrapped line. Used by ComposerWidget to avoid redundant
/// wrapping when rendering text selections.
fn layout_input_with_scroll_and_char_indices(
    input: &str,
    cursor: usize,
    width: usize,
    max_height: usize,
) -> (Vec<String>, usize, usize, usize, Vec<(usize, String)>) {
    let (all_lines, all_with_indices) = wrap_input_lines_internal(input, width);

    let lines = if all_lines.is_empty() {
        vec![String::new()]
    } else {
        all_lines
    };

    let (cursor_row, cursor_col) = cursor_row_col(input, cursor, width.max(1));

    let max_height = max_height.max(1);
    let mut start = 0usize;
    if cursor_row >= max_height {
        start = cursor_row + 1 - max_height;
    }
    if start + max_height > lines.len() {
        start = lines.len().saturating_sub(max_height);
    }
    let visible = lines
        .into_iter()
        .skip(start)
        .take(max_height)
        .collect::<Vec<_>>();
    let visible_cursor_row = cursor_row.saturating_sub(start);

    // Also slice the char indices to match visible lines
    let visible_with_indices = all_with_indices
        .into_iter()
        .skip(start)
        .take(max_height)
        .collect();

    (
        visible,
        visible_cursor_row,
        cursor_col.min(width.saturating_sub(1)),
        start,
        visible_with_indices,
    )
}

fn cursor_row_col(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    // Derive the cursor's row/col from the SAME wrapped lines the renderer
    // draws. An earlier version recomputed wrapping here with hard margin
    // breaks while wrap_text broke on word boundaries, so the two disagreed on
    // row count: a long paste landed one row short of its marker, and the
    // caret drifted behind fast typing. Walking the actual wrapped lines makes
    // a desync impossible by construction (regression introduced in ff97641b7).
    let (_, lines_with_indices) = wrap_input_lines_internal(input, width.max(1));
    cursor_row_col_in_lines(&lines_with_indices, cursor)
}

/// Map a char-index cursor onto wrapped lines tagged with their starting char
/// index, as produced by wrap_input_lines_internal. The row is the line whose
/// char range contains the cursor; the column is the display width of that
/// line up to the cursor. Because wrap_text emits a trailing empty line when a
/// line fills exactly to the width, a cursor at the end of a full line lands
/// on that empty line (row+1, col 0), the display convention callers rely on,
/// without any special case here.
fn cursor_row_col_in_lines(
    lines_with_indices: &[(usize, String)],
    cursor: usize,
) -> (usize, usize) {
    let mut row = 0usize;
    let mut line_start = 0usize;
    let mut line: &str = "";
    let mut found = false;
    for (i, (start, l)) in lines_with_indices.iter().enumerate() {
        if *start <= cursor {
            row = i;
            line_start = *start;
            line = l.as_str();
            found = true;
        } else {
            break;
        }
    }
    if !found {
        return (0, 0);
    }
    let offset = cursor.saturating_sub(line_start);
    let byte_end = line
        .char_indices()
        .nth(offset)
        .map(|(b, _)| b)
        .unwrap_or(line.len());
    let col = visible_str_width(&line[..byte_end]);
    (row, col)
}

/// Internal helper that returns both wrapped lines and character indices.
/// Used by `wrap_input_lines`, `wrap_input_lines_for_mouse`, and
/// `layout_input_with_scroll` to avoid redundant wrapping computations.
fn wrap_input_lines_internal(input: &str, width: usize) -> (Vec<String>, Vec<(usize, String)>) {
    let mut lines = Vec::new();
    let mut lines_with_indices = Vec::new();
    let mut char_idx = 0usize;

    if input.is_empty() {
        lines_with_indices.push((0, String::new()));
        return (lines, lines_with_indices);
    }

    for raw_line in input.split('\n') {
        if raw_line.is_empty() {
            lines.push(String::new());
            if width != 0 {
                lines_with_indices.push((char_idx, String::new()));
            }
            char_idx += 1; // the '\n'
            continue;
        }

        let wrapped = wrap_text(raw_line, width);
        if wrapped.is_empty() {
            lines.push(String::new());
            if width != 0 {
                lines_with_indices.push((char_idx, String::new()));
            }
        } else {
            for wrapped_line in &wrapped {
                let line_char_len: usize = wrapped_line.chars().count();
                lines.push(wrapped_line.clone());
                if width != 0 {
                    lines_with_indices.push((char_idx, wrapped_line.clone()));
                }
                char_idx += line_char_len;
            }
        }
        char_idx += 1; // the '\n'
    }

    (lines, lines_with_indices)
}

fn wrap_input_lines(input: &str, width: usize) -> Vec<String> {
    let (lines, _) = wrap_input_lines_internal(input, width);
    lines
}

/// For mouse coordinate mapping: returns (char_start_of_line, line_text) pairs
/// matching the wrapping produced by `wrap_input_lines`.
pub fn wrap_input_lines_for_mouse(input: &str, width: usize) -> Vec<(usize, String)> {
    if input.is_empty() || width == 0 {
        return vec![(0, String::new())];
    }

    let (_, lines_with_indices) = wrap_input_lines_internal(input, width);
    lines_with_indices
}

/// Wrap composer text to `width` display columns, breaking at word boundaries
/// where one is available.
///
/// This used to break strictly on the grapheme that crossed the margin, so a
/// wrapped sentence split mid-word — `…Write the file onl` / `y after the…`.
/// The text was never lost, but a line ending in a severed word reads exactly
/// like content that was cut off, which is what it was reported as.
///
/// Two invariants the callers depend on and this must not break:
///
/// * **Nothing is added or removed.** Concatenating the returned lines
///   reproduces `text` exactly. `wrap_input_lines_internal` walks the wrapped
///   lines accumulating `chars().count()` to map cursor and mouse positions
///   back into the raw buffer, so a dropped break character would silently
///   desynchronise the caret. The space a line breaks on therefore stays at
///   the end of the preceding line rather than being swallowed.
/// * **Every line fits.** A word longer than `width` — a URL, a path, a
///   base64 blob — has no usable break point and still breaks hard.
///
/// Display width as painted: ratatui strips control characters, so they
/// occupy no cells. Non-control graphemes keep plain unicode width, matching
/// the long-standing wrap/click/caret contract.
pub(crate) fn visible_grapheme_width(grapheme: &str) -> usize {
    if grapheme.chars().any(|c| c.is_control()) {
        0
    } else {
        grapheme.width()
    }
}

/// Plain unicode width with painted control handling: strip control
/// graphemes first so emoji and wide-glyph measurement keeps the exact
/// [`UnicodeWidthStr`] semantics on the remainder.
fn visible_str_width(text: &str) -> usize {
    text.graphemes(true)
        .filter(|grapheme| !grapheme.chars().any(|c| c.is_control()))
        .collect::<String>()
        .width()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    // Byte offset in `current` just past the most recent space, and the
    // display width up to that point. `None` while the line holds no usable
    // break point — a leading space is not one, since breaking there would
    // emit an empty line and make no progress.
    let mut break_at: Option<(usize, usize)> = None;

    // Flush `current` up to its break point (if any), carrying the remainder
    // onto the next line.
    macro_rules! flush {
        () => {{
            match break_at.take() {
                Some((byte, _)) if byte < current.len() => {
                    let remainder = current.split_off(byte);
                    lines.push(std::mem::replace(&mut current, remainder));
                    current_width = visible_str_width(&current);
                }
                _ => {
                    lines.push(std::mem::take(&mut current));
                    current_width = 0;
                }
            }
        }};
    }

    for grapheme in text.graphemes(true) {
        if grapheme == "\n" {
            break_at = None;
            lines.push(std::mem::take(&mut current));
            current_width = 0;
            continue;
        }

        let grapheme_width = visible_grapheme_width(grapheme);
        if current_width + grapheme_width > width && current_width != 0 {
            flush!();
        }

        current.push_str(grapheme);
        current_width += grapheme_width;
        if grapheme == " " && !current.trim_start().is_empty() {
            break_at = Some((current.len(), current_width));
        }

        if current_width >= width {
            flush!();
        }
    }

    lines.push(current);
    lines
}

fn line_spans_with_selection<'a>(
    line: &'a str,
    line_start: usize,
    line_end: usize,
    sel_start: usize,
    sel_end: usize,
    highlight_bg: Color,
) -> Vec<Span<'a>> {
    let normal_style = Style::default().fg(palette::TEXT_PRIMARY);
    let sel_style = Style::default().fg(palette::TEXT_PRIMARY).bg(highlight_bg);

    // No overlap between this line and the selection
    if line_end <= sel_start || line_start >= sel_end {
        return vec![Span::styled(line, normal_style)];
    }

    let local_sel_start = sel_start.saturating_sub(line_start);
    let local_sel_end = sel_end.min(line_end).saturating_sub(line_start);

    // Build a Vec of byte offsets for each char boundary, plus one past the end.
    let mut byte_offsets: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
    byte_offsets.push(line.len());

    let b0 = byte_offsets
        .get(local_sel_start)
        .copied()
        .unwrap_or(line.len());
    let b1 = byte_offsets
        .get(local_sel_end)
        .copied()
        .unwrap_or(line.len());

    let mut spans = Vec::with_capacity(3);

    // Text before selection
    if b0 > 0 {
        spans.push(Span::styled(&line[..b0], normal_style));
    }
    // Selected text
    if b1 > b0 {
        spans.push(Span::styled(&line[b0..b1], sel_style));
    }
    // Text after selection
    if b1 < line.len() {
        spans.push(Span::styled(&line[b1..], normal_style));
    }

    spans
}

// End exact frozen composer source fragments; test-only, no production fallback.
