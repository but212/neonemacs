//! Buffer switching/killing and window layout commands (§4, §5).

use crate::core::commands::util::ask;
use crate::core::editor::{Buffer, BufferId, Confirm, Dir, Editor, Window};
use crate::core::effect::Effects;

pub(crate) fn split_window_below(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (split(ed, Dir::Below), fx)
}

pub(crate) fn split_window_right(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (split(ed, Dir::Right), fx)
}

/// Replaces the selected leaf with a split of two windows that show the same buffer.
pub(crate) fn split(ed: Editor, dir: Dir) -> Editor {
    let win = ed.windows().clone().split(dir);
    ed.set_windows(win)
}

pub(crate) fn other_window(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let win = ed.windows().clone().next_window();
    (ed.set_windows(win), fx)
}

pub(crate) fn delete_other_windows(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let win = ed.windows().clone().delete_other_windows();
    (ed.set_windows(win), fx)
}

/// Shows buffer `id` in the selected window. The old buffer keeps its point.
pub fn switch_to(ed: Editor, id: BufferId) -> Editor {
    let old = ed.window().buffer;
    if old == id {
        return ed;
    }
    let point = ed.window().point;
    let ed = ed.with_buffer(old, |b| Buffer {
        saved_point: point,
        ..b
    });
    let target = ed.buffer_by_id(id).saved_point;
    ed.with_window(|w| Window {
        buffer: id,
        point: target,
        top: 0,
        ..w
    })
}

/// Kills buffer `id`. A modified buffer asks first.
pub fn kill_buffer(ed: Editor, id: BufferId) -> Editor {
    if ed.buffer_by_id(id).modified {
        ask(
            ed,
            Confirm::KillModified(id),
            "Buffer is modified; kill anyway? (y or n)",
        )
    } else {
        kill_now(ed, id)
    }
}

/// Removes buffer `id`. Windows that showed it move to another buffer (I-08).
/// Killing the last buffer leaves an empty `*scratch*` (A4).
pub(crate) fn kill_now(ed: Editor, id: BufferId) -> Editor {
    ed.kill_buffer_id(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Mode;
    use crate::core::file::Backing;
    use crate::core::text::Text;
    use crate::test_support::{editor, keys, point, run_command, typ};

    #[test]
    fn windows_keep_independent_points() {
        let ed = typ(editor(), "abcd");
        let ed = run_command(ed, split_window_below);
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
            typ(editor(), "hi").add_buffer("b2", Text::from_bytes(b"xyz"), Backing::None);
        let ed = switch_to(ed, other);
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-e");
        assert_eq!(point(&ed), 3);
        let ed = switch_to(ed, BufferId(0));
        assert_eq!(point(&ed), 2);
        let ed = switch_to(ed, other);
        assert_eq!(point(&ed), 3);

        let ed = kill_buffer(ed, other);
        assert_eq!(ed.buffers().len(), 1);
        assert_eq!(ed.buffer().name, "*scratch*");
    }

    #[test]
    fn killing_last_buffer_makes_scratch() {
        let ed = editor();
        let only = ed.window().buffer;
        let ed = kill_buffer(ed, only);
        assert_eq!(ed.buffers().len(), 1);
        assert_eq!(ed.buffer().name, "*scratch*");
        assert_ne!(ed.buffer().id, only);
    }

    #[test]
    fn kill_modified_buffer_asks() {
        let (ed, other) = editor().add_buffer("b2", Text::default(), Backing::None);
        let ed = switch_to(ed, other);
        let ed = typ(ed, "x");
        let ed = switch_to(ed, BufferId(0));

        let ed = kill_buffer(ed, other);
        assert!(matches!(ed.mode(), Mode::Confirm(Confirm::KillModified(_))));
        let ed = keys(ed, "n");
        assert_eq!(ed.buffers().len(), 2);
        let ed = kill_buffer(ed, other);
        let ed = keys(ed, "y");
        assert_eq!(ed.buffers().len(), 1);
    }
}
