//! Viewport scrolling and window rectangle layout (§5, §11, A5).
//! `scroll` adjusts each window's `top` and `left_col` so point stays visible, and warms
//! multi-line syntax state caches for the visible viewport.

use crate::config::Toggles;
use crate::editor::{Dir, Editor, Layout, Window};
use crate::width::{Metrics, column};
use crate::{Doc, line_to_offset, offset_to_line};
use alloc::vec;
use alloc::vec::Vec;

pub const HUGE_FILE_SYNTAX_BYTES: usize = 100 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

fn rects(layout: &Layout, rect: Rect) -> Vec<Rect> {
    match layout {
        Layout::Leaf(_) => vec![rect],
        Layout::Split { dir, first, second } => {
            let (first_rect, second_rect) = match dir {
                Dir::Below => {
                    let top = rect.h.div_ceil(2);
                    (
                        Rect { h: top, ..rect },
                        Rect {
                            y: rect.y + top,
                            h: rect.h - top,
                            ..rect
                        },
                    )
                }
                Dir::Right => {
                    let left = rect.w.div_ceil(2);
                    (
                        Rect { w: left, ..rect },
                        Rect {
                            x: rect.x + left,
                            w: rect.w - left,
                            ..rect
                        },
                    )
                }
            };
            let mut out = rects(first, first_rect);
            out.extend(rects(second, second_rect));
            out
        }
    }
}

/// Screen area of every window, in layout order. The last row is the echo area.
#[must_use]
pub fn areas(ed: &Editor, cols: usize, rows: usize) -> Vec<Rect> {
    let body = Rect {
        x: 0,
        y: 0,
        w: cols,
        h: rows.saturating_sub(1),
    };
    rects(ed.windows().layout(), body)
}

/// Moves each window's `top` and `left_col` so that its point is visible, and warms
/// multi-line syntax state caches for the visible viewport.
#[must_use]
pub fn scroll(ed: Editor, cols: usize, rows: usize) -> Editor {
    let m = ed.config().metrics();
    let highlight_on = ed.config().toggles.contains(Toggles::SYNTAX_HIGHLIGHT);
    let win_areas: Vec<(Window, Rect)> = ed
        .windows()
        .windows()
        .into_iter()
        .copied()
        .zip(areas(&ed, cols, rows))
        .collect();
    let scrolled: Vec<(Window, Rect)> = win_areas
        .into_iter()
        .map(|(w, area)| {
            (
                scroll_window(ed.buffer_by_id(w.buffer).text(), w, area, m),
                area,
            )
        })
        .collect();
    let mut ed = ed.with_windows(|mut win| {
        for (i, (w, _)) in scrolled.iter().enumerate() {
            win = win.set_leaf(i, *w);
        }
        win
    });
    if highlight_on {
        for (w, area) in scrolled {
            let buf = ed.buffer_by_id(w.buffer);
            let huge = buf.text().total_bytes() >= HUGE_FILE_SYNTAX_BYTES;
            let syntax = ed.syntax_registry().find_by_filename(buf.name()).cloned();
            if !huge
                && let Some(def) = syntax
                && def.heading_prefix.is_none()
                && (def.block_comment.is_some() || def.raw_strings)
            {
                let first_line = crate::line_at(buf.text(), w.top);
                let through = first_line.saturating_add(area.h.saturating_sub(1));
                ed = ed.with_buffer(w.buffer, |b| b.ensure_syntax_cache(&def, through));
            }
        }
    }
    ed
}

fn scroll_window(text: &Doc, mut w: Window, area: Rect, m: Metrics) -> Window {
    let height = area.h.saturating_sub(1).max(1);
    w.height = u32::try_from(height).unwrap_or(u32::MAX);
    let width = area.w.max(1);
    let total = text.total_bytes();
    let p = text.floor_boundary(w.point.min(total));
    let p_line = offset_to_line(text, p).expect("floor_boundary is valid");
    let top_line = crate::line_at(text, w.top);
    if p < w.top || p_line.saturating_sub(top_line) >= height {
        w.top = line_to_offset(text, p_line).expect("p_line < total_lines");
    }
    let col = column(text, p, m);
    if col < w.left_col {
        w.left_col = col;
    } else if col >= w.left_col + width {
        w.left_col = col + 1 - width;
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn scroll_records_body_height_per_window() {
        let ed = Editor::new(Config::default());
        assert_eq!(ed.window().height, 0);
        let ed = scroll(ed, 20, 6);
        assert_eq!(ed.window().height, 4);
        let ed = scroll(ed.with_windows(|wm| wm.split(Dir::Below)), 20, 10);
        assert!(ed.windows().windows().iter().all(|w| w.height >= 1));
    }
}
