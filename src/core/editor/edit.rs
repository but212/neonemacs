//! The edit pipeline (§7, I-01, I-06, I-09, I-38). `apply` is the single edit point: every change
//! to a buffer goes through it, so the read-only check, undo, and anchor moves stay in one place.

use super::{Buffer, Editor, LastCommand, Window, snap, snap_end};
use crate::core::effect::Effects;
use crate::core::undo::Edit;

/// Typed characters that share one undo group.
const TYPED_GROUP: usize = 20;

impl Editor {
    /// Applies `edit` to the buffer of the selected window. Returns the new editor and
    /// the number of undo groups dropped to respect the limit, or `None` when refused.
    #[must_use]
    pub fn apply(mut self, edit: Edit) -> (Self, Option<usize>) {
        let id = self.window().buffer;
        if self.buffer_by_id(id).view_only() {
            self.interaction.message = "Buffer is read-only".to_owned();
            return (self, None);
        }

        // The yank offsets describe the text before this edit, so the yank ends here (I-38).
        if matches!(self.interaction.last, LastCommand::Yank { .. }) {
            self.interaction.last = LastCommand::Other;
        }

        let mut dropped_res = None;
        let undo_limit = self.settings.config.undo_limit;
        self.documents.edit_seq = self.documents.edit_seq.wrapping_add(1);
        self.documents.buffers = self.documents.buffers.with_buffer(id, |buf| {
            let (buf, dropped) = buf.apply_edit(edit.clone(), |a| remap(&edit, a), undo_limit);
            dropped_res = Some(dropped);
            buf
        });

        let text = self.documents.buffers.get_by_id(id).text();
        let anchor = |a| snap(text, remap(&edit, a));
        self.documents.windows = self.documents.windows.map_windows(&|w| {
            if w.buffer == id {
                Window {
                    point: anchor(w.point),
                    top: anchor(w.top),
                    ..w
                }
            } else {
                w
            }
        });

        (self, dropped_res)
    }

    /// Deletes `start..end`. Returns the editor and the removed bytes.
    #[must_use]
    pub fn delete_range(self, start: usize, end: usize) -> (Self, Vec<u8>) {
        let text = self.buffer().text();
        let len = text.len();
        let start = snap(text, start.min(len));
        let end = snap_end(text, end.min(len));
        if start >= end {
            return (self, Vec::new());
        }
        let bytes = self.buffer().text().bytes(start..end);
        let (ed, _) = self.apply(Edit::Delete {
            pos: start,
            bytes: bytes.clone(),
        });
        (ed, bytes)
    }

    /// Deletes `start..end` and pushes the removed bytes to the kill ring.
    /// Consecutive kills append to one entry (I-09).
    #[must_use]
    pub fn kill_range(self, start: usize, end: usize) -> Self {
        let (ed, bytes) = self.delete_range(start, end);
        if bytes.is_empty() {
            return ed;
        }
        let ed = if ed.interaction.prev == LastCommand::Kill
            || ed.interaction.last == LastCommand::Kill
        {
            ed.with_kill_ring(|kr| kr.append_last(&bytes))
        } else {
            ed.with_kill_ring(|kr| kr.push(bytes))
        };
        ed.set_last(LastCommand::Kill)
    }

    /// Inserts `bytes` at `pos` and moves point after them.
    #[must_use]
    pub fn insert_at(self, pos: usize, bytes: &[u8]) -> Self {
        let pos = snap(self.buffer().text(), pos.min(self.buffer().text().len()));
        let (ed, applied) = self.apply(Edit::Insert {
            pos,
            bytes: bytes.to_vec(),
        });
        if applied.is_some() {
            ed.move_point(pos + bytes.len())
        } else {
            ed
        }
    }

    /// Ends the current undo group.
    pub fn boundary(self) -> Self {
        self.with_current_buffer(Buffer::close_undo)
    }

    #[must_use]
    pub const fn begin_command(mut self) -> Self {
        self.interaction.prev = self.interaction.last;
        self.interaction.last = LastCommand::Other;
        self
    }

    /// Closes the undo group after a command, unless the command was an insert.
    #[must_use]
    pub fn end_command(self) -> Self {
        if matches!(self.interaction.last, LastCommand::Insert { .. }) {
            self
        } else {
            self.boundary()
        }
    }

