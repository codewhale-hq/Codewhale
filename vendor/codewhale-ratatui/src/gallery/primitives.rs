use ratatui::{buffer::Buffer, layout::Rect};

use crate::{
    ChecksMark, ChecksState, Count, KeyHint, MotionMode, Notice, Paint, Role, Skeleton, Theme,
};

use super::Entry;

pub(crate) fn entries() -> Vec<Entry> {
    vec![Entry {
        name: "primitives",
        width: 56,
        height: 10,
        draw: draw_primitives,
    }]
}

fn draw_primitives(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }

    let notice = Notice::warning("Provider rate limit")
        .body("Using fallback route while quota recovers.")
        .action(KeyHint::new("r", "retry"))
        .dismissable(true);
    let notice_h = notice.height(area.width, theme).min(area.height);
    notice.paint(
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: notice_h,
        },
        buf,
        theme,
    );

    let y = area.y.saturating_add(notice_h + 1);
    if y < area.bottom() {
        let skel = Skeleton::text(2).motion(MotionMode::Reduced);
        let skel_h = skel
            .height(area.width, theme)
            .min(area.bottom().saturating_sub(y));
        skel.paint(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: skel_h,
            },
            buf,
            theme,
        );
    }

    let y = area.y.saturating_add(notice_h + 4);
    if y < area.bottom() {
        let count = Count::new(120).max(99).role(Role::Attention);
        count.paint(
            Rect {
                x: area.x,
                y,
                width: area.width.min(6),
                height: 1,
            },
            buf,
            theme,
        );
        let checks = ChecksMark::new(ChecksState::Passing).count(12);
        let x = area.x.saturating_add(6);
        if x < area.right() {
            checks.paint(
                Rect {
                    x,
                    y,
                    width: area.right().saturating_sub(x),
                    height: 1,
                },
                buf,
                theme,
            );
        }
    }
}
