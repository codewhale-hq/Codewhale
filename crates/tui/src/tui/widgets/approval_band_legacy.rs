// Frozen original approval band and helpers; private test counterpart only.
use super::*;
/// Compact, bottom-anchored approval card.
///
/// The widget reads its selected option and locale directly from the
/// [`ApprovalView`]. Rendering preserves transcript context while reserving
/// the complete action set and at least one load-bearing command/preview row
/// on ordinary terminal sizes.
pub struct ApprovalWidget<'a> {
    request: &'a ApprovalRequest,
    view: &'a ApprovalView,
}

impl<'a> ApprovalWidget<'a> {
    pub fn new(request: &'a ApprovalRequest, view: &'a ApprovalView) -> Self {
        Self { request, view }
    }

    /// Build the inline approval content, split into the informational `body`
    /// (which may scroll/truncate within its region) and the interactive
    /// `controls` (which are always reserved and can never be clipped). Both
    /// `render` and `inline_region` use this so the painted band and the
    /// dimmed backdrop region always agree.
    ///
    /// The save preview says what a persistent rule would cover while the
    /// controls offer to save it, so it is never dropped: it is a trust
    /// boundary, not decoration. A band too short for the full preview gets
    /// one line per rule instead of calling the request "truncated" (#6566).
    /// The band always reserves the compact preview's rows and `render` pins
    /// the preview above the controls, so a short band cuts the request
    /// detail, never the preview. A frame too small (or too narrow) for even
    /// the one-line preview fails closed: the card drops the preview and the
    /// save offers together (`[p]`, `s`), keeping only one-off decisions.
    fn build_inline_content(&self, area: Rect) -> InlineContent {
        let (compact, save_start, controls) = self.build_inline_parts(area, true, true);
        let save_reserve = measure_wrapped_rows(&compact[save_start..], area.width);
        let compact = InlineContent {
            body: compact,
            save_start,
            save_reserve,
            controls,
            save_shown: true,
        };
        if save_start == compact.body.len() {
            return InlineContent {
                save_shown: false,
                ..compact
            };
        }
        if !compact.save_preview_fits(area) {
            let (body, save_start, controls) = self.build_inline_parts(area, true, false);
            return InlineContent {
                body,
                save_start,
                save_reserve: 0,
                controls,
                save_shown: false,
            };
        }
        let (body, save_start, controls) = self.build_inline_parts(area, false, true);
        let full = InlineContent {
            body,
            save_start,
            save_reserve,
            controls,
            save_shown: true,
        };
        if full.body_fits(area) { full } else { compact }
    }

