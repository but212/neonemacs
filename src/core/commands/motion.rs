//! Cursor motion and mark setting commands (§3, §6).

use crate::core::editor::{Editor, LastCommand};
use crate::core::effect::Effects;
use crate::core::width::{column, pos_at_column};

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
    let tab = usize::from(ed.config().tab_width);
    let p = ed.window().point;
    let text = ed.buffer().text();
    let goal = match ed.prev() {
        LastCommand::Vertical { col } => col,
        _ => column(text, p, tab),
    };
    let line = text.line_start(p);
    let target = if down {
        let nl = text.line_end(p);
        (nl < text.len()).then_some(nl + 1)
    } else {
        (line > 0).then(|| text.line_start(line - 1))
    };
    let target_pos = target.map(|start| pos_at_column(text, start, goal, tab));
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
    use crate::test_support::{editor, keys, point, run_command, typ};

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
        let ed = keys(ed, "C-a C-SPC C-e");
        let ed = run_command(ed, exchange_point_and_mark);
        assert_eq!(point(&ed), 0);
        assert_eq!(ed.buffer().mark(), Some(3));
    }
}
