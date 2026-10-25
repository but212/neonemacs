//! Text modification, kill ring, and formatting commands (§6).
//!
//! Separated into two layers:
//! - Layer 1: Pure domain calculations on `Text` and positions.
//! - Layer 2: Built-in command adapters operating on `Editor`.

use crate::core::commands::motion::region;
use crate::core::editor::{Editor, LastCommand};
use crate::core::effect::Effects;
use crate::core::text::Text;

// --- Layer 1: Pure domain primitives ---

/// Computes the delete range for backward deletion from `point`.
pub fn delete_backward_char_range(text: &Text, point: usize) -> Option<(usize, usize)> {
    if point == 0 {
        None
    } else {
        Some((text.prev_boundary(point), point))
    }
}

/// Computes the delete range for forward deletion from `point`.
pub fn delete_char_range(text: &Text, point: usize) -> Option<(usize, usize)> {
    if point >= text.len() {
        None
    } else {
        Some((point, text.next_boundary(point)))
    }
}

/// Computes the range to kill from `point` to the end of the line (or newline).
pub fn kill_line_range(text: &Text, point: usize) -> Option<(usize, usize)> {
    let line_end = text.line_end(point);
    let end = if line_end == point && point < text.len() {
        point + 1
    } else {
        line_end
    };
    if point >= end {
        None
    } else {
        Some((point, end))
    }
}

// --- Layer 2: Built-in command adapters ---

pub(crate) fn delete_backward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let Some((start, end)) = delete_backward_char_range(ed.buffer().text(), p) else {
        return (ed, fx);
    };
    (ed.delete_range(start, end).0, fx)
}

pub(crate) fn delete_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let Some((start, end)) = delete_char_range(ed.buffer().text(), p) else {
        return (ed, fx);
    };
    (ed.delete_range(start, end).0, fx)
}

pub(crate) fn kill_region(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some((start, end)) = region(&ed) else {
        return (ed.set_message("The mark is not set now"), fx);
    };
    if start >= end {
        return (ed, fx);
    }
    let ed = ed.kill_range(start, end);
    let ed = ed.with_current_buffer(|b| b.set_mark(None));
    (ed, fx)
}

pub(crate) fn kill_ring_save(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some((start, end)) = region(&ed) else {
        return (ed.set_message("The mark is not set now"), fx);
    };
    let bytes = ed.buffer().text().bytes(start..end);
    let ed = ed.with_kill_ring(|kr| kr.push(bytes));
    (ed, fx)
}

/// Kills to the end of the line, or the newline when already at the end.
pub(crate) fn kill_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let Some((start, end)) = kill_line_range(ed.buffer().text(), p) else {
        return (ed, fx);
    };
    (ed.kill_range(start, end), fx)
}

pub(crate) fn yank(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some(bytes) = ed.kill_ring().last().map(<[u8]>::to_vec) else {
        return (ed.set_message("Kill ring is empty"), fx);
    };
    let p = ed.window().point;
    let ed = ed.insert_at(p, &bytes);
    let ed = ed.set_last(LastCommand::Yank {
        start: p,
        end: p + bytes.len(),
    });
    (ed, fx)
}

/// Replaces the last yank with the previous kill-ring entry. Valid only right after `yank`.
pub(crate) fn yank_pop(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let LastCommand::Yank { start, end } = ed.prev() else {
        return (ed.set_message("Previous command was not a yank"), fx);
    };
    let (ed, _) = ed.delete_range(start, end);
    let ed = ed.with_kill_ring(|kr| kr.rotate_right(1));
    let bytes = ed
        .kill_ring()
        .last()
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    let ed = ed.insert_at(start, &bytes);
    let ed = ed.set_last(LastCommand::Yank {
        start,
        end: start + bytes.len(),
    });
    (ed, fx)
}

pub(crate) fn view_mode(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let on = !ed.buffer().view_only();
    let ed = ed.with_current_buffer(|b| b.set_view_only(on));
    let ed = ed.set_message(if on { "View mode on" } else { "View mode off" });
    (ed, fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::KillRing;
    use crate::test_support::{editor, keys, point, run_command, text, typ};

    #[test]
    fn insert_hangul_saves_utf8() {
        let ed = typ(editor(), "한");
        assert_eq!(ed.buffer().text().to_bytes(), "한".as_bytes());
    }

    #[test]
    fn motion_delete_hangul() {
        let ed = typ(editor(), "가나다");
        let ed = keys(ed, "DEL");
        assert_eq!(text(&ed), "가나");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "C-a C-d");
        assert_eq!(text(&ed), "나");
    }

    #[test]
    fn kill_ring_region_and_yank() {
        let ed = typ(editor(), "abc");
        let ed = keys(ed, "C-a C-SPC C-f C-f");
        let ed = keys(ed, "C-w");
        assert_eq!(text(&ed), "c");
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-y");
        assert_eq!(text(&ed), "abc");
        assert_eq!(point(&ed), 2);
    }

    #[test]
    fn kill_ring_consecutive_kills_append() {
        let ed = typ(editor(), "one\ntwo");
        let ed = keys(ed, "C-p C-a");
        assert_eq!(point(&ed), 0);
        let ed = run_command(ed, kill_line);
        let ed = run_command(ed, kill_line);
        assert_eq!(text(&ed), "two");
        assert_eq!(ed.kill_ring().entries(), &[b"one\n".to_vec()]);
    }

    #[test]
    fn kill_ring_yank_pop_cycles() {
        let ed =
            editor().with_kill_ring(|_| KillRing::from_entries(vec![b"A".to_vec(), b"B".to_vec()]));
        let ed = keys(ed, "C-y");
        assert_eq!(text(&ed), "B");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "A");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "B");
    }
}