    /// The body, how many of its leading lines come before the save preview,
    /// and the controls. `compact_save_preview` puts each rule on one line;
    /// without `offer_save` there is neither a save preview nor a save offer.
    fn build_inline_parts(
        &self,
        area: Rect,
        compact_save_preview: bool,
        offer_save: bool,
    ) -> (Vec<Line<'static>>, usize, Vec<Line<'static>>) {
        let risk = self.request.risk;
        let stakes = self.request.stakes();
        let locale = self.view.locale();
        let repo_law = self.request.is_repo_law_prompt();
        let palette_colors = if repo_law {
            repo_law_approval_palette()
        } else {
            approval_palette(stakes)
        };
        let critical = matches!(stakes, crate::tui::approval::ApprovalStakes::Critical);

        let mut body: Vec<Line<'static>> = Vec::with_capacity(16);
        // Header: effect badge + the plain summary of the call (E6). The raw
        // tool name stays one details chord away in the pager.
        body.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!(
                    " {} ",
                    if repo_law {
                        tr(locale, MessageId::ApprovalRepoLawBadge)
                    } else {
                        effect_badge_text(self.request, stakes, locale)
                    }
                ),
                Style::default()
                    .fg(palette::WHALE_BG)
                    .bg(palette_colors.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                if repo_law {
                    format!(
                        "{} · {}",
                        tr(locale, MessageId::ApprovalRepoLawTitle),
                        approval_heading(self.request, locale)
                    )
                } else {
                    approval_heading(self.request, locale)
                },
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        // A child's card names the agent that is waiting (approvals C1).
        if let Some(owner) = self.request.owner.as_ref() {
            body.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    approval_owner_header(owner, locale),
                    Style::default()
                        .fg(palette::TEXT_SECONDARY)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        if repo_law {
            body.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "◆ ",
                    Style::default()
                        .fg(palette::STATUS_WARNING)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    tr(locale, MessageId::ApprovalRepoLawWarning),
                    Style::default()
                        .fg(palette::WHALE_ERROR)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            body.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    tr(locale, MessageId::ApprovalRepoLawRuleLabel),
                    Style::default().fg(palette::TEXT_HINT),
                ),
                Span::styled(
                    self.request.description.clone(),
                    Style::default().fg(palette::TEXT_SECONDARY),
                ),
            ]));
        }

        // Command / change preview FIRST — for an approval the thing being run
        // is the load-bearing content, so on a short terminal it is the
        // secondary context (about/impacts/category) that scrolls away, never
        // the command.
        let details = self.request.prominent_detail_items(locale);
        if details.is_empty() {
            push_params_detail_line(&mut body, self.request, locale, area.width);
        } else {
            let mut rendered_detail = false;
            for detail in details.iter().take(4) {
                let is_change_preview = detail.is_preview;
                if let Some(shell_lines) = detail.shell_lines.as_deref() {
                    let command_width = area.width.saturating_sub(10) as usize;
                    // A short approval band has room for only one detail row
                    // before its truncation hint. Project the most useful
                    // command/change into that row instead of spending it on
                    // setup (`cd`, `set`) or diff metadata. The complete,
                    // original-order value remains available in the details
                    // pager.
                    let inline_shell_lines = prioritize_inline_shell_lines(
                        shell_lines,
                        is_change_preview,
                        area.height <= 24,
                    );
                    // Bound every multi-line preview so one huge command cannot
                    // grow the band without limit; the details chord opens the rest.
                    let max_rows = if is_change_preview {
                        if self.request.intent_summary.is_some() {
                            Some(3)
                        } else {
                            Some(5)
                        }
                    } else {
                        Some(8)
                    };
                    push_shell_command_lines(
                        &mut body,
                        &detail.label,
                        &inline_shell_lines,
                        command_width.max(20),
                        max_rows,
                    );
                } else {
                    push_detail_line(&mut body, &detail.label, &detail.value);
                }
                rendered_detail = true;
            }
            if !rendered_detail {
                push_params_detail_line(&mut body, self.request, locale, area.width);
            }
        }

        // Intent summary ("why this change is needed", #2381).
        if let Some(ref summary) = self.request.intent_summary {
            let max_width = area.width.saturating_sub(14) as usize;
            if max_width > 0 {
                let intent_label = tr(locale, MessageId::ApprovalIntentLabel);
                let summary_lines: Vec<&str> = summary.lines().collect();
                let intent_lines = 3usize;
                for (i, sline) in summary_lines.iter().take(intent_lines).enumerate() {
                    let prefix = if i == 0 {
                        intent_label.clone()
                    } else {
                        Cow::Borrowed("  ")
                    };
                    let truncated = crate::utils::truncate_with_ellipsis(sline, max_width, "...");
                    body.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            prefix,
                            if i == 0 {
                                Style::default().fg(palette::TEXT_HINT)
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled(truncated, Style::default().fg(palette::TEXT_SECONDARY)),
                    ]));
                }
                if summary_lines.len() > intent_lines {
                    let more = tr(locale, MessageId::ApprovalMoreLines)
                        .replace("{count}", &(summary_lines.len() - intent_lines).to_string());
                    body.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(more, Style::default().fg(palette::TEXT_HINT)),
                    ]));
                }
            }
        }

        // Destructive policy / cancel semantics — critical stakes only. For
        // routine and elevated work the controls speak for themselves; the
        // extra policy prose was noise that made every edit read like an
        // emergency.
        // The semantics prose says Esc stops the turn; a child's card hides
        // on Esc instead, so it never shows that line.
        if critical && self.request.owner.is_none() {
            push_destructive_approval_semantics(&mut body, locale, false);
        }

        // Secondary context: what it is and what it touches. Only critical
        // prompts carry the full about/impact/category dossier by default —
        // everything stays one details chord away in the pager. Keep a single
        // About line as fallback context when nothing else was rendered.
        if critical || details.is_empty() {
            body.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(label_about(locale), Style::default().fg(palette::TEXT_HINT)),
                Span::styled(
                    self.request.description_for_locale(locale),
                    Style::default().fg(palette::TEXT_BODY),
                ),
            ]));
        }
        if critical {
            for impact in self.request.impacts_for_locale(locale).into_iter().take(4) {
                body.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        label_impact(locale),
                        Style::default().fg(palette::TEXT_HINT),
                    ),
                    Span::styled(impact, Style::default().fg(palette::TEXT_BODY)),
                ]));
            }
            // Category line — localized risk category.
            let (cat_label, cat_color) = category_label_for(self.request, locale);
            body.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(label_type(locale), Style::default().fg(palette::TEXT_HINT)),
                Span::styled(
                    cat_label,
                    Style::default().fg(cat_color).add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        // Preview the validated persistent-rule candidates. Informational, so
        // they live in the scrollable body rather than the action rows.
        let essential_len = body.len();
        if let Some(preview) = self.request.ask_rule_save_preview().filter(|_| offer_save) {
            push_permission_rule_save_preview(
                &mut body,
                &preview,
                palette_colors.shortcut,
                area.width,
                compact_save_preview,
            );
        }
        if let Some(preview) = self
            .request
            .allow_rule_save_preview()
            .filter(|_| offer_save)
        {
            push_permission_rule_save_preview(
                &mut body,
                &preview,
                palette_colors.shortcut,
                area.width,
                compact_save_preview,
            );
        }

        let controls = build_approval_controls(
            self.request,
            self.view,
            risk,
            locale,
            palette_colors.accent,
            palette_colors.shortcut,
            offer_save,
        );
        (body, essential_len, controls)
    }

    /// Bottom-anchored band this inline prompt occupies within `area`. Must
    /// match what `render` paints so the backdrop dims exactly this strip.
    pub(crate) fn inline_region(&self, area: Rect) -> Rect {
        if area.width == 0 || area.height == 0 {
            return Rect {
                x: area.x,
                y: area.y.saturating_add(area.height),
                width: 0,
                height: 0,
            };
        }
        if self.view.collapsed {
            // Collapsed mode is a single banner row pinned to the bottom.
            let h = area.height.min(1);
            return Rect {
                x: area.x,
                y: area.y.saturating_add(area.height.saturating_sub(h)),
                width: area.width,
                height: h,
            };
        }
        self.build_inline_content(area).region(area)
    }
}

