//! Editor state. `apply` is the single edit point: it enforces the read-only
//! flag (I-06), records undo (§7), and moves every anchor to a boundary (I-01).
//! Every transition takes the editor by value and returns the new one.

use crate::core::buffer::BufferPool;
use crate::core::commands::CommandRegistry;
use crate::core::config::Config;
use crate::core::effect::Effects;
use crate::core::file::Backing;
use crate::core::keymap::Keymap;
use crate::core::syntax::SyntaxRegistry;
use crate::core::text::Text;
use crate::core::undo::Edit;
use crate::core::window::WindowManager;

pub use crate::core::buffer::{Buffer, BufferId, BufferPool as _BufferPool};
pub use crate::core::mode::{
    Confirm, Isearch, LastCommand, Mode, Prefix, Prompt, Purpose, QueryReplace,
};
pub use crate::core::window::{
    Dir, Layout, Window, WindowManager as _WindowManager, snap_point as snap,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillRing {
    entries: Vec<Vec<u8>>,
}

impl KillRing {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub const fn from_entries(entries: Vec<Vec<u8>>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[Vec<u8>] {
        &self.entries
    }

    pub fn push(mut self, bytes: Vec<u8>) -> Self {
        self.entries.push(bytes);
        self
    }

    pub fn append_last(mut self, bytes: &[u8]) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.extend_from_slice(bytes);
        } else {
            self.entries.push(bytes.to_vec());
        }
        self
    }

    pub fn rotate_right(mut self, n: usize) -> Self {
        if !self.entries.is_empty() {
            let len = self.entries.len();
            self.entries.rotate_right(n % len);
        }
        self
    }

    pub fn last(&self) -> Option<&[u8]> {
        self.entries.last().map(|v| v.as_slice())
    }

    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone)]
