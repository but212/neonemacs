//! Buffer switching/killing and window layout commands (§4, §5).
//!
//! Separated into two layers:
//! - Layer 1: Pure domain logic on buffer IDs, layout transitions, and confirmation predicates.
//! - Layer 2: Built-in command adapters operating on `Editor`.

use crate::core::editor::{BufferId, Confirm, Dir, Editor, Window};
use crate::core::effect::{Effect, Effects};
use crate::core::window::WindowManager;

pub(crate) fn split_window_below(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (split(ed, Dir::Below), fx)
}

pub(crate) fn split_window_right(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (split(ed, Dir::Right), fx)
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

/// Kills buffer `id`. A modified buffer asks first.
#[must_use]
pub fn kill_buffer(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    if ed.buffer_by_id(id).modified() {
        (
            ed.confirm(
                Confirm::KillModified(id),
                "Buffer is modified; kill anyway? (y or n)",
            ),
            fx,
        )
    } else {
        if let Some(eff) = Effect::remove_wal_if_file(ed.buffer_by_id(id)) {
            fx.push(eff);
        }
        (ed.kill_buffer_id(id), fx)
    }
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
        let (ed, other) = editor().add_buffer("b2", Text::default(), Backing::None);
        let ed = switch_to(ed, other);
        let ed = typ(ed, "x");
        let ed = switch_to(ed, BufferId(0));

        let (ed, _) = kill_buffer(ed, other, Vec::new());
        assert!(matches!(ed.mode(), Mode::Confirm(Confirm::KillModified(_))));
        let ed = keys(ed, "n");
        assert_eq!(ed.buffers().len(), 2);
        let (ed, _) = kill_buffer(ed, other, Vec::new());
        let ed = keys(ed, "y");
        assert_eq!(ed.buffers().len(), 1);
    }
}