impl Renderable for ApprovalWidget<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Collapsed mode: a single-line banner at the bottom of the area
        // so the user can still see the transcript behind it.
        if self.view.collapsed {
            self.view.set_mouse_hitboxes(Vec::new());
            self.view.set_save_preview_shown(false);
            let bar_y = area.y.saturating_add(area.height.saturating_sub(1));
            let bar_area = Rect::new(area.x, bar_y, area.width, 1);
            Clear.render(bar_area, buf);

            let stakes = self.request.stakes();
            let repo_law = self.request.is_repo_law_prompt();
            let palette_colors = if repo_law {
                repo_law_approval_palette()
            } else {
                approval_palette(stakes)
            };
            let summary = format!(
                " {} — {}  [Tab to expand] ",
                if repo_law {
                    tr(self.view.locale(), MessageId::ApprovalRepoLawTitle)
                } else {
                    Cow::Owned(approval_heading(self.request, self.view.locale()))
                },
                if repo_law {
                    tr(self.view.locale(), MessageId::ApprovalRepoLawBadge)
                } else {
                    effect_badge_text(self.request, stakes, self.view.locale())
                },
            );
            let line = Line::from(Span::styled(
                summary,
                Style::default()
                    .fg(palette::WHALE_BG)
                    .bg(palette_colors.accent)
                    .add_modifier(Modifier::BOLD),
            ));
            Paragraph::new(line).render(bar_area, buf);
            return;
        }

        // Compute stakes once for this render pass (it runs command_safety
        // analysis on shell commands); reuse it for the palette and the
        // left-rail gate instead of re-deriving per band.
        let stakes = self.request.stakes();
        let repo_law = self.request.is_repo_law_prompt();
        let palette_colors = if repo_law {
            repo_law_approval_palette()
        } else {
            approval_palette(stakes)
        };
        let content = self.build_inline_content(area);
        let region = content.region(area);
        let InlineContent {
            body,
            save_start,
            controls,
            save_shown,
            ..
        } = content;
        self.view.set_save_preview_shown(false);
        if region.width == 0 || region.height == 0 {
            return;
        }
        self.view.set_save_preview_shown(save_shown);

        // Opaque inline panel anchored to the bottom of the frame. The
        // transcript above stays visible; only this band is painted — the
        // approval is no longer a full-screen takeover (#3799).
        Clear.render(region, buf);
        Block::default()
            .style(Style::default().bg(palette::WHALE_BG))
            .render(region, buf);

        // Top separator rule, risk-tinted, so the prompt reads as a distinct
        // panel without a heavy full border box.
        let rule_glyph = if repo_law { "═" } else { "─" };
        let rule: String = rule_glyph.repeat(region.width as usize);
        buf.set_string(
            region.x,
            region.y,
            &rule,
            Style::default().fg(palette_colors.border),
        );

        // Reserve the controls FIRST: they take their rows off the bottom of
        // the band and can never be clipped, no matter how long the body is.
        // The informational body takes whatever remains and shows a pager
        // affordance when it does not fit. This is the core #3799 fix — the
        // action row is no longer the last thing in a single clipping
        // Paragraph.
        let inner_top = region.y.saturating_add(1);
        let inner_height = region.height.saturating_sub(1);
        let control_rows = measure_wrapped_rows(&controls, region.width).min(inner_height);
        let body_height = inner_height.saturating_sub(control_rows);

        let body_rect = Rect {
            x: region.x,
            y: inner_top,
            width: region.width,
            height: body_height,
        };
        let control_rect = Rect {
            x: region.x,
            y: inner_top.saturating_add(body_height),
            width: region.width,
            height: control_rows,
        };

        // One hitbox per option in `ApprovalOption` order; an option the card
        // is not offering keeps an empty box so the indices stay aligned.
        let mut hitboxes = Vec::new();
        let options =
            approval_options_for_request(self.request, self.request.risk, self.view.locale());
        let mut shown_index = 0;
        for option in &options {
            if option.persistent && !save_shown {
                hitboxes.push(Rect::default());
                continue;
            }
            let first_line = 1 + shown_index;
            shown_index += 1;
            let y_offset = measure_wrapped_rows(&controls[..first_line], region.width);
            let next_offset = measure_wrapped_rows(&controls[..first_line + 1], region.width);
            let y = control_rect.y.saturating_add(y_offset);
            let height = next_offset.saturating_sub(y_offset).min(
                control_rect
                    .y
                    .saturating_add(control_rect.height)
                    .saturating_sub(y),
            );
            if height > 0 {
                hitboxes.push(Rect::new(control_rect.x, y, control_rect.width, height));
            }
        }
        self.view.set_mouse_hitboxes(hitboxes);

        let body_rows = measure_wrapped_rows(&body, region.width);
        if body_rows > body_height && body_height > 0 {
            // Body does not fit (short terminal). The save preview is pinned
            // directly above the controls that offer to save it; the request
            // detail above it shows as much as fits and points at the params
            // pager through the platform-aware details chord.
            let mut body = body;
            let save = body.split_off(save_start.min(body.len()));
            let save_rows = measure_wrapped_rows(&save, region.width).min(body_height);
            let head_height = body_height.saturating_sub(save_rows);
            if head_height > 0 {
                let shown = head_height.saturating_sub(1);
                if shown > 0 {
                    Paragraph::new(body).wrap(Wrap { trim: false }).render(
                        Rect {
                            height: shown,
                            ..body_rect
                        },
                        buf,
                    );
                }
                buf.set_string(
                    region.x,
                    body_rect.y.saturating_add(shown),
                    approval_truncation_hint(self.view.locale()),
                    Style::default().fg(palette::TEXT_HINT),
                );
            }
            if save_rows > 0 {
                Paragraph::new(save).wrap(Wrap { trim: false }).render(
                    Rect {
                        y: body_rect.y.saturating_add(head_height),
                        height: save_rows,
                        ..body_rect
                    },
                    buf,
                );
            }
        } else {
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .render(body_rect, buf);
        }

        Paragraph::new(controls)
            .wrap(Wrap { trim: false })
            .render(control_rect, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1
    }
}