pub struct Editor {
    buffers: BufferPool,
    windows: WindowManager,
    kill_ring: KillRing,
    mode: Mode,
    keymap: Keymap,
    /// Kind of the command that just ran. The running command sets it.
    last: LastCommand,
    /// Kind of the command before the running one. Chaining commands read it.
    prev: LastCommand,
    message: String,
    config: Config,
    syntax_registry: SyntaxRegistry,
    commands: CommandRegistry,
    quit: bool,
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
        let buffers = BufferPool::new(config.undo_limit);
        let initial_window = Window {
            buffer: BufferId(0),
            point: 0,
            top: 0,
            left_col: 0,
        };
        Self {
            buffers,
            windows: WindowManager::new(initial_window),
            kill_ring: KillRing::new(),
            mode: Mode::Normal {
                prefix: Prefix::None,
            },
            keymap: Keymap::survival(),
            last: LastCommand::Other,
            prev: LastCommand::Other,
            message: String::new(),
            config,
            syntax_registry: SyntaxRegistry::new(),
            commands: CommandRegistry::new(),
            quit: false,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn set_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn clear_message(mut self) -> Self {
        self.message.clear();
        self
    }

    pub const fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn set_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    pub const fn buffers(&self) -> &BufferPool {
        &self.buffers
    }

    pub fn set_buffers(mut self, buffers: BufferPool) -> Self {
        self.buffers = buffers;
        self
    }

    pub const fn windows(&self) -> &WindowManager {
        &self.windows
    }

    pub fn set_windows(mut self, windows: WindowManager) -> Self {
        self.windows = windows;
        self
    }

    pub const fn config(&self) -> &Config {
        &self.config
    }

    pub const fn set_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    pub fn syntax_registry(&self) -> &SyntaxRegistry {
        &self.syntax_registry
    }

    pub fn with_syntax_registry(mut self, registry: SyntaxRegistry) -> Self {
        self.syntax_registry = registry;
        self
    }

    pub fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    pub fn register_command(mut self, name: impl Into<String>) -> Self {
        self.commands = self.commands.insert(name);
        self
    }

    pub const fn quit(&self) -> bool {
        self.quit
    }

    pub const fn set_quit(mut self, quit: bool) -> Self {
        self.quit = quit;
        self
    }

    pub const fn kill_ring(&self) -> &KillRing {
        &self.kill_ring
    }

    pub fn with_kill_ring(mut self, f: impl FnOnce(KillRing) -> KillRing) -> Self {
        self.kill_ring = f(self.kill_ring);
        self
    }

    pub const fn last(&self) -> LastCommand {
        self.last
    }

    pub const fn set_last(mut self, last: LastCommand) -> Self {
        self.last = last;
        self
    }

    pub const fn prev(&self) -> LastCommand {
        self.prev
    }

    pub const fn set_prev(mut self, prev: LastCommand) -> Self {
        self.prev = prev;
        self
    }

    pub const fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn set_keymap(mut self, keymap: Keymap) -> Self {
        self.keymap = keymap;
        self
    }

    pub fn with_keymap(mut self, f: impl FnOnce(Keymap) -> Keymap) -> Self {
        self.keymap = f(self.keymap);
        self
    }

    pub fn window(&self) -> &Window {
        self.windows.current()
    }

    /// Removes buffer `id`. Windows that showed it move to another buffer (I-08).
    /// Killing the last buffer leaves an empty `*scratch*` (A4).
    pub fn kill_buffer_id(mut self, id: BufferId) -> Self {
        let (buffers, replacement) = self.buffers.kill(id, self.config.undo_limit);
        self.buffers = buffers;
        let saved = self.buffer_by_id(replacement).saved_point;
        self.windows = self.windows.retarget(id, replacement, saved);
        self
    }

    /// Replaces the selected window with `f` applied to it (I-19).
    pub fn with_window(mut self, f: impl FnOnce(Window) -> Window) -> Self {
        let w = f(self.window().clone());
        let text = &self.buffer_by_id(w.buffer).text;
        let w = Window {
            point: snap(text, w.point),
            top: snap(text, w.top),
            ..w
        };
        let sel = self.windows.selected();
        self.windows = self.windows.set_leaf(sel, w);
        self
    }

    pub const fn leaf_count(&self) -> usize {
        self.windows.leaf_count()
    }

    pub fn buffer(&self) -> &Buffer {
        self.buffers.get_by_id(self.window().buffer)
    }

    /// Replaces the buffer with `id` by `f` applied to it. Panics when `id` is unknown (I-20).
    pub fn with_buffer(mut self, id: BufferId, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        self.buffers = self.buffers.with_buffer(id, f);
        self
    }

    /// Like `with_buffer`, for the buffer of the selected window.
    pub fn with_current_buffer(self, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let id = self.window().buffer;
        self.with_buffer(id, f)
    }

    /// Restores the content of the buffer visiting `path` from WAL bytes (I-23).
    pub fn restore_buffer_content(mut self, path: &std::path::Path, bytes: &[u8]) -> Self {
        self.buffers = self.buffers.restore_content(path, bytes);
        self
    }

    /// Adds a buffer and returns its id. Ids are never reused.
    pub fn add_buffer(mut self, name: &str, text: Text, backing: Backing) -> (Self, BufferId) {
        let (buffers, id) = self
            .buffers
            .add(name, text, backing, self.config.undo_limit);
        self.buffers = buffers;
        (self, id)
    }

    /// Applies `edit` to the buffer of the selected window. Returns the new editor and
    /// the number of undo groups dropped to respect the limit, or `None` when refused.
    pub fn apply(mut self, edit: Edit) -> (Self, Option<usize>) {
        let id = self.window().buffer;
        if self.buffer_by_id(id).view_only {
            self.message = "Buffer is read-only".to_owned();
            return (self, None);
        }

        let mut dropped_res = None;
        self.buffers = self.buffers.with_buffer(id, |mut buf| {
            buf.text = edited(buf.text, &edit);
            buf.modified = true;
            let anchor = |a| snap(&buf.text, remap(&edit, a));
            buf.mark = buf.mark.map(anchor);
            buf.saved_point = anchor(buf.saved_point);
            let (undo, dropped) = buf.undo.record(edit.clone());
            buf.undo = undo;
            dropped_res = Some(dropped);
            buf
        });

        let text = &self.buffers.get_by_id(id).text;
        let anchor = |a| snap(text, remap(&edit, a));
        self.windows = self.windows.map_windows(&|w| {
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
        let len = self.buffer().text.len();
        let start = start.min(len);
        let end = end.min(len);
        if start >= end {
            return (self, Vec::new());
        }
        let bytes = self.buffer().text.bytes(start..end);
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
        let ed = if ed.prev == LastCommand::Kill || ed.last == LastCommand::Kill {
            ed.with_kill_ring(|kr| kr.append_last(&bytes))
        } else {
            ed.with_kill_ring(|kr| kr.push(bytes))
        };
        ed.set_last(LastCommand::Kill)
    }

    /// Inserts `bytes` at `pos` and moves point after them.
    pub fn insert_at(self, pos: usize, bytes: &[u8]) -> Self {
        let pos = pos.min(self.buffer().text.len());
        let (ed, _) = self.apply(Edit::Insert {
            pos,
            bytes: bytes.to_vec(),
        });
        ed.move_point(pos + bytes.len())
    }

    /// Moves point to `pos`, snapped to a unit boundary (I-01).
    pub fn move_point(self, pos: usize) -> Self {
        let pos = snap(&self.buffer().text, pos);
        self.with_window(|w| Window { point: pos, ..w })
    }

    /// Ends the current undo group.
    pub fn boundary(self) -> Self {
        self.with_current_buffer(|b| Buffer {
            undo: b.undo.close(),
            ..b
        })
    }

    pub const fn begin_command(mut self) -> Self {
        self.prev = self.last;
        self.last = LastCommand::Other;
        self
    }

    /// Closes the undo group after a command, unless the command was an insert.
    pub fn end_command(self) -> Self {
        if matches!(self.last, LastCommand::Insert { .. }) {
            self
        } else {
            self.boundary()
        }
    }

    pub fn buffer_by_id(&self, id: BufferId) -> &Buffer {
        self.buffers.get_by_id(id)
    }

    /// Inserts a typed character at point. Up to `TYPED_GROUP` characters share an undo group.
    pub fn self_insert(mut self, c: char) -> Self {
        let count = match self.prev {
            LastCommand::Insert { count } if count < TYPED_GROUP => count + 1,
            _ => {
                self = self.boundary();
                1
            }
        };
        let bytes = c.to_string().into_bytes();
        let len = bytes.len();
        let at = snap(&self.buffer().text, self.window().point);
        let (ed, applied) = self.apply(Edit::Insert { pos: at, bytes });
        if applied.is_none() {
            return ed;
        }
        let mut ed = ed.with_window(|w| Window {
            point: at + len,
            ..w
        });
        ed.last = LastCommand::Insert { count };
        ed
    }

    /// Emacs-style undo. Consecutive undos walk back through the log; any other
    /// command starts the next undo from the newest group, which undoes the last undo.
    pub fn undo(self, fx: Effects) -> (Editor, Effects) {
        let mut ed = self.boundary();
        let id = ed.window().buffer;
        let groups = ed.buffer_by_id(id).undo.groups().len();
        let idx = match ed.prev {
            LastCommand::Undo { next } => next.checked_sub(1),
            _ => groups.checked_sub(1),
        };
        let Some(idx) = idx else {
            ed.message = "No further undo information".to_owned();
            return (ed, fx);
        };
        let group = ed.buffer_by_id(id).undo.groups()[idx].clone();
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
        ed.last = LastCommand::Undo {
            next: idx.saturating_sub(dropped),
        };
        (ed, fx)
    }
}

/// The text after `edit`.
fn edited(text: Text, edit: &Edit) -> Text {
    match edit {
        Edit::Insert { pos, bytes } => text.insert(*pos, bytes),
        Edit::Delete { pos, bytes } => text.delete(*pos..*pos + bytes.len()),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keymap::{Code, Key};
    use crate::test_support::{press, run_command, text};

    fn editor_with(s: &str) -> Editor {
        Editor::new(Config::default()).with_current_buffer(|b| Buffer {
            text: Text::from_bytes(s.as_bytes()),
            ..b
        })
    }

    fn type_char(ed: Editor, c: char) -> Editor {
        run_command(ed, |e, fx| (e.self_insert(c), fx))
    }

    fn undo(ed: Editor) -> Editor {
        run_command(ed, |e, fx| e.undo(fx))
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
            ed.mode,
            Mode::Normal {
                prefix: Prefix::CtlX
            }
        );
        let ed = press(ed, Key::ctrl('g'));
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
        assert_eq!(ed.message, "Quit");
    }

    #[test]
    fn view_mode_blocks_edits() {
        let ed = editor_with("hello").with_current_buffer(|b| Buffer {
            view_only: true,
            ..b
        });
        let (ed, applied) = ed.apply(Edit::Insert {
            pos: 0,
            bytes: b"x".to_vec(),
        });
        assert!(applied.is_none());
        assert_eq!(ed.message, "Buffer is read-only");
    }

    #[test]
    fn anchors_follow_edit() {
        let ed = editor_with("abcdef")
            .with_window(|w| Window { point: 4, ..w })
            .with_current_buffer(|b| Buffer {
                mark: Some(2),
                saved_point: 5,
                ..b
            });
        let (ed, _) = ed.apply(Edit::Insert {
            pos: 1,
            bytes: b"XYZ".to_vec(),
        });
        assert_eq!(ed.window().point, 7);
        assert_eq!(ed.buffer().mark, Some(5));
        assert_eq!(ed.buffer().saved_point, 8);
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
        let ed = ed.with_buffer(b2, |b| Buffer {
            modified: true,
            ..b
        });
        assert!(!ed.buffer_by_id(BufferId(0)).modified);
        assert!(ed.buffer_by_id(b2).modified);
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
        assert_eq!(ed.buffer().text.to_bytes(), b"wonderful world");
        assert_eq!(ed.kill_ring().entries(), &[b"hello ".to_vec()]);

        // Consecutive kill "wonderful " -> should append to the same kill ring entry (I-09)
        let ed = ed.kill_range(0, 10);
        assert_eq!(ed.buffer().text.to_bytes(), b"world");
        assert_eq!(ed.kill_ring().entries(), &[b"hello wonderful ".to_vec()]);
    }
}
