//! Editor state. `apply` is the single edit point: it enforces the read-only
//! flag (I-06), records undo (§7), and moves every anchor to a boundary (I-01).
//! Every transition takes the editor by value and returns the new one.

mod documents;
mod interaction;
mod settings;

use crate::core::buffer::BufferPool;
use crate::core::commands::CommandRegistry;
use crate::core::config::Config;
use crate::core::effect::Effects;
use crate::core::eol::Eol;
use crate::core::file::{Backing, FilePath};
use crate::core::keymap::Keymap;
pub use crate::core::kill_ring::KillRing;
use crate::core::syntax::SyntaxRegistry;
use crate::core::text::Text;
use crate::core::undo::Edit;
use crate::core::window::WindowManager;
use documents::Documents;
use interaction::Interaction;
use settings::Settings;
use std::path::{Path, PathBuf};

pub use crate::core::buffer::{Buffer, BufferId};
pub use crate::core::mode::{
    Confirm, Isearch, LastCommand, Mode, Prefix, Prompt, Purpose, QueryReplace,
};
pub use crate::core::window::{Dir, Layout, Window, snap_point as snap};

/// Snaps an end-of-range offset to a unit boundary (moves to end of unit if mid-unit).
pub fn snap_end(text: &Text, a: usize) -> usize {
    let a = a.min(text.len());
    if text.is_boundary(a) {
        a
    } else {
        text.next_boundary(a)
    }
}

#[derive(Debug, Clone)]
pub struct Editor {
    documents: Documents,
    interaction: Interaction,
    settings: Settings,
}

/// Typed characters that share one undo group.
const TYPED_GROUP: usize = 20;

impl Default for Editor {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Editor {
    pub fn new(config: Config) -> Self {
        let initial_window = Window {
            buffer: BufferId(0),
            point: 0,
            top: 0,
            left_col: 0,
        };
        Self {
            documents: Documents::new(initial_window),
            interaction: Interaction::new(),
            settings: Settings::new(config),
        }
    }

    pub const fn edit_seq(&self) -> u64 {
        self.documents.edit_seq
    }

    pub fn clear_wal_dirty(self, id: BufferId) -> Self {
        self.with_buffer(id, Buffer::clear_wal_dirty)
    }

    pub fn message(&self) -> &str {
        &self.interaction.message
    }

    pub fn set_message(mut self, message: impl Into<String>) -> Self {
        self.interaction.message = message.into();
        self
    }

    pub fn clear_message(mut self) -> Self {
        self.interaction.message.clear();
        self
    }

    pub const fn mode(&self) -> &Mode {
        &self.interaction.mode
    }

    pub fn set_mode(mut self, mode: Mode) -> Self {
        self.interaction.mode = mode;
        self
    }

    pub fn confirm(self, question: Confirm, message: impl Into<String>) -> Self {
        self.set_mode(Mode::Confirm(question)).set_message(message)
    }

    pub const fn buffers(&self) -> &BufferPool {
        &self.documents.buffers
    }

    pub const fn windows(&self) -> &WindowManager {
        &self.documents.windows
    }

    pub fn with_windows(mut self, f: impl FnOnce(WindowManager) -> WindowManager) -> Self {
        self.documents.windows = f(self.documents.windows);
        self
    }

    pub const fn config(&self) -> &Config {
        &self.settings.config
    }

    pub const fn set_config(mut self, config: Config) -> Self {
        self.settings.config = config;
        self
    }

    /// Sets the directory that relative file paths resolve against (I-37).
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.settings.cwd = cwd;
        self
    }

    /// The absolute path that `input` names, resolved against the working directory (I-37).
    pub fn resolve_path(&self, input: &Path) -> FilePath {
        FilePath::resolve(&self.settings.cwd, input)
    }

    pub const fn syntax_registry(&self) -> &SyntaxRegistry {
        &self.settings.syntax_registry
    }

    pub fn with_syntax_registry(
        mut self,
        f: impl FnOnce(SyntaxRegistry) -> SyntaxRegistry,
    ) -> Self {
        self.settings.syntax_registry = f(self.settings.syntax_registry);
        self
    }

    pub const fn commands(&self) -> &CommandRegistry {
        &self.settings.commands
    }

    pub fn register_command(mut self, name: impl Into<String>) -> Self {
        self.settings.commands = self.settings.commands.insert(name);
        self
    }

    pub const fn quit(&self) -> bool {
        self.interaction.quit
    }