/// The inline approval band's lines. `body[save_start..]` is the
/// persistent-rule save preview; `save_reserve` is the rows its one-line
/// form needs, which the band always keeps for it. `save_shown` says the
/// preview is on screen, and with it the offers to save the rule.
struct InlineContent {
    body: Vec<Line<'static>>,
    save_start: usize,
    save_reserve: u16,
    controls: Vec<Line<'static>>,
    save_shown: bool,
}

impl InlineContent {
    fn region(&self, area: Rect) -> Rect {
        inline_region_for(area, &self.body, self.save_reserve, &self.controls)
    }

    /// Whether the band keeps the whole one-line save preview on screen
    /// above the controls (render pins it there when the body is cut).
    fn save_preview_fits(&self, area: Rect) -> bool {
        let region = self.region(area);
        let inner_height = region.height.saturating_sub(1);
        let control_rows = measure_wrapped_rows(&self.controls, region.width).min(inner_height);
        self.save_reserve <= inner_height.saturating_sub(control_rows)
    }

    /// Whether the whole body fits the band above the controls.
    fn body_fits(&self, area: Rect) -> bool {
        let region = self.region(area);
        let inner_height = region.height.saturating_sub(1);
        let control_rows = measure_wrapped_rows(&self.controls, region.width).min(inner_height);
        measure_wrapped_rows(&self.body, region.width) <= inner_height.saturating_sub(control_rows)
    }
}