    /// Inserts a typed character at point. Up to `TYPED_GROUP` characters share an undo group.
    #[must_use]
    pub fn self_insert(mut self, c: char) -> Self {
        let count = match self.interaction.prev {
            LastCommand::Insert { count } if count < TYPED_GROUP => count + 1,
            _ => {
                self = self.boundary();
                1
            }
        };
        let bytes = c.to_string().into_bytes();
        let len = bytes.len();
        let at = snap(self.buffer().text(), self.window().point);
        let (ed, applied) = self.apply(Edit::Insert { pos: at, bytes });
        if applied.is_none() {
            return ed;
        }
        let mut ed = ed.with_window(|w| Window {
            point: at + len,
            ..w
        });
        ed.interaction.last = LastCommand::Insert { count };
        ed
    }

    /// Emacs-style undo. Consecutive undos walk back through the log; any other
    /// command starts the next undo from the newest group, which undoes the last undo.
    #[must_use]
    pub fn undo(self, fx: Effects) -> (Editor, Effects) {
        let mut ed = self.boundary();
        let id = ed.window().buffer;
        let groups = ed.buffer_by_id(id).undo().groups().len();
        let idx = match ed.interaction.prev {
            LastCommand::Undo { next } => next.checked_sub(1),
            _ => groups.checked_sub(1),
        };
        let Some(idx) = idx else {
            ed.interaction.message = "No further undo information".to_owned();
            return (ed, fx);
        };
        let group = ed.buffer_by_id(id).undo().groups()[idx].clone();
        let mut dropped = 0;
        for edit in group.iter().rev() {
            let (next, applied) = ed.apply(edit.inverse());
            ed = next;
            match applied {
                Some(d) => dropped += d,
                None => break,
            }
        }
        let mut ed = ed.boundary();
        ed.interaction.last = LastCommand::Undo {
            next: idx.saturating_sub(dropped),
        };
        (ed, fx)
    }
}