    pub const fn set_quit(mut self, quit: bool) -> Self {
        self.interaction.quit = quit;
        self
    }

    pub const fn kill_ring(&self) -> &KillRing {
        &self.documents.kill_ring
    }

    pub fn with_kill_ring(mut self, f: impl FnOnce(KillRing) -> KillRing) -> Self {
        self.documents.kill_ring = f(self.documents.kill_ring);
        self
    }

    pub const fn last(&self) -> LastCommand {
        self.interaction.last
    }

    pub const fn set_last(mut self, last: LastCommand) -> Self {
        self.interaction.last = last;
        self
    }

    pub const fn prev(&self) -> LastCommand {
        self.interaction.prev
    }

    pub const fn keymap(&self) -> &Keymap {
        &self.settings.keymap
    }

    pub fn with_keymap(mut self, f: impl FnOnce(Keymap) -> Keymap) -> Self {
        self.settings.keymap = f(self.settings.keymap);
        self
    }

    pub fn window(&self) -> &Window {
        self.documents.windows.current()
    }

    /// Removes buffer `id`. Windows that showed it move to another buffer (I-08).
    /// Killing the last buffer leaves an empty `*scratch*` (A4).
    pub fn kill_buffer_id(mut self, id: BufferId) -> Self {
        let (buffers, replacement) = self.documents.buffers.kill(id);
        self.documents.buffers = buffers;
        let saved = self.buffer_by_id(replacement).saved_point();
        self.documents.windows = self.documents.windows.retarget(id, replacement, saved);
        self
    }

    /// Replaces the selected window with `f` applied to it (I-19).
    pub fn with_window(mut self, f: impl FnOnce(Window) -> Window) -> Self {
        let w = f(*self.window());
        let text = self.buffer_by_id(w.buffer).text();
        let w = Window {
            point: snap(text, w.point),
            top: snap(text, w.top),
            ..w
        };
        let sel = self.documents.windows.selected();
        self.documents.windows = self.documents.windows.set_leaf(sel, w);
        self
    }

    pub const fn leaf_count(&self) -> usize {
        self.documents.windows.leaf_count()
    }

    pub fn buffer(&self) -> &Buffer {
        self.documents.buffers.get_by_id(self.window().buffer)
    }

    /// Replaces the buffer with `id` by `f` applied to it. Panics when `id` is unknown (I-20).
    pub fn with_buffer(mut self, id: BufferId, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        self.documents.buffers = self.documents.buffers.with_buffer(id, f);
        self
    }

    /// Like `with_buffer`, for the buffer of the selected window.
    pub fn with_current_buffer(self, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let id = self.window().buffer;
        self.with_buffer(id, f)
    }

    /// Replaces the text of buffer `id` outside `apply`. The undo history restarts (I-34), and the
    /// windows showing it are snapped to the new text (I-01).
    pub fn replace_text(self, id: BufferId, text: Text) -> Self {
        self.with_buffer(id, |b| b.with_text(text))
            .snap_windows_for_buffer(id)
    }

    /// Snaps `point` and resets `top` for every window displaying buffer `id` (I-01).
    pub fn snap_windows_for_buffer(mut self, id: BufferId) -> Self {
        let text = self.documents.buffers.get_by_id(id).text();
        self.documents.windows = self.documents.windows.map_windows(&|w| {
            if w.buffer == id {
                Window {
                    point: snap(text, w.point),
                    top: 0,
                    ..w
                }
            } else {
                w
            }
        });
        self
    }

    /// Restores the content of the buffer visiting `path` from WAL bytes and EOL (I-23).
    pub fn restore_buffer_content(
        mut self,
        path: &std::path::Path,
        bytes: &[u8],
        eol: Eol,
        line: Option<usize>,
    ) -> Self {
        let (buffers, id) = self
            .documents
            .buffers
            .restore_content(path, bytes, eol, line);
        self.documents.buffers = buffers;
        match id {
            Some(id) => self.snap_windows_for_buffer(id),
            None => self,
        }
    }

    /// Adds a buffer and returns its id. Ids are never reused.
    pub fn add_buffer(mut self, name: &str, text: Text, backing: Backing) -> (Self, BufferId) {
        let (buffers, id) = self.documents.buffers.add(name, text, backing);
        self.documents.buffers = buffers;
        (self, id)
    }

    /// Applies `edit` to the buffer of the selected window. Returns the new editor and
    /// the number of undo groups dropped to respect the limit, or `None` when refused.
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