/// Bottom-anchored band the inline approval prompt occupies within `area`.
/// Sized to the measured content, capped to half the frame like the compact
/// permission surfaces in peer coding agents, and always tall enough to show
/// the reserved controls (#3799). Full details remain available through the
/// platform-aware details chord.
///
/// `save_rows` are the rows of the one-line persistent-rule save preview.
/// They are always reserved after the controls, on every frame height,
/// because the controls offer to save that rule and the person must see what
/// it covers.
fn inline_region_for(
    area: Rect,
    body: &[Line<'static>],
    save_rows: u16,
    controls: &[Line<'static>],
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect {
            x: area.x,
            y: area.y.saturating_add(area.height),
            width: 0,
            height: 0,
        };
    }
    let width = area.width;
    let body_rows = measure_wrapped_rows(body, width);
    let control_rows = measure_wrapped_rows(controls, width);
    // +1 for the top separator rule.
    let desired = 1u16.saturating_add(body_rows).saturating_add(control_rows);
    // Never shrink below the rule + controls. At normal terminal heights,
    // reserve four body rows: header, detail label, at least one command or
    // preview row, and the truncation hint. Half a viewport is the preferred
    // cap; up to four fifths is allowed only when necessary to retain that
    // load-bearing preview on a short frame. The extra permanent-grant row
    // needs one more reserved line than the legacy four-action card. Truly
    // tiny frames prioritize the complete action set and details chord.
    let controls_floor = 1u16.saturating_add(control_rows).min(area.height);
    // The request's own preview (what runs now) and the save preview (what a
    // saved rule would cover from now on) are reserved side by side: neither
    // may push the other off a short band.
    let head_rows = body_rows.saturating_sub(save_rows);
    let preview_rows = if area.height >= 16 {
        head_rows.min(4).saturating_add(save_rows)
    } else {
        save_rows
    };
    let preview_floor = controls_floor.saturating_add(preview_rows).min(area.height);
    let preferred_cap = area.height.div_ceil(2);
    let short_frame_cap = area.height.saturating_mul(4).div_ceil(5);
    // The save preview is never traded for the short-frame cap: whenever the
    // frame has rows after the controls, the preview gets them first.
    let save_floor = controls_floor.saturating_add(save_rows).min(area.height);
    let max_height = preferred_cap
        .max(preview_floor.min(short_frame_cap.saturating_add(save_rows)))
        .max(save_floor)
        .min(area.height);
    let min_height = controls_floor;
    let height = desired.clamp(min_height, max_height);
    Rect {
        x: area.x,
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width,
        height,
    }
}

/// Terminal rows `lines` occupy under the exact ratatui word-wrap used by the
/// renderer. Exact measurement keeps localized controls and their mouse
/// hitboxes aligned without padding the compact approval band.
fn measure_wrapped_rows(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    let rows = Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width);
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// Build the always-visible approval controls: a "proceed?" prompt, the
/// numbered/selectable options, and the selection hint. Rendered into a region
/// reserved off the bottom of the band so it can never be clipped (#3799).
fn build_approval_controls(
    request: &ApprovalRequest,
    view: &ApprovalView,
    risk: RiskLevel,
    locale: Locale,
    accent: Color,
    shortcut: Color,
    offer_save: bool,
) -> Vec<Line<'static>> {
    let mut controls: Vec<Line<'static>> = Vec::with_capacity(6);
    controls.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            approval_proceed_question(locale),
            Style::default()
                .fg(palette::TEXT_BODY)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    let options = approval_options_for_request(request, risk, locale);
    for (i, opt) in options.iter().enumerate() {
        if opt.persistent && !offer_save {
            continue;
        }
        let is_selected = i == view.selected();
        let label_color = if opt.dangerous {
            accent
        } else {
            palette::TEXT_BODY
        };
        let option_style = approval_option_style(is_selected, label_color);
        let shortcut_style = approval_option_style(is_selected, shortcut);
        // Leading caret marks the row Enter will fire — selection is not
        // signalled by background alone.
        let lead = if is_selected {
            Span::styled("\u{276f} ", approval_selected_style())
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![
            lead,
            Span::styled(
                format!("[{}] ", opt.key_hint),
                shortcut_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(opt.label.to_string(), option_style),
        ];
        if is_selected {
            spans.push(Span::styled(" (Enter)", shortcut_style));
        }
        controls.push(Line::from(spans));
    }
    controls.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            if request.owner.is_some() {
                child_footer_controls(locale)
            } else {
                footer_controls(locale)
            },
            Style::default().fg(palette::TEXT_MUTED),
        ),
        if offer_save && request.can_save_ask_rule() {
            Span::styled(save_ask_rule_hint(locale), Style::default().fg(shortcut))
        } else {
            Span::raw("")
        },
    ]));
    controls
}

fn approval_proceed_question(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "是否继续？",
        _ => "Do you want to proceed?",
    }
}

fn approval_truncation_hint(locale: Locale) -> Cow<'static, str> {
    let details = crate::tui::shell_key_routing::tool_details_chord();
    Cow::Owned(tr(locale, MessageId::ApprovalTruncationHint).replace("{details}", details.as_ref()))
}

/// Approval palette per risk variant.
struct ApprovalColors {
    border: Color,
    accent: Color,
    shortcut: Color,
}

fn approval_palette(stakes: crate::tui::approval::ApprovalStakes) -> ApprovalColors {
    use crate::tui::approval::ApprovalStakes;
    match stakes {
        ApprovalStakes::Routine => ApprovalColors {
            border: palette::BORDER_COLOR,
            accent: palette::WHALE_HUMAN,
            shortcut: palette::WHALE_ACTION,
        },
        // Ordinary state-touching work: a calm ask, not an alarm.
        ApprovalStakes::Elevated => ApprovalColors {
            border: palette::WHALE_HUMAN,
            accent: palette::WHALE_HUMAN,
            shortcut: palette::WHALE_ACTION,
        },
        ApprovalStakes::Critical => ApprovalColors {
            border: palette::WHALE_ERROR,
            accent: palette::WHALE_ERROR,
            shortcut: palette::STATUS_WARNING,
        },
    }
}

fn repo_law_approval_palette() -> ApprovalColors {
    ApprovalColors {
        border: palette::STATUS_WARNING,
        accent: palette::WHALE_ERROR,
        shortcut: palette::STATUS_WARNING,
    }
}

fn approval_selected_style() -> Style {
    menu_style::selected_row_style()
}

fn approval_option_style(is_selected: bool, color: Color) -> Style {
    if is_selected {
        approval_selected_style()
    } else {
        Style::default().fg(color)
    }
}

/// The approval card's heading: the plain summary of the call (E6), in the
/// card's language, falling back to the tool name only when no summary was
/// derived.
fn approval_heading(request: &ApprovalRequest, locale: Locale) -> String {
    if request.summary.trim().is_empty() {
        return request.tool_name.clone();
    }
    let summary = request.summary_for_locale(locale);
    if summary.trim().is_empty() {
        request.tool_name.clone()
    } else {
        summary
    }
}

