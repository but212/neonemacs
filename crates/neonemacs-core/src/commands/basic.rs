//! Basic editing, cursor motion, buffer switching, and window layout commands (§3, §4, §5, §6).

use crate::editor::{Buffer, BufferId, Confirm, Dir, Editor, LastCommand, Window};
use crate::effect::Effects;
use crate::width::{column, pos_and_column};
use crate::window::WindowManager;
use crate::{Doc, line_end, line_start, line_to_offset, offset_to_line, total_lines};

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
    let start = line_start(ed.buffer().text(), ed.window().point);
    (ed.move_point(start), fx)
}

pub(crate) fn move_end_of_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let end = line_end(ed.buffer().text(), ed.window().point);
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
    let text = ed.buffer().text();
    let p = text.floor_boundary(ed.window().point);
    let goal = match ed.prev() {
        LastCommand::Vertical { col } => col,
        _ => column(text, p, m),
    };
    let line = offset_to_line(text, p).expect("floor_boundary is valid");
    let target = if down {
        (line + 1 < total_lines(text))
            .then(|| line_to_offset(text, line + 1).expect("line + 1 < total_lines"))
    } else {
        (line > 0).then(|| line_to_offset(text, line - 1).expect("line - 1 < total_lines"))
    };
    let target_pos = target.map(|start| pos_and_column(text, start, goal, m).0);
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

/// `recenter` (C-l): puts the line holding point on the middle row. Text and point are unchanged.
pub(crate) fn recenter(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let half = ed.window().height / 2;
    let text = ed.buffer().text();
    let line = crate::line_at(text, ed.window().point);
    let top = line_to_offset(text, line.saturating_sub(half)).expect("line < total_lines");
    (ed.with_window(|w| Window { top, ..w }), fx)
}

pub(crate) fn set_mark_command(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let ed = ed
        .with_current_buffer(|b| b.set_mark(Some(p)))
        .set_message("Mark set");
    (ed, fx)
}

pub(crate) fn delete_backward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    if p == 0 {
        return (ed, fx);
    }
    let start = ed.buffer().text().prev_boundary(p);
    (ed.delete_only(start, p), fx)
}

pub(crate) fn delete_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let text = ed.buffer().text();
    if p >= text.total_bytes() {
        return (ed, fx);
    }
    let end = text.next_boundary(p);
    (ed.delete_only(p, end), fx)
}

fn with_region(
    ed: Editor,
    fx: Effects,
    f: impl FnOnce(Editor, Effects, usize, usize) -> (Editor, Effects),
) -> (Editor, Effects) {
    let Some((start, end)) = region(&ed) else {
        return (ed.set_message("The mark is not set now"), fx);
    };
    f(ed, fx, start, end)
}

pub(crate) fn kill_region(ed: Editor, fx: Effects) -> (Editor, Effects) {
    with_region(ed, fx, |ed, fx, start, end| {
        if start >= end {
            return (ed, fx);
        }
        let ed = ed.kill_range(start, end);
        let ed = ed.with_current_buffer(|b| b.set_mark(None));
        (ed, fx)
    })
}

pub(crate) fn kill_ring_save(ed: Editor, fx: Effects) -> (Editor, Effects) {
    with_region(ed, fx, |ed, fx, start, end| {
        let text = ed.buffer().text();
        let start = text.floor_boundary(start);
        let end = text.floor_boundary(end);
        let doc = text.slice(start..end).unwrap_or_default();
        let max = ed.config().kill_ring_max;
        let ed = ed
            .with_kill_ring(|kr| kr.push(doc, max))
            .with_current_buffer(Buffer::deactivate_mark);
        (ed, fx)
    })
}

pub(crate) fn yank(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some(doc) = ed.kill_ring().last().cloned() else {
        return (ed.set_message("Kill ring is empty"), fx);
    };
    let p = ed.buffer().text().floor_boundary(ed.window().point);
    let len = doc.total_bytes();
    let (ed, applied) = ed.apply(crate::undo::Edit::replace(p, 0, doc));
    let ed = if applied.is_some() {
        ed.move_point(p + len).set_last(LastCommand::Yank {
            start: p,
            end: p + len,
        })
    } else {
        ed
    };
    (ed, fx)
}

