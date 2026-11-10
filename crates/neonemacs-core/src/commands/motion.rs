//! Cursor motion and mark setting commands (§3, §6).

use crate::editor::{Editor, LastCommand, Window};
use crate::effect::Effects;
use crate::text::Text;
use crate::width::{column, pos_at_column};

/// Helper for region lookup on `Editor`.
pub(crate) fn region(ed: &Editor) -> Option<(usize, usize)> {
    ed.buffer()
        .mark()
        .map(|m| (ed.window().point.min(m), ed.window().point.max(m)))
}

pub(crate) fn forward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let next = ed.buffer().text().next_boundary(p);
    (ed.move_point(next), fx)
}

pub(crate) fn backward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let prev = if p > 0 {
        ed.buffer().text().prev_boundary(p)
    } else {
        0
    };
    (ed.move_point(prev), fx)
}

pub(crate) fn move_beginning_of_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let start = ed.buffer().text().line_start(p);
    (ed.move_point(start), fx)
}

pub(crate) fn move_end_of_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let end = ed.buffer().text().line_end(p);
    (ed.move_point(end), fx)
}

pub(crate) fn next_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (vertical(ed, true), fx)
}

pub(crate) fn previous_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (vertical(ed, false), fx)
}

/// Moves to the goal column on the next or previous line.
fn vertical(ed: Editor, down: bool) -> Editor {
    let m = ed.config().metrics();
    let p = ed.window().point;
    let text = ed.buffer().text();
    let goal = match ed.prev() {
        LastCommand::Vertical { col } => col,
        _ => column(text, p, m),
    };
    let line = text.line_start(p);
    let target = if down {
        let nl = text.line_end(p);
        (nl < text.len()).then_some(nl + 1)
    } else {
        (line > 0).then(|| text.line_start(line - 1))
    };
    let target_pos = target.map(|start| pos_at_column(text, start, goal, m));
    let ed = if let Some(pos) = target_pos {
        ed.move_point(pos)
    } else {
        let msg = if down {
            "End of buffer"
        } else {
            "Beginning of buffer"
        };
        ed.set_message(msg)
    };
    ed.set_last(LastCommand::Vertical { col: goal })
}

/// Start of the line `n` lines before the line holding `start`; stops at the first line.
fn lines_back(text: &Text, start: usize, n: usize) -> usize {
    let mut s = text.line_start(start);
    for _ in 0..n {
        if s == 0 {
            break;
        }
        s = text.line_start(s - 1);
    }
    s
}

/// `recenter` (C-l): puts the line holding point on the middle row. Text and point are unchanged.
pub(crate) fn recenter(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let half = ed.window().height / 2;
    let text = ed.buffer().text();
    let top = lines_back(text, ed.window().point, half);
    (ed.with_window(|w| Window { top, ..w }), fx)
}

pub(crate) fn set_mark_command(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let ed = ed
        .with_current_buffer(|b| b.set_mark(Some(p)))
        .set_message("Mark set");
    (ed, fx)
}

pub(crate) fn exchange_point_and_mark(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some(mark) = ed.buffer().mark() else {
        return (ed.set_message("No mark set in this buffer"), fx);
    };
    let p = ed.window().point;
    let ed = ed.with_current_buffer(|b| b.set_mark(Some(p)));
    (ed.move_point(mark), fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{editor, keys, point, run_command, set_mark, text, typ};

    #[test]
    fn the_mark_is_active_until_an_edit_cancel_or_copy() {
        // I-47
        let ed = set_mark(keys(typ(editor(), "abc"), "C-a"));
        assert!(ed.buffer().mark_active());

        // An edit keeps the mark position but ends the region.
        let edited = typ(ed.clone(), "x");
        assert!(!edited.buffer().mark_active());
        assert_eq!(edited.buffer().mark(), Some(0));

        // C-g ends it too, and a copy (M-w) does.
        let quit = keys(ed.clone(), "C-g");
        assert!(!quit.buffer().mark_active());
        assert_eq!(quit.buffer().mark(), Some(0));
        let copied = keys(keys(ed, "C-e"), "M-w");
        assert!(!copied.buffer().mark_active());
        assert_eq!(copied.buffer().mark(), Some(0));
        assert_eq!(text(&copied), "abc");

        // Setting the mark again makes it active again.
        assert!(set_mark(copied).buffer().mark_active());
    }

    #[test]
    fn motion_hangul_moves_by_character() {
        let ed = typ(editor(), "가나다");
        assert_eq!(point(&ed), 9);
        let ed = keys(ed, "C-b");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "C-b C-b");
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-f");
        assert_eq!(point(&ed), 3);
        let ed = keys(ed, "C-e");
        assert_eq!(point(&ed), 9);
        let ed = keys(ed, "C-a");
        assert_eq!(point(&ed), 0);
    }

    #[test]
    fn motion_vertical_keeps_goal_column() {
        // Lines: "ab가" (bytes 0..5, columns 0..4), "cd" (bytes 6..8), "" (byte 9).
        let ed = typ(editor(), "ab가\ncd\n");
        assert_eq!(point(&ed), 9);
        let ed = keys(ed, "C-p");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "C-e C-p");
        // Goal column 2 is the start of "가", which is two columns wide.
        assert_eq!(point(&ed), 2);
        let ed = keys(ed, "C-n");
        // The goal survives the vertical move; "cd" is only two columns wide, so it clamps.
        assert_eq!(point(&ed), 8);
    }

    #[test]
    fn exchange_point_and_mark_swaps() {
        let ed = typ(editor(), "abc");
        let ed = set_mark(keys(ed, "C-a"));
        let ed = keys(ed, "C-e");
        let ed = run_command(ed, exchange_point_and_mark);
        assert_eq!(point(&ed), 0);
        assert_eq!(ed.buffer().mark(), Some(3));
    }

    fn numbered(n: usize) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        for i in 0..n {
            writeln!(s, "l{i}").unwrap();
        }
        s
    }

    /// Byte offset of line `i` in `numbered`: three bytes per line below 10, four from 10.
    fn ls(i: usize) -> usize {
        if i < 10 { 3 * i } else { 30 + 4 * (i - 10) }
    }

    fn with_height(ed: Editor, height: usize) -> Editor {
        ed.with_window(|w| Window { height, ..w })
    }

    fn top(ed: &Editor) -> usize {
        ed.window().top
    }

    #[test]
    fn recenter_is_safe_before_first_render() {
        let ed = typ(editor(), &numbered(5));
        assert_eq!(ed.window().height, 0);
        let ed = run_command(ed, recenter);
        // With no known height the point's own line becomes the top.
        assert_eq!(top(&ed), ed.buffer().text().line_start(point(&ed)));
    }

    #[test]
    fn recenter_puts_point_line_on_middle_row_without_editing() {
        let ed = with_height(typ(editor(), &numbered(30)), 10);
        let ed = ed.with_window(|w| Window {
            point: 0,
            top: 0,
            ..w
        });
        let ed = ed.with_window(|w| Window { point: ls(20), ..w });
        let before = ed.buffer().text().len();
        let ed = run_command(ed, recenter);
        assert_eq!(top(&ed), ls(15));
        assert_eq!(point(&ed), ls(20));
        assert_eq!(ed.buffer().text().len(), before);
    }

    #[test]
    fn recenter_clamps_to_buffer_start() {
        let ed = with_height(typ(editor(), &numbered(30)), 10);
        let ed = ed.with_window(|w| Window { point: 3 * 2, ..w });
        let ed = run_command(ed, recenter);
        assert_eq!(top(&ed), 0);
    }
}
