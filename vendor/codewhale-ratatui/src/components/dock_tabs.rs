//! Native work-dock tab fitting and painting. The host supplies availability,
//! counts, actual close copy and interaction state; this owns no focus policy.

use super::{WorkbarPanel, WorkbarTab};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockTabTarget {
    Panel(WorkbarPanel),
    Close,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DockTabStyles {
    pub idle: Style,
    pub active: Style,
    pub hovered: Style,
    pub close: Style,
    pub close_hovered: Style,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockTabEntry {
    pub tab: WorkbarTab,
    pub area: Rect,
    pub label: String,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockTabPlan {
    pub tabs: Vec<DockTabEntry>,
    pub close_area: Rect,
    pub close: String,
    close_style: Style,
}

impl DockTabPlan {
    #[must_use]
    pub fn hitboxes(&self) -> Vec<(DockTabTarget, Rect)> {
        let mut boxes = self
            .tabs
            .iter()
            .filter(|tab| !tab.area.is_empty())
            .map(|tab| (DockTabTarget::Panel(tab.tab.panel), tab.area))
            .collect::<Vec<_>>();
        if !self.close_area.is_empty() {
            boxes.push((DockTabTarget::Close, self.close_area));
        }
        boxes
    }

    /// Paint this exact plan; hitboxes and painted cells share one snapshot.
    pub fn paint(&self, buf: &mut Buffer) {
        for tab in &self.tabs {
            Paragraph::new(Line::from(Span::styled(tab.label.as_str(), tab.style)))
                .render(tab.area, buf);
        }
        Paragraph::new(Line::from(Span::styled(
            self.close.clone(),
            self.close_style,
        )))
        .render(self.close_area, buf);
    }
}

/// One native row. All styles are supplied by the live host theme, including
/// backgrounds and modifiers. Close copy describes the host's actual action.
#[derive(Clone, Debug)]
pub struct DockTabRow<'a> {
    pub tabs: &'a [WorkbarTab],
    pub active: WorkbarPanel,
    pub bottom: bool,
    pub close: Cow<'a, str>,
    pub hovered: Option<DockTabTarget>,
    pub pressed: Option<WorkbarPanel>,
    pub styles: DockTabStyles,
}

impl DockTabRow<'_> {
    #[must_use]
    pub fn plan(&self, area: Rect) -> DockTabPlan {
        let width = usize::from(area.width);
        let safe_close = crate::text::display_safe(self.close.as_ref()).into_owned();
        let close_width =
            unicode_width::UnicodeWidthStr::width(safe_close.as_str()).min(width) as u16;
        let y = if self.bottom {
            area.y
                .saturating_add(1)
                .min(area.bottom().saturating_sub(1))
        } else {
            area.y
        };
        let close_area = Rect::new(
            area.right().saturating_sub(close_width),
            y,
            close_width,
            u16::from(!area.is_empty()),
        );
        let mut plan = DockTabPlan {
            tabs: Vec::new(),
            close_area,
            close: safe_close,
            close_style: if self.hovered == Some(DockTabTarget::Close) {
                self.styles.close_hovered
            } else {
                self.styles.close
            },
        };
        if area.is_empty() {
            return plan;
        }
        let mut entries = WorkbarPanel::ORDER
            .into_iter()
            .filter_map(|panel| self.tabs.iter().find(|tab| tab.panel == panel).cloned())
            .collect::<Vec<_>>();
        if !entries.iter().any(|tab| tab.panel == self.active) {
            entries.push(WorkbarTab::new(self.active));
            entries.sort_by_key(|tab| {
                WorkbarPanel::ORDER
                    .iter()
                    .position(|panel| *panel == tab.panel)
            });
        }
        let fits = |tabs: &[WorkbarTab], counts: bool| {
            tabs.iter()
                .map(|tab| {
                    unicode_width::UnicodeWidthStr::width(tab.panel.label())
                        + if counts && tab.count.is_some_and(|n| n > 0) {
                            1 + tab.count.unwrap_or(0).to_string().len()
                        } else {
                            0
                        }
                        + 2
                })
                .sum::<usize>()
                .saturating_add(tabs.len().saturating_sub(1).saturating_mul(2))
                .saturating_add(usize::from(close_width) + 2)
                <= width
        };
        let show_counts = fits(&entries, true);
        while !fits(&entries, show_counts) && entries.len() > 1 {
            let Some(index) = entries.iter().rposition(|tab| tab.panel != self.active) else {
                break;
            };
            entries.remove(index);
        }
        let mut x = area.x.saturating_add(1);
        for tab in entries {
            let label = if show_counts && tab.count.is_some_and(|n| n > 0) {
                format!("{} {}", tab.panel.label(), tab.count.unwrap_or(0))
            } else {
                tab.panel.label().to_string()
            };
            let tab_width = u16::try_from(
                unicode_width::UnicodeWidthStr::width(label.as_str()).saturating_add(2),
            )
            .unwrap_or(u16::MAX)
            .min(area.width);
            let style = if tab.panel == self.active || self.pressed == Some(tab.panel) {
                self.styles.active
            } else if self.hovered == Some(DockTabTarget::Panel(tab.panel)) {
                self.styles.hovered
            } else {
                self.styles.idle
            };
            if x.saturating_add(tab_width) > close_area.x {
                if plan.tabs.is_empty() && tab.panel == self.active {
                    let avail = close_area.x.saturating_sub(x);
                    if avail > 0 {
                        let rendered = if avail >= 3 {
                            format!(
                                " {} ",
                                crate::text::truncate(
                                    label.as_str(),
                                    usize::from(avail - 2),
                                    false
                                )
                            )
                        } else {
                            crate::text::truncate(label.as_str(), usize::from(avail), false)
                                .into_owned()
                        };
                        plan.tabs.push(DockTabEntry {
                            tab,
                            area: Rect::new(x, y, avail, 1),
                            label: rendered,
                            style,
                        });
                    }
                }
                break;
            }
            plan.tabs.push(DockTabEntry {
                tab,
                area: Rect::new(x, y, tab_width, 1),
                label: format!(" {label} "),
                style,
            });
            x = x.saturating_add(tab_width).saturating_add(2);
        }
        plan
    }
}

impl Widget for &DockTabRow<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.plan(area).paint(buf);
    }
}