/// Replaces the last yank with the previous kill-ring entry. Valid only right after `yank`.
pub(crate) fn yank_pop(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let LastCommand::Yank { start, end } = ed.prev() else {
        return (ed.set_message("Previous command was not a yank"), fx);
    };
    let text = ed.buffer().text();
    if start > end || !text.is_boundary(start) || !text.is_boundary(end) {
        return (
            ed.set_message("Yank merged with neighboring UTF-8 bytes"),
            fx,
        );
    }
    let entries = ed.kill_ring().entries();
    let next_doc = match entries.len() {
        0 => Doc::default(),
        1 => entries[0].clone(),
        n => entries[n - 2].clone(),
    };
    let len = next_doc.total_bytes();
    let (ed, applied) = ed.apply(crate::undo::Edit::replace(start, end - start, next_doc));
    let ed = if applied.is_some() {
        ed.with_kill_ring(|kr| kr.rotate_right(1))
            .move_point(start + len)
            .set_last(LastCommand::Yank {
                start,
                end: start + len,
            })
    } else {
        ed
    };
    (ed, fx)
}

pub(crate) fn view_mode(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let on = !ed.buffer().view_only();
    let ed = ed.with_current_buffer(|b| b.set_view_only(on));
    let ed = ed.set_message(if on { "View mode on" } else { "View mode off" });
    (ed, fx)
}

/// Replaces the selected leaf with a split of two windows that show the same buffer.
pub(crate) fn split(ed: Editor, dir: Dir) -> Editor {
    ed.with_windows(|wm| wm.split(dir))
}

pub(crate) fn other_window(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (ed.with_windows(WindowManager::next_window), fx)
}

pub(crate) fn delete_other_windows(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (ed.with_windows(WindowManager::delete_other_windows), fx)
}

/// Shows buffer `id` in the selected window. The old buffer keeps its point.
#[must_use]
pub fn switch_to(ed: Editor, id: BufferId) -> Editor {
    let old = ed.window().buffer;
    if old == id {
        return ed;
    }
    let point = ed.window().point;
    let ed = ed.with_buffer(old, |b| b.set_saved_point(point));
    let target = ed.buffer_by_id(id).saved_point();
    ed.with_window(|w| Window {
        buffer: id,
        point: target,
        top: 0,
        ..w
    })
}

pub(crate) fn kill_buffer_now(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    fx.extend(ed.remove_wal_effect(id));
    (ed.kill_buffer_id(id), fx)
}