/// Badge naming what the call does, not a risk tier: "Reads only", "Changes
/// files", "Runs a command", "Uses the network". Anything the stakes
/// classifier calls destructive or publishing reads "Can't be undone".
fn effect_badge_text(
    request: &ApprovalRequest,
    stakes: crate::tui::approval::ApprovalStakes,
    locale: Locale,
) -> Cow<'static, str> {
    if stakes == crate::tui::approval::ApprovalStakes::Critical {
        return tr(locale, MessageId::ApprovalRiskDestructive);
    }
    let id = match request.category {
        ToolCategory::Safe | ToolCategory::McpRead => MessageId::ApprovalEffectReadsOnly,
        ToolCategory::FileWrite => MessageId::ApprovalEffectChangesFiles,
        ToolCategory::Shell => MessageId::ApprovalEffectRunsCommand,
        ToolCategory::Network => MessageId::ApprovalEffectUsesNetwork,
        ToolCategory::McpAction => MessageId::ApprovalEffectConnectedApp,
        ToolCategory::Agent => MessageId::ApprovalEffectStartsAgent,
        ToolCategory::Unknown => MessageId::ApprovalEffectUnclassified,
    };
    tr(locale, id)
}

fn category_label_for(request: &ApprovalRequest, locale: Locale) -> (Cow<'static, str>, Color) {
    let category = request.category;
    let label = match category {
        ToolCategory::Safe => tr(locale, MessageId::ApprovalCategorySafe),
        ToolCategory::FileWrite => tr(locale, MessageId::ApprovalCategoryFileWrite),
        ToolCategory::Shell => tr(locale, MessageId::ApprovalCategoryShell),
        ToolCategory::Network => tr(locale, MessageId::ApprovalCategoryNetwork),
        ToolCategory::McpRead => tr(locale, MessageId::ApprovalCategoryMcpRead),
        ToolCategory::McpAction => tr(locale, MessageId::ApprovalCategoryMcpAction),
        ToolCategory::Agent => tr(locale, MessageId::ApprovalCategoryAgent),
        ToolCategory::Unknown => tr(locale, MessageId::ApprovalCategoryUnknown),
    };
    // "Connected app (github)": name the server the tool comes from.
    let label = match (
        category,
        crate::tui::approval::connected_app_server(&request.tool_name),
    ) {
        (ToolCategory::McpRead | ToolCategory::McpAction, Some(server)) => {
            Cow::Owned(format!("{label} ({server})"))
        }
        _ => label,
    };
    let color = match category {
        ToolCategory::Safe => palette::STATUS_SUCCESS,
        ToolCategory::FileWrite => palette::STATUS_WARNING,
        ToolCategory::Shell => palette::STATUS_ERROR,
        ToolCategory::Network => palette::STATUS_WARNING,
        ToolCategory::McpRead => palette::WHALE_ACTION,
        ToolCategory::McpAction => palette::STATUS_WARNING,
        ToolCategory::Agent => palette::WHALE_ACTION,
        ToolCategory::Unknown => palette::STATUS_ERROR,
    };
    (label, color)
}

fn label_type(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalFieldType)
}

fn label_about(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalFieldAbout)
}

fn label_impact(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalFieldImpact)
}

fn label_params(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalFieldParams)
}

fn push_detail_line(lines: &mut Vec<Line<'static>>, label: &str, value: &str) {
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{label:<7} "),
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(palette::TEXT_BODY)),
    ]));
}

fn push_params_detail_line(
    lines: &mut Vec<Line<'static>>,
    request: &ApprovalRequest,
    locale: Locale,
    card_width: u16,
) {
    let params_str = request.params_display();
    let params_width = card_width.saturating_sub(14) as usize;
    let params_truncated =
        crate::utils::truncate_with_ellipsis(&params_str, params_width.max(20), "...");
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            label_params(locale),
            Style::default().fg(palette::TEXT_HINT),
        ),
        Span::styled(
            params_truncated,
            Style::default().fg(palette::TEXT_SECONDARY),
        ),
    ]));
}

fn push_permission_rule_save_preview(
    lines: &mut Vec<Line<'static>>,
    preview: &crate::tui::approval::PermissionRuleSavePreview,
    shortcut: Color,
    card_width: u16,
    compact: bool,
) {
    if compact {
        // One line: what saving does, then what it covers, with the count of
        // entries that did not fit kept visible after any ellipsis.
        let summary = preview.summary();
        let more = if preview.omitted > 0 {
            format!(" +{} more", preview.omitted)
        } else {
            String::new()
        };
        let budget = (card_width as usize)
            .saturating_sub(10 + summary.chars().count() + 3 + more.chars().count())
            .max(12);
        let entries =
            crate::utils::truncate_with_ellipsis(&preview.entries.join("; "), budget, "...");
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "Save:   ",
                Style::default().fg(shortcut).add_modifier(Modifier::BOLD),
            ),
            Span::styled(summary, Style::default().fg(palette::TEXT_BODY)),
            Span::styled(
                format!(" · {entries}{more}"),
                Style::default().fg(palette::TEXT_SECONDARY),
            ),
        ]));
        return;
    }
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Save:   ",
            Style::default().fg(shortcut).add_modifier(Modifier::BOLD),
        ),
        Span::styled(preview.summary(), Style::default().fg(palette::TEXT_BODY)),
    ]));

    let entry_width = card_width.saturating_sub(10) as usize;
    let entries = preview.entries.join("; ");
    let truncated = crate::utils::truncate_with_ellipsis(&entries, entry_width.max(20), "...");
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(truncated, Style::default().fg(palette::TEXT_SECONDARY)),
    ]));
    if preview.omitted > 0 {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("... {} more", preview.omitted),
                Style::default().fg(palette::TEXT_HINT),
            ),
        ]));
    }
}