    /// Moves point to `pos`, snapped to a unit boundary (I-01).
    pub fn move_point(self, pos: usize) -> Self {
        let pos = snap(self.buffer().text(), pos);
        self.with_window(|w| Window { point: pos, ..w })
    }

    /// Ends the current undo group.
    pub fn boundary(self) -> Self {
        self.with_current_buffer(Buffer::close_undo)
    }

    pub const fn begin_command(mut self) -> Self {
        self.interaction.prev = self.interaction.last;
        self.interaction.last = LastCommand::Other;
        self
    }

    /// Closes the undo group after a command, unless the command was an insert.
    pub fn end_command(self) -> Self {
        if matches!(self.interaction.last, LastCommand::Insert { .. }) {
            self
        } else {
            self.boundary()
        }
    }

    pub fn buffer_by_id(&self, id: BufferId) -> &Buffer {
        self.documents.buffers.get_by_id(id)
    }

    /// Inserts a typed character at point. Up to `TYPED_GROUP` characters share an undo group.
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
    use crate::core::keymap::{Code, Key};
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
    fn keys_type_and_undo() {
        let ed = press(editor_with(""), Key::plain(Code::Char('a')));
        let ed = press(ed, Key::plain(Code::Char('b')));
        assert_eq!(text(&ed), "ab");
        let ed = press(ed, Key::plain(Code::Char('c')));
        assert_eq!(text(&ed), "abc");
    }

    #[test]
    fn keys_route_through_prefixes() {
        let ed = press(editor_with(""), Key::ctrl('x'));
        assert_eq!(
            ed.interaction.mode,
            Mode::Normal {
                prefix: Prefix::CtlX
            }
        );
        let ed = press(ed, Key::ctrl('g'));
        assert_eq!(
            ed.interaction.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
        assert_eq!(ed.interaction.message, "Quit");
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
    fn with_buffer_updates_only_target() {
        let (ed, b2) =
            editor_with("first").add_buffer("b2", Text::from_bytes(b"second"), Backing::None);
        let ed = ed.with_buffer(b2, |b| b.set_modified(true));
        assert!(!ed.buffer_by_id(BufferId(0)).modified());
        assert!(ed.buffer_by_id(b2).modified());
    }

    #[test]
    fn kill_ring_operations() {
        let kr = KillRing::new();
        assert!(kr.is_empty());
        assert_eq!(kr.len(), 0);
        assert_eq!(kr.last(), None);

        let kr = kr.push(b"hello".to_vec());
        assert_eq!(kr.len(), 1);
        assert_eq!(kr.last(), Some(&b"hello"[..]));

        let kr = kr.append_last(b" world");
        assert_eq!(kr.len(), 1);
        assert_eq!(kr.last(), Some(&b"hello world"[..]));

        let kr = kr.push(b"second".to_vec());
        assert_eq!(kr.len(), 2);
        assert_eq!(kr.last(), Some(&b"second"[..]));

        let kr = kr.rotate_right(1);
        assert_eq!(kr.last(), Some(&b"hello world"[..]));
    }

    #[test]
    fn editor_encapsulation_and_transitions() {
        let ed = Editor::new(Config::default());
        assert_eq!(ed.message(), "");
        assert!(!ed.quit());

        let ed = ed.set_message("status");
        assert_eq!(ed.message(), "status");
        let ed = ed.clear_message();
        assert_eq!(ed.message(), "");

        let ed = ed.set_quit(true);
        assert!(ed.quit());

        let ed = ed.with_kill_ring(|kr| kr.push(b"sample".to_vec()));
        assert_eq!(ed.kill_ring().last(), Some(&b"sample"[..]));

        let ed = ed.with_keymap(|km| km.bind_str("C-x 2", "split-window-below").unwrap());
        let key = crate::core::keymap::parse_key_seq("C-x 2").unwrap();
        assert_eq!(ed.keymap().lookup(&key), Some("split-window-below"));
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

    #[test]
    fn with_windows_and_snap_windows_for_buffer() {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Text::from_bytes("가나다라".as_bytes())))
            .with_window(|w| Window {
                point: 9,
                top: 6,
                ..w
            })
            .with_windows(|wm| wm.split(Dir::Below));
        let id = ed.window().buffer;
        let ed = ed
            .with_buffer(id, |b| b.with_text(Text::from_bytes("가".as_bytes())))
            .snap_windows_for_buffer(id);
        assert_eq!(ed.window().point, 3);
        assert_eq!(ed.window().top, 0);
    }
}
