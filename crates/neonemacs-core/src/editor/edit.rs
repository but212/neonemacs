//! The edit pipeline (§7, I-01, I-06, I-09, I-38). `apply` is the single edit point: every change
//! to a buffer goes through it, so the read-only check, undo, and anchor moves stay in one place.

use super::{Buffer, Editor, LastCommand, Window};
use crate::config::Toggles;
use crate::effect::Effects;
use crate::undo::{Edit, RecordedEdit};
use crate::width::column;
use alloc::borrow::ToOwned;
use alloc::vec::Vec;

impl Editor {
    /// Applies `edit` to the buffer of the selected window. Returns the new editor and
    /// the number of undo groups dropped to respect the limit, or `None` when refused.
    #[must_use]
    pub fn apply(self, edit: Edit) -> (Self, Option<usize>) {
        self.apply_recorded(edit.into())
    }

    fn apply_recorded(mut self, edit: RecordedEdit) -> (Self, Option<usize>) {
        let id = self.window().buffer;
        if self.buffer_by_id(id).view_only() {
            return (self.set_message("Buffer is read-only"), None);
        }

        let span = edit.span;
        let mut dropped_res = None;
        let undo_limit = self.settings.config.undo_limit;
        self.documents.buffers = self.documents.buffers.with_buffer(id, |buf| {
            let (buf, dropped) = buf.apply_recorded(edit, |a| span.remap(a), undo_limit);
            dropped_res = dropped;
            buf
        });
        if dropped_res.is_none() {
            return (self, None);
        }

        // The yank offsets describe the text before this edit, so the yank ends here (I-38).
        if matches!(self.interaction.last, LastCommand::Yank { .. }) {
            self.interaction.last = LastCommand::Other;
        }
        self.documents.edit_seq = self.documents.edit_seq.wrapping_add(1);

        let text = self.documents.buffers.get_by_id(id).text();
        let anchor = |a| text.floor_boundary(span.remap(a));
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

    /// Deletes `start..end` without returning a copy of the removed bytes.
    #[must_use]
    pub fn delete_only(self, start: usize, end: usize) -> Self {
        let text = self.buffer().text();
        let start = text.floor_boundary(start);
        let end = text.ceil_boundary(end);
        if start >= end {
            return self;
        }
        let (ed, _) = self.apply(Edit::delete(start, end - start));
        ed
    }

    /// Deletes `start..end` and pushes the removed `Doc` slice to the kill ring.
    /// Consecutive kills append to one entry (I-09).
    #[must_use]
    pub fn kill_range(self, start: usize, end: usize) -> Self {
        let text = self.buffer().text();
        let start = text.floor_boundary(start);
        let end = text.ceil_boundary(end);
        if start >= end {
            return self;
        }
        let Ok(killed) = text.slice(start..end) else {
            return self;
        };
        let (ed, applied) = self.apply(Edit::delete(start, end - start));
        if applied.is_none() || killed.total_bytes() == 0 {
            return ed;
        }
        let max = ed.config().kill_ring_max;
        let ed = if ed.interaction.prev == LastCommand::Kill
            || ed.interaction.last == LastCommand::Kill
        {
            ed.with_kill_ring(|kr| kr.append_last(killed))
        } else {
            ed.with_kill_ring(|kr| kr.push(killed, max))
        };
        ed.set_last(LastCommand::Kill)
    }

    /// Inserts `bytes` at `pos` and moves point after them.
    #[must_use]
    pub fn insert_at(self, pos: usize, bytes: &[u8]) -> Self {
        let pos = self.buffer().text().floor_boundary(pos);
        let (ed, applied) = self.apply(Edit::insert(pos, bytes));
        if applied.is_some() {
            ed.move_point(pos + bytes.len())
        } else {
            ed
        }
    }

    /// Ends the current undo group.
    #[must_use]
    pub fn boundary(self) -> Self {
        let limit = self.settings.config.undo_limit;
        self.with_current_buffer(|b| b.close_undo(limit))
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

    /// Inserts a typed character at point. Up to `amalgamating-undo-limit` characters share an undo group.
    #[must_use]
    pub fn self_insert(mut self, c: char) -> Self {
        let count = match self.interaction.prev {
            LastCommand::Insert { count }
                if count < self.settings.config.amalgamating_undo_limit =>
            {
                count + 1
            }
            _ => {
                self = self.boundary();
                1
            }
        };
        let at = self.buffer().text().floor_boundary(self.window().point);
        let (ed, applied, len) =
            if c == '\t' && self.settings.config.toggles.contains(Toggles::EXPAND_TABS) {
                let m = self.settings.config.metrics();
                let tab = m.tab;
                let col = column(self.buffer().text(), at, m);
                let spaces = tab - col % tab;
                let bytes = " ".repeat(spaces).into_bytes();
                let (ed, applied) = self.apply(Edit::insert(at, bytes));
                (ed, applied, spaces)
            } else {
                let mut utf8 = [0u8; 4];
                let s = c.encode_utf8(&mut utf8);
                let len = s.len();
                let (ed, applied) = self.apply(Edit::insert(at, s.as_bytes()));
                (ed, applied, len)
            };
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
    pub fn undo(self, mut fx: Effects) -> (Self, Effects) {
        let mut ed = self.boundary();
        let id = ed.window().buffer;
        let groups = ed.buffer_by_id(id).undo().groups().len();
        let idx = match ed.interaction.prev {
            LastCommand::Undo { next } => next.checked_sub(1),
            _ => groups.checked_sub(1),
        };
        let Some(idx) = idx else {
            "No further undo information".clone_into(&mut ed.interaction.message);
            return (ed, fx);
        };
        let (clean_epoch, inverses) = {
            let group = &ed.buffer_by_id(id).undo().groups()[idx];
            let inverses: Vec<_> = group.edits().iter().rev().cloned().collect();
            (group.clean_epoch(), inverses)
        };
        let mut dropped = 0;
        let mut complete = true;
        for inv in inverses {
            let (next, applied) = ed.apply_recorded(inv);
            ed = next;
            if let Some(d) = applied {
                dropped += d;
            } else {
                complete = false;
                break;
            }
        }
        let mut ed = ed.boundary();
        // The group opened on text equal to the saved one, so undoing all of it restores that text (I-41).
        if complete && clean_epoch == Some(ed.buffer_by_id(id).epoch()) {
            fx.extend(ed.remove_wal_effect(id));
            ed = ed.with_buffer(id, Buffer::restore_clean);
        }
        ed.interaction.last = LastCommand::Undo {
            next: idx.saturating_sub(dropped),
        };
        (ed, fx)
    }
}

#[cfg(test)]
fn remap(edit: &Edit, a: usize) -> usize {
    RecordedEdit::from(edit.clone()).span.remap(a)
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::vec;

    use super::*;
    use crate::Doc;
    use crate::config::Config;
    use crate::keymap::{Code, Key};
    use crate::test_support::{press, run_command, text};

    fn editor_with(s: &str) -> Editor {
        Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(s.as_bytes())))
    }

    fn type_char(ed: Editor, c: char) -> Editor {
        run_command(ed, |e, fx| (e.self_insert(c), fx))
    }

    fn undo(ed: Editor) -> Editor {
        run_command(ed, Editor::undo)
    }

    #[test]
    fn insert_at_and_kill_range_snap_to_unit_boundaries() {
        // "가" is 3 bytes: 0..3
        let ed = editor_with("가");
        assert_eq!(ed.buffer().text().total_bytes(), 3);

        // insert_at pos 1 (mid-character) should snap to boundary (0)
        let ed_ins = ed.clone().insert_at(1, b"a");
        assert_eq!(text(&ed_ins), "a가");

        // kill_range 1..2 should snap boundaries (0..3)
        let ed_del = ed.kill_range(1, 2);
        assert_eq!(text(&ed_del), "");
        assert_eq!(
            ed_del.kill_ring().last(),
            Some(&Doc::from_bytes("가".as_bytes()))
        );

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
    fn remap_replace_equals_delete_then_insert() {
        for pos in 0..6 {
            for removed in [0, 1, 3] {
                for inserted in [0, 1, 4] {
                    let replace =
                        Edit::replace(pos, removed, Doc::from_bytes(&vec![b'i'; inserted]));
                    let del = Edit::delete(pos, removed);
                    let ins = Edit::insert(pos, vec![b'i'; inserted]);
                    for a in 0..14 {
                        assert_eq!(
                            remap(&replace, a),
                            remap(&ins, remap(&del, a)),
                            "pos={pos} removed={removed} inserted={inserted} a={a}"
                        );
                    }
                }
            }
        }
    }

    /// A file-backed buffer that was just saved with `saved` as its text.
    fn saved(saved: &str) -> Editor {
        use crate::file::{Backing, DiskState, FilePath};
        Editor::new(Config::default()).with_current_buffer(|b| {
            b.with_text(Doc::from_bytes(saved.as_bytes()))
                .set_backing(Backing::file(
                    FilePath::resolve("", "f.txt"),
                    DiskState::Unknown,
                ))
                .record_saved(DiskState::Unknown)
        })
    }

    fn undo_fx(ed: Editor) -> (Editor, Effects) {
        crate::dispatch::command(ed, Vec::new(), Editor::undo)
    }

    #[test]
    fn undo_back_to_saved_text_is_clean_and_drops_the_wal() {
        // I-41
        let ed = type_char(saved(""), 'a');
        assert!(ed.buffer().modified());
        let (ed, fx) = undo_fx(ed);
        assert_eq!(text(&ed), "");
        assert!(!ed.buffer().modified());
        assert!(!ed.buffer().is_wal_dirty());
        assert_eq!(
            fx,
            vec![crate::effect::Effect::RemoveWal {
                target: crate::effect::WalTarget::Sibling("f.txt".to_owned())
            }]
        );
    }

    #[test]
    fn undo_past_the_saved_text_is_modified() {
        let ed = type_char(editor_with(""), 'x');
        let ed = ed.with_current_buffer(|b| b.record_saved(crate::file::DiskState::Unknown));
        let ed = type_char(ed, 'a');
        let ed = undo(ed);
        assert!(!ed.buffer().modified());
        // The chained undo removes the saved "x": now the text differs from the saved one.
        let ed = undo(ed);
        assert_eq!(text(&ed), "");
        assert!(ed.buffer().modified());
    }

    #[test]
    fn undoing_an_undo_returns_to_clean_through_the_original_group() {
        // Undoing to clean text drops the WAL of the file-backed buffer (I-41).
        let wal = crate::effect::Effect::RemoveWal {
            target: crate::effect::WalTarget::Sibling("f.txt".to_owned()),
        };
        let (ed, fx) = undo_fx(type_char(saved(""), 'a'));
        assert_eq!(fx, core::slice::from_ref(&wal));
        assert!(!ed.buffer().modified());
        // A command in between breaks the undo chain, so this undo redoes "a".
        let ed = run_command(ed, |e, fx| (e.move_point(0), fx));
        let (ed, fx) = undo_fx(ed);
        assert!(fx.is_empty());
        assert_eq!(text(&ed), "a");
        assert!(ed.buffer().modified());
        // The chained undo removes "a" through the group opened on the clean text.
        let (ed, fx) = undo_fx(ed);
        assert_eq!(fx, [wal]);
        assert_eq!(text(&ed), "");
        assert!(!ed.buffer().modified());
    }

    #[test]
    fn a_save_after_the_edit_makes_undo_modified() {
        let ed = type_char(saved(""), 'a');
        let ed = ed.with_current_buffer(|b| b.record_saved(crate::file::DiskState::Unknown));
        let ed = undo(ed);
        assert_eq!(text(&ed), "");
        assert!(ed.buffer().modified());
    }

    #[test]
    fn trimmed_history_never_reports_clean() {
        // The group opened on the clean text is trimmed away, so equal text stays "modified".
        let ed = saved("").set_config(Config {
            undo_limit: 1,
            ..Config::default()
        });
        let ed = type_char(ed, 'a');
        let ed = run_command(ed, |e, fx| {
            let (e, _) = e.apply(Edit::delete(0, 1));
            (e, fx)
        });
        assert_eq!(text(&ed), "");
        assert!(ed.buffer().modified());
    }

    #[test]
    fn chained_undo_stays_on_the_right_group_when_the_log_is_trimmed() {
        // Undoing c adds a 3-byte backing charge to the three 1-byte groups. The limit drops "a".
        let ed = editor_with("").set_config(Config {
            undo_limit: 5,
            ..Config::default()
        });
        // A command in between closes each group, so every character is its own undo step.
        let split = |ed: Editor| {
            run_command(ed, |e, fx| {
                let p = e.window().point;
                (e.move_point(p), fx)
            })
        };
        let ed = type_char(split(type_char(split(type_char(ed, 'a')), 'b')), 'c');
        let ed = undo(ed);
        assert_eq!(text(&ed), "ab");
        // The next undo must reach "b", not repeat "c" because the dropped group was not counted.
        let ed = undo(ed);
        assert_eq!(text(&ed), "a");
    }

    #[test]
    fn refused_undo_in_view_mode_stays_modified() {
        let ed = type_char(saved(""), 'a').with_current_buffer(|b| b.set_view_only(true));
        let ed = undo(ed);
        assert_eq!(text(&ed), "a");
        assert!(ed.buffer().modified());
    }

    #[test]
    fn invalid_edit_leaves_editor_unchanged() {
        let ed = editor_with("é")
            .move_point(2)
            .set_last(LastCommand::Yank { start: 0, end: 2 });
        let seq = ed.edit_seq();
        let (ed, applied) = ed.apply(Edit::delete(1, 1));
        assert_eq!(applied, None);
        assert_eq!(text(&ed), "é");
        assert_eq!(ed.window().point, 2);
        assert_eq!(ed.edit_seq(), seq);
        assert_eq!(ed.interaction.last, LastCommand::Yank { start: 0, end: 2 });
        assert!(!ed.buffer().modified());
        assert!(ed.buffer().undo().groups().is_empty());
    }

    #[test]
    fn undo_limit_charges_storage_needed_to_restore_utf8_seams() {
        let original = Doc::from_bytes(&[0xC3, b'X', 0xA9]);
        for limit in [2, 3] {
            let ed = editor_with("")
                .set_config(Config {
                    undo_limit: limit,
                    ..Config::default()
                })
                .with_current_buffer(|b| b.with_text(original.clone()))
                .delete_only(1, 2)
                .boundary();
            assert_eq!(text(&ed), "é");
            assert_eq!(ed.buffer().undo().groups().len(), usize::from(limit == 3));
            if limit == 3 {
                let (ed, _) = ed.undo(Vec::new());
                assert_eq!(ed.buffer().text(), &original);
            }
        }
    }

    #[test]
    fn undo_limit_evicts_replacements_that_retain_a_full_slice_each() {
        let ed = editor_with(&"a".repeat(1024)).set_config(Config {
            undo_limit: 2048,
            ..Config::default()
        });
        let ed = [b"b", b"c", b"d"].into_iter().fold(ed, |ed, byte| {
            ed.apply(Edit::replace(512, 1, Doc::from_bytes(byte)))
                .0
                .boundary()
        });
        assert_eq!(ed.buffer().undo().groups().len(), 2);
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(ed.buffer().text().byte_at(512), Some(b'c'));
    }

    #[test]
    fn undo_and_undo_of_undo_remap_around_a_joined_utf8_unit() {
        let original = [0xC3, b'X', 0xA9];
        let ed = editor_with("")
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(&original)).set_mark(Some(3)))
            .move_point(3);
        let ed = ed.delete_only(1, 2).boundary();
        assert_eq!(text(&ed), "é");
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(&original));
        assert_eq!(ed.window().point, 3);
        assert_eq!(ed.buffer().mark(), Some(3));
        let (ed, _) = ed
            .set_last(LastCommand::Other)
            .begin_command()
            .undo(Vec::new());
        assert_eq!(text(&ed), "é");
        assert_eq!(ed.window().point, 2);
        assert_eq!(ed.buffer().mark(), Some(2));
    }

    #[test]
    fn replace_undoes_in_one_step() {
        let ed = editor_with("hello world");
        let (ed, _) = ed.apply(Edit::replace(6, 5, Doc::from_bytes(b"there")));
        assert_eq!(text(&ed), "hello there");
        assert_eq!(ed.buffer().undo().groups().len(), 1);
        assert_eq!(ed.buffer().undo().groups()[0].edits().len(), 1);
        let ed = undo(ed);
        assert_eq!(text(&ed), "hello world");
    }

    #[test]
    fn view_mode_blocks_edits() {
        let ed = editor_with("hello").with_current_buffer(|b| b.set_view_only(true));
        let (ed, applied) = ed.apply(Edit::insert(0, b"x".as_slice()));
        assert!(applied.is_none());
        assert_eq!(ed.interaction.message, "Buffer is read-only");
    }

    #[test]
    fn empty_delete_only_on_view_only_is_a_noop() {
        let ed = editor_with("hello").with_current_buffer(|b| b.set_view_only(true));
        let ed = ed.delete_only(2, 2);
        assert_eq!(text(&ed), "hello");
        assert_eq!(ed.interaction.message, "");
    }

    #[test]
    fn anchors_follow_edit() {
        let ed = editor_with("abcdef")
            .with_window(|w| Window { point: 4, ..w })
            .with_current_buffer(|b| b.set_mark(Some(2)).set_saved_point(5));
        let (ed, _) = ed.apply(Edit::insert(1, b"XYZ".as_slice()));
        assert_eq!(ed.window().point, 7);
        assert_eq!(ed.buffer().mark(), Some(5));
        assert_eq!(ed.buffer().saved_point(), 8);
    }

    #[test]
    fn anchor_snaps_to_unit_start() {
        let ed = editor_with("a\r\nb").with_window(|w| Window { point: 1, ..w });
        let (ed, _) = ed.apply(Edit::insert(0, b"x".as_slice()));
        assert_eq!(ed.window().point, 2);
    }

    #[test]
    fn editor_delete_and_kill_range_appends_consecutive() {
        let ed = Editor::new(Config::default());
        let (ed, id) = ed.add_buffer(
            "test",
            Doc::from_bytes(b"hello wonderful world"),
            crate::editor::Backing::None,
        );
        let ed = ed.with_window(|w| Window {
            buffer: id,
            point: 0,
            ..w
        });

        // Kill "hello "
        let ed = ed.kill_range(0, 6);
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"wonderful world"));
        assert_eq!(ed.kill_ring().entries(), &[Doc::from_bytes(b"hello ")]);

        // Consecutive kill "wonderful " -> should append to the same kill ring entry (I-09)
        let ed = ed.kill_range(0, 10);
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"world"));
        assert_eq!(
            ed.kill_ring().entries(),
            &[Doc::from_bytes(b"hello wonderful ")]
        );
    }

    #[test]
    fn amalgamating_undo_limit_sets_the_typed_group_size() {
        let ed = Editor::new(Config {
            amalgamating_undo_limit: 2,
            ..Config::default()
        });
        let ed = crate::test_support::typ(ed, "abcde");
        let ed = crate::test_support::keys(ed, "C-/");
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"abcd"));
        let ed = crate::test_support::keys(ed, "C-/");
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"ab"));
    }

    #[test]
    fn kill_ring_max_setting_bounds_the_ring() {
        let ed = Editor::new(Config {
            kill_ring_max: 1,
            ..Config::default()
        });
        let (ed, id) = ed.add_buffer("test", Doc::from_bytes(b"ab"), crate::editor::Backing::None);
        let ed = ed.with_window(|w| Window {
            buffer: id,
            point: 0,
            ..w
        });
        let ed = ed
            .kill_range(0, 1)
            .set_last(LastCommand::Other)
            .set_last(LastCommand::Other)
            .kill_range(0, 1);
        assert_eq!(ed.kill_ring().entries(), &[Doc::from_bytes(b"b")]);
    }

    fn soft_tabs(s: &str) -> Editor {
        let config = Config {
            toggles: Toggles::DEFAULT.union(Toggles::EXPAND_TABS),
            ..Config::default()
        };
        Editor::new(config).with_current_buffer(|b| b.with_text(Doc::from_bytes(s.as_bytes())))
    }

    #[test]
    fn expand_tabs_inserts_spaces_to_next_stop_as_one_undo_step() {
        for (before, spaces) in [("", 8), ("abc", 5), ("abcdefg", 1), ("가", 6)] {
            let ed = soft_tabs(before);
            let ed = ed.with_window(|w| Window {
                point: before.len(),
                ..w
            });
            let ed = type_char(ed, '\t');
            assert_eq!(text(&ed), format!("{before}{}", " ".repeat(spaces)));
            assert_eq!(ed.window().point, before.len() + spaces);
            assert_eq!(text(&undo(ed)), before);
        }
    }

    #[test]
    fn tab_is_literal_when_expand_tabs_is_off_and_other_chars_are_unchanged() {
        let ed = type_char(editor_with(""), '\t');
        assert_eq!(text(&ed), "\t");
        let ed = type_char(soft_tabs(""), 'x');
        assert_eq!(text(&ed), "x");
    }

    #[test]
    fn expand_tabs_follows_tab_width() {
        let config = Config {
            toggles: Toggles::DEFAULT.union(Toggles::EXPAND_TABS),
            tab_width: 4,
            ..Config::default()
        };
        let ed = type_char(Editor::new(config), '\t');
        assert_eq!(text(&ed), "    ");
    }
}