fn push_shell_command_lines(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    command_lines: &[String],
    command_width: usize,
    max_rows: Option<usize>,
) {
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{label}:"),
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let mut rendered = 0usize;
    for line in command_lines {
        for wrapped in wrap_text(line, command_width) {
            if max_rows.is_some_and(|limit| rendered >= limit) {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        "...",
                        Style::default()
                            .fg(palette::TEXT_HINT)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                return;
            }
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    wrapped,
                    Style::default()
                        .fg(palette::TEXT_BODY)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            rendered += 1;
        }
    }
}

/// Put one representative command/change first for compact inline rendering.
/// This is a display-only projection: approval parameters and the details
/// pager retain the exact original order.
fn prioritize_inline_shell_lines(
    command_lines: &[String],
    is_change_preview: bool,
    compact: bool,
) -> Vec<String> {
    if !compact || command_lines.len() < 2 {
        return command_lines.to_vec();
    }

    let representative = if is_change_preview {
        command_lines
            .iter()
            .enumerate()
            .max_by_key(|(index, line)| (preview_line_priority(line), std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
    } else {
        command_lines
            .iter()
            .enumerate()
            .max_by_key(|(index, line)| (command_line_priority(line), std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
    };
    let Some(representative) = representative.filter(|index| *index > 0) else {
        return command_lines.to_vec();
    };

    let mut projected = Vec::with_capacity(command_lines.len());
    projected.push(command_lines[representative].clone());
    projected.extend(
        command_lines
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != representative)
            .map(|(_, line)| line.clone()),
    );
    projected
}

fn preview_line_priority(line: &str) -> u8 {
    let trimmed = line.trim_start();
    if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
        4
    } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
        3
    } else if trimmed.starts_with("@@") {
        2
    } else if trimmed.starts_with("diff ")
        || trimmed.starts_with("---")
        || trimmed.starts_with("+++")
    {
        0
    } else {
        1
    }
}

fn command_line_priority(line: &str) -> u8 {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return 0;
    }

    let tokens = trimmed
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '(' | ')'))
        .filter(|token| !token.is_empty())
        .map(|token| token.rsplit('/').next().unwrap_or(token))
        .collect::<Vec<_>>();
    if tokens.iter().any(|token| {
        matches!(
            *token,
            "rm" | "rmdir"
                | "unlink"
                | "mv"
                | "dd"
                | "chmod"
                | "chown"
                | "kill"
                | "pkill"
                | "shutdown"
                | "reboot"
                | "mkfs"
        )
    }) || tokens.windows(2).any(|pair| {
        matches!(
            pair,
            ["git", "push"] | ["cargo", "publish"] | ["npm", "publish"]
        )
    }) || trimmed.contains('>')
    {
        return 4;
    }

    let first = tokens.first().copied().unwrap_or_default();
    if matches!(
        first,
        "cd" | "pushd" | "popd" | "set" | "export" | "unset" | "pwd" | ":" | "true"
    ) {
        1
    } else if matches!(first, "echo" | "printf") {
        2
    } else {
        3
    }
}

fn push_destructive_approval_semantics(
    lines: &mut Vec<Line<'static>>,
    locale: Locale,
    compact: bool,
) {
    if compact {
        let (label, value) = destructive_approval_compact_semantics(locale);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(label, Style::default().fg(palette::TEXT_HINT)),
            Span::styled(value, Style::default().fg(palette::TEXT_SECONDARY)),
        ]));
        return;
    }

    for (label, value) in destructive_approval_semantics(locale) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(label, Style::default().fg(palette::TEXT_HINT)),
            Span::styled(value, Style::default().fg(palette::TEXT_SECONDARY)),
        ]));
    }
}

fn destructive_approval_compact_semantics(locale: Locale) -> (&'static str, &'static str) {
    match locale {
        Locale::ZhHans => ("规则: ", "批准策略要求确认；拒绝跳过本次，Esc 中止整轮。"),
        _ => (
            "Why: ",
            "Your permissions ask before this; d doesn't allow it, Esc stops the turn.",
        ),
    }
}

fn destructive_approval_semantics(locale: Locale) -> [(&'static str, &'static str); 2] {
    match locale {
        Locale::ZhHans => [
            ("规则: ", "你的设置要求先确认这一步。"),
            ("取消: ", "拒绝只跳过本次工具调用；Esc 会中止整轮。"),
        ],
        _ => [
            ("Why: ", "Your settings ask you to confirm this step first."),
            (
                "Stop: ",
                "Don't allow skips only this step; Esc stops the whole turn.",
            ),
        ],
    }
}