/// Kills buffer `id`. A modified buffer asks first.
#[must_use]
pub fn kill_buffer(ed: Editor, id: BufferId, fx: Effects) -> (Editor, Effects) {
    if ed.buffer_by_id(id).modified() {
        (
            ed.confirm(
                Confirm::KillModified(id),
                "Buffer is modified; kill anyway? (y or n)",
            ),
            fx,
        )
    } else {
        kill_buffer_now(ed, id, fx)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::editor::{KillRing, Mode};
    use crate::file::Backing;
    use crate::test_support::{editor, keys, point, run_command, set_mark, text, typ};
    use crate::{Doc, line_to_offset, offset_to_line};

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

    fn numbered(n: usize) -> String {
        use core::fmt::Write;
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
        let t = ed.buffer().text();
        let line = offset_to_line(t, t.floor_boundary(point(&ed))).unwrap();
        assert_eq!(top(&ed), line_to_offset(t, line).unwrap());
    }

    #[test]
    fn recenter_puts_point_line_on_middle_row_and_clamps_to_start() {
        let base = with_height(typ(editor(), &numbered(30)), 10);
        let ed = base.clone().with_window(|w| Window {
            point: ls(20),
            top: 0,
            ..w
        });
        let before = ed.buffer().text().total_bytes();
        let ed = run_command(ed, recenter);
        assert_eq!(top(&ed), ls(15));
        assert_eq!(point(&ed), ls(20));
        assert_eq!(ed.buffer().text().total_bytes(), before);

        let ed = base.with_window(|w| Window { point: 3 * 2, ..w });
        let ed = run_command(ed, recenter);
        assert_eq!(top(&ed), 0);
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
        let ed = set_mark(keys(ed, "C-a"));
        let ed = keys(ed, "C-f C-f");
        let ed = keys(ed, "C-w");
        assert_eq!(text(&ed), "c");
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-y");
        assert_eq!(text(&ed), "abc");
        assert_eq!(point(&ed), 2);
    }

    #[test]
    fn kill_ring_yank_pop_cycles() {
        let ed = editor().with_kill_ring(|_| {
            KillRing::from_entries(vec![Doc::from_bytes(b"A"), Doc::from_bytes(b"B")])
        });
        let ed = keys(ed, "C-y");
        assert_eq!(text(&ed), "B");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "A");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "B");
    }

    #[test]
    fn yank_pop_refuses_to_delete_bytes_outside_the_yank() {
        for (original, pos, inserted) in [(&[0xC3][..], 1, 0xA9), (&[0xA9][..], 0, 0xC3)] {
            let entries = vec![Doc::from_bytes(b"hello"), Doc::from_bytes(&[inserted])];
            let ed = editor()
                .with_current_buffer(|b| b.with_text(Doc::from_bytes(original)))
                .move_point(pos)
                .with_kill_ring(|_| KillRing::from_entries(entries.clone()));
            let ed = run_command(ed, yank);
            assert_eq!(text(&ed), "é");
            let yank_point = point(&ed);
            let seq = ed.edit_seq();
            let ed = run_command(ed, yank_pop);
            assert_eq!(text(&ed), "é");
            assert_eq!(point(&ed), yank_point);
            assert_eq!(ed.edit_seq(), seq);
            assert_eq!(ed.kill_ring().entries(), &entries);
            assert_eq!(ed.message(), "Yank merged with neighboring UTF-8 bytes");
        }
    }

    #[test]
    fn yank_and_yank_pop_on_view_only_leave_kill_ring_and_last_unmutated() {
        let initial_ring = [Doc::from_bytes(b"A"), Doc::from_bytes(b"B")];
        let ed = editor()
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"abc")).set_view_only(true))
            .with_kill_ring(|_| KillRing::from_entries(initial_ring.to_vec()));

        let ed = run_command(ed, yank);
        assert_eq!(text(&ed), "abc");
        assert_eq!(point(&ed), 0);
        assert_eq!(ed.message(), "Buffer is read-only");
        assert_eq!(ed.last(), LastCommand::Other);
        assert_eq!(ed.kill_ring().entries(), &initial_ring);

        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "abc");
        assert_eq!(ed.message(), "Previous command was not a yank");
        assert_eq!(ed.last(), LastCommand::Other);
        assert_eq!(ed.kill_ring().entries(), &initial_ring);

        // Even if buffer becomes read-only right after a successful yank, yank-pop must not rotate.
        let ed = editor()
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"abc")))
            .with_kill_ring(|_| KillRing::from_entries(initial_ring.to_vec()));
        let ed = run_command(ed, yank).with_current_buffer(|b| b.set_view_only(true));
        assert_eq!(text(&ed), "Babc");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "Babc");
        assert_eq!(ed.message(), "Buffer is read-only");
        assert_eq!(ed.last(), LastCommand::Other);
        assert_eq!(ed.kill_ring().entries(), &initial_ring);
    }

    #[test]
    fn windows_keep_independent_points() {
        let ed = typ(editor(), "abcd");
        let ed = run_command(ed, |e, fx| (split(e, Dir::Below), fx));
        assert_eq!(ed.leaf_count(), 2);
        let ed = ed.with_window(|w| Window { point: 3, ..w });
        let ed = run_command(ed, other_window);
        assert_eq!(point(&ed), 4);
        let ed = run_command(ed, delete_other_windows);
        assert_eq!(ed.leaf_count(), 1);
        assert_eq!(point(&ed), 4);
    }

    #[test]
    fn buffers_remember_point_and_kill_falls_back() {
        let (ed, other) =
            typ(editor(), "hi").add_buffer("b2", Doc::from_bytes(b"xyz"), Backing::None);
        let ed = switch_to(ed, other);
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-e");
        assert_eq!(point(&ed), 3);
        let ed = switch_to(ed, BufferId(0));
        assert_eq!(point(&ed), 2);
        let ed = switch_to(ed, other);
        assert_eq!(point(&ed), 3);

        let (ed, _) = kill_buffer(ed, other, Vec::new());
        assert_eq!(ed.buffers().len(), 1);
        assert_eq!(ed.buffer().name(), "*scratch*");
    }

    #[test]
    fn killing_last_buffer_makes_scratch() {
        let ed = editor();
        let only = ed.window().buffer;
        let (ed, _) = kill_buffer(ed, only, Vec::new());
        assert_eq!(ed.buffers().len(), 1);
        assert_eq!(ed.buffer().name(), "*scratch*");
        assert_ne!(ed.buffer().id(), only);
    }

    #[test]
    fn kill_modified_buffer_asks() {
        let (ed, other) = editor().add_buffer("b2", Doc::default(), Backing::None);
        let ed = switch_to(ed, other);
        let ed = typ(ed, "x");
        let ed = switch_to(ed, BufferId(0));

        let (ed, _) = kill_buffer(ed, other, Vec::new());
        assert!(matches!(
            ed.mode(),
            Mode::Confirm { kind: Confirm::KillModified(_), .. }
        ));
        let ed = keys(ed, "n");
        assert_eq!(ed.buffers().len(), 2);
        let (ed, _) = kill_buffer(ed, other, Vec::new());
        let ed = keys(ed, "y");
        assert_eq!(ed.buffers().len(), 1);
    }
}