/// Where an anchor at `a` lands after `edit`.
fn remap(edit: &Edit, a: usize) -> usize {
    match edit {
        Edit::Insert { pos, bytes } => {
            if a > *pos {
                a + bytes.len()
            } else {
                a
            }
        }
        Edit::Delete { pos, bytes } => {
            let end = pos + bytes.len();
            if a <= *pos {
                a
            } else if a <= end {
                *pos
            } else {
                a - bytes.len()
            }
        }
        Edit::SetEol { .. } => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use crate::core::keymap::{Code, Key};
    use crate::core::text::Text;
    use crate::test_support::{press, run_command, text};

    fn editor_with(s: &str) -> Editor {
        Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Text::from_bytes(s.as_bytes())))
    }

    fn type_char(ed: Editor, c: char) -> Editor {
        run_command(ed, |e, fx| (e.self_insert(c), fx))
    }

    fn undo(ed: Editor) -> Editor {
        run_command(ed, |e, fx| e.undo(fx))
    }

    #[test]
    fn insert_at_and_delete_range_snap_to_unit_boundaries() {
        // "가" is 3 bytes: 0..3
        let ed = editor_with("가");
        assert_eq!(ed.buffer().text().len(), 3);

        // insert_at pos 1 (mid-character) should snap to boundary (0)
        let ed_ins = ed.clone().insert_at(1, b"a");
        assert_eq!(text(&ed_ins), "a가");

        // delete_range 1..2 should snap boundaries (0..3)
        let (ed_del, removed) = ed.clone().delete_range(1, 2);
        assert_eq!(text(&ed_del), "");
        assert_eq!(removed, "가".as_bytes());

        // insert_at on view_only buffer must NOT move point (I-06)
        let ed_ro = editor_with("hello").with_current_buffer(|b| b.set_view_only(true));
        let ed_ro = ed_ro.insert_at(0, b"x");
        assert_eq!(ed_ro.window().point, 0);
        assert_eq!(text(&ed_ro), "hello");
    }

    #[test]
    fn non_insert_command_closes_typing_undo_group() {
        // Typing 'hello', then DEL, then undo should restore 'hello'
        let ed = editor_with("");
        let ed = press(ed, Key::plain(Code::Char('h')));
        let ed = press(ed, Key::plain(Code::Char('e')));
        let ed = press(ed, Key::plain(Code::Char('l')));
        let ed = press(ed, Key::plain(Code::Char('l')));
        let ed = press(ed, Key::plain(Code::Char('o')));
        assert_eq!(text(&ed), "hello");

        let ed = press(ed, Key::plain(Code::Backspace));
        assert_eq!(text(&ed), "hell");

        // Undo should undo only DEL (restoring 'hello')
        let ed = press(ed, Key::ctrl('/'));
        assert_eq!(text(&ed), "hello");
    }

    #[test]
    fn undo_groups() {
        let ed = editor_with("");
        let ed = type_char(ed, 'a');
        let ed = type_char(ed, 'b');
        let ed = type_char(ed, 'c');
        assert_eq!(text(&ed), "abc");
        let ed = undo(ed);
        assert_eq!(text(&ed), "");
    }

    #[test]
    fn undo_of_undo() {
        let ed = editor_with("");
        let ed = type_char(ed, 'a');
        let ed = run_command(ed, |e, fx| (e, fx));
        let ed = type_char(ed, 'b');
        let ed = run_command(ed, |e, fx| (e, fx));
        let ed = undo(ed);
        assert_eq!(text(&ed), "a");
        let ed = run_command(ed, |e, fx| (e, fx));
        let ed = undo(ed);
        assert_eq!(text(&ed), "ab");
    }

    #[test]
    fn consecutive_undos_walk_back() {
        let ed = editor_with("");
        let ed = type_char(ed, 'a');
        let ed = run_command(ed, |e, fx| (e, fx));
        let ed = type_char(ed, 'b');
        let ed = run_command(ed, |e, fx| (e, fx));
        let ed = undo(ed);
        assert_eq!(text(&ed), "a");
        let ed = undo(ed);
        assert_eq!(text(&ed), "");
    }

    #[test]
    fn view_mode_blocks_edits() {
        let ed = editor_with("hello").with_current_buffer(|b| b.set_view_only(true));
        let (ed, applied) = ed.apply(Edit::Insert {
            pos: 0,
            bytes: b"x".to_vec(),
        });
        assert!(applied.is_none());
        assert_eq!(ed.interaction.message, "Buffer is read-only");
    }

    #[test]
    fn anchors_follow_edit() {
        let ed = editor_with("abcdef")
            .with_window(|w| Window { point: 4, ..w })
            .with_current_buffer(|b| b.set_mark(Some(2)).set_saved_point(5));
        let (ed, _) = ed.apply(Edit::Insert {
            pos: 1,
            bytes: b"XYZ".to_vec(),
        });
        assert_eq!(ed.window().point, 7);
        assert_eq!(ed.buffer().mark(), Some(5));
        assert_eq!(ed.buffer().saved_point(), 8);
    }

    #[test]
    fn anchor_snaps_to_unit_start() {
        let ed = editor_with("a\r\nb").with_window(|w| Window { point: 1, ..w });
        let (ed, _) = ed.apply(Edit::Insert {
            pos: 0,
            bytes: b"x".to_vec(),
        });
        assert_eq!(ed.window().point, 2);
    }

    #[test]
    fn editor_delete_and_kill_range_appends_consecutive() {
        let ed = Editor::new(Config::default());
        let (ed, id) = ed.add_buffer(
            "test",
            crate::core::text::Text::from_bytes(b"hello wonderful world"),
            crate::core::editor::Backing::None,
        );
        let ed = ed.with_window(|w| Window {
            buffer: id,
            point: 0,
            ..w
        });

        // Kill "hello "
        let ed = ed.kill_range(0, 6);
        assert_eq!(ed.buffer().text().to_bytes(), b"wonderful world");
        assert_eq!(ed.kill_ring().entries(), &[b"hello ".to_vec()]);

        // Consecutive kill "wonderful " -> should append to the same kill ring entry (I-09)
        let ed = ed.kill_range(0, 10);
        assert_eq!(ed.buffer().text().to_bytes(), b"world");
        assert_eq!(ed.kill_ring().entries(), &[b"hello wonderful ".to_vec()]);
    }
}