fn footer_controls(locale: Locale) -> Cow<'static, str> {
    // Platform-aware details chord (⌥V on macOS, Alt+V elsewhere). Bare `v`
    // is never advertised as a details shortcut (TUI-DOG-002).
    let details = crate::tui::shell_key_routing::tool_details_chord();
    Cow::Owned(tr(locale, MessageId::ApprovalControlsHint).replace("{details}", details.as_ref()))
}

/// Controls hint for a child's card: Esc hides it, `g` opens the agent.
fn child_footer_controls(locale: Locale) -> Cow<'static, str> {
    let details = crate::tui::shell_key_routing::tool_details_chord();
    Cow::Owned(format!(
        "{}  ·  {}",
        tr(locale, MessageId::ApprovalControlsHintChild).replace("{details}", details.as_ref()),
        tr(locale, MessageId::ApprovalGoToAgent)
    ))
}

/// "Agent: {agent} · {role}", dropping the role segment when the roster does
/// not know the agent's role yet.
fn approval_owner_header(owner: &crate::tui::approval::ApprovalOwner, locale: Locale) -> String {
    let template = tr(locale, MessageId::ApprovalOwnerHeader);
    let with_agent = template.replace("{agent}", &owner.label);
    match owner.role.as_deref() {
        Some(role) => with_agent.replace("{role}", role),
        None => with_agent
            .replace(" · {role}", "")
            .replace("{role}", "")
            .trim_end()
            .to_string(),
    }
}

fn save_ask_rule_hint(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalSaveAskRuleHint)
}

#[derive(Clone)]
struct ApprovalOptionRow {
    label: Cow<'static, str>,
    key_hint: &'static str,
    dangerous: bool,
    /// Saves a persistent rule: offered only beside its save preview.
    persistent: bool,
}

fn approval_options_for(risk: RiskLevel, locale: Locale) -> [ApprovalOptionRow; 4] {
    let dangerous = matches!(risk, RiskLevel::Destructive);
    [
        ApprovalOptionRow {
            label: option_approve_once(locale),
            key_hint: "1 / y",
            dangerous,
            persistent: false,
        },
        ApprovalOptionRow {
            label: option_approve_always(locale),
            key_hint: "2 / a",
            dangerous,
            persistent: false,
        },
        ApprovalOptionRow {
            label: option_deny(locale),
            key_hint: "3 / d / n",
            dangerous: false,
            persistent: false,
        },
        ApprovalOptionRow {
            label: option_abort(locale),
            key_hint: "Esc",
            dangerous: false,
            persistent: false,
        },
    ]
}

/// Workflow elevated-plan card options (#4126): Approve / Edit plan / Cancel.
fn workflow_approval_options(risk: RiskLevel, locale: Locale) -> [ApprovalOptionRow; 3] {
    let dangerous = matches!(risk, RiskLevel::Destructive);
    [
        ApprovalOptionRow {
            label: workflow_option_approve(locale),
            key_hint: "1 / y",
            dangerous,
            persistent: false,
        },
        ApprovalOptionRow {
            label: workflow_option_edit_plan(locale),
            key_hint: "2 / e",
            dangerous: false,
            persistent: false,
        },
        ApprovalOptionRow {
            label: workflow_option_cancel(locale),
            key_hint: "3 / Esc",
            dangerous: false,
            persistent: false,
        },
    ]
}

fn approval_options_for_request(
    request: &ApprovalRequest,
    risk: RiskLevel,
    locale: Locale,
) -> Vec<ApprovalOptionRow> {
    if request.tool_name == "workflow" {
        workflow_approval_options(risk, locale).to_vec()
    } else {
        let mut options = approval_options_for(risk, locale).to_vec();
        if request.owner.is_some() {
            // Must match `ApprovalOption::CHILD_ORDER`: no "Stop this turn".
            options.pop();
            return options;
        }
        if request.can_save_allow_rule() {
            options.insert(
                2,
                ApprovalOptionRow {
                    label: tr(locale, MessageId::ApprovalOptionAllowExactRepo),
                    key_hint: "p",
                    dangerous: false,
                    persistent: true,
                },
            );
        }
        options
    }
}

fn workflow_option_approve(locale: Locale) -> Cow<'static, str> {
    match locale {
        Locale::ZhHans => Cow::Borrowed("批准"),
        _ => Cow::Borrowed("Approve"),
    }
}

fn workflow_option_edit_plan(locale: Locale) -> Cow<'static, str> {
    match locale {
        Locale::ZhHans => Cow::Borrowed("编辑计划"),
        _ => Cow::Borrowed("Edit plan"),
    }
}

fn workflow_option_cancel(locale: Locale) -> Cow<'static, str> {
    match locale {
        Locale::ZhHans => Cow::Borrowed("取消"),
        _ => Cow::Borrowed("Cancel"),
    }
}

fn option_approve_once(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalOptionApproveOnce)
}

fn option_approve_always(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalOptionApproveAlways)
}

fn option_deny(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalOptionDeny)
}

fn option_abort(locale: Locale) -> Cow<'static, str> {
    tr(locale, MessageId::ApprovalOptionAbortTurn)
}

// End exact frozen source counterpart; never included in production.
