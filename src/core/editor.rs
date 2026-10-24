//! Editor state. `apply` is the single edit point: it enforces the read-only
//! flag (I-06), records undo (§7), and moves every anchor to a boundary (I-01).
//! Every transition takes the editor by value and returns the new one.

use crate::core::buffer::BufferPool;
use crate::core::commands;
use crate::core::config::Config;
use crate::core::effect::{Effect, Effects};
use crate::core::file::Backing;
use crate::core::keymap::{Code, Key, Keymap};
use crate::core::text::Text;
use crate::core::undo::Edit;
use crate::core::window::WindowManager;

pub use crate::core::buffer::{Buffer, BufferId, BufferPool as _BufferPool};
pub use crate::core::mode::{
    Confirm, Isearch, LastCommand, Mode, Prefix, Prompt, Purpose, QueryReplace,
};
pub use crate::core::window::{snap_point as snap, Dir, Layout, Window, WindowManager as _WindowManager};

#[derive(Debug, Clone)]
pub struct Editor {
    pub buffers: BufferPool,
    pub windows: WindowManager,
    pub kill_ring: Vec<Vec<u8>>,
    pub mode: Mode,
    pub keymap: Keymap,
    /// Kind of the command that just ran. The running command sets it.
    pub last: LastCommand,
    /// Kind of the command before the running one. Chaining commands read it.
    pub prev: LastCommand,
    pub message: String,
    pub config: Config,
    pub quit: bool,
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
            kill_ring: Vec::new(),
            mode: Mode::Normal {
                prefix: Prefix::None,
            },
            keymap: Keymap::survival(),
            last: LastCommand::Other,
            prev: LastCommand::Other,
            message: String::new(),
            config,
            quit: false,
        }
    }

    pub fn window(&self) -> &Window {
        self.windows.current()
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
        let (buffers, id) = self.buffers.add(name, text, backing, self.config.undo_limit);
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
        let mut buf = self.buffer_by_id(id).clone();
        buf.text = edited(buf.text, &edit);
        buf.modified = true;

        let anchor = |a| snap(&buf.text, remap(&edit, a));
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
        buf.mark = buf.mark.map(anchor);
        buf.saved_point = anchor(buf.saved_point);
        let (undo, dropped) = buf.undo.record(edit);
        buf.undo = undo;
        self.buffers = self.buffers.with_buffer(id, |_| buf);
        (self, Some(dropped))
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

    /// Runs one command. The undo group ends after every command that is not an insert.
    pub fn command(
        self,
        fx: Effects,
        run: impl FnOnce(Editor, Effects) -> (Editor, Effects),
    ) -> (Editor, Effects) {
        let (ed, fx) = run(self.begin_command(), fx);
        (ed.end_command(), fx)
    }

    const fn begin_command(mut self) -> Self {
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

    /// Handles one key press: resolves prefixes, then runs a bound command or inserts text.
    pub fn handle_key(self, key: Key, fx: Effects) -> (Editor, Effects) {
        match &self.mode {
            Mode::Normal { prefix } => {
                let prefix = *prefix;
                self.handle_normal_key(prefix, key, fx)
            }
            Mode::Confirm(question) => {
                let question = *question;
                self.answer(question, key, fx)
            }
            Mode::Isearch(_) => commands::isearch_key(self, key, fx),
            Mode::QueryReplace(_) => (commands::query_key(self, key), fx),
            Mode::Prompt(_) => commands::prompt_key(self, key, fx),
        }
    }

    /// Answers a yes-or-no question. `y` confirms, `n` and `C-g` cancel, other keys wait.
    fn answer(mut self, question: Confirm, key: Key, fx: Effects) -> (Editor, Effects) {
        let yes = key.text() == Some('y');
        if yes || key.text() == Some('n') || key == Key::ctrl('g') {
            self.mode = Mode::Normal {
                prefix: Prefix::None,
            };
            if yes {
                return commands::confirmed(self, question, fx);
            }
            self.message = "Cancelled".to_owned();
        }
        (self, fx)
    }

    pub fn handle_normal_key(mut self, prefix: Prefix, key: Key, fx: Effects) -> (Editor, Effects) {
        self.mode = Mode::Normal {
            prefix: Prefix::None,
        };
        if key == Key::ctrl('g') {
            self.message = "Quit".to_owned();
            return (self, fx);
        }
        let keys = match prefix {
            Prefix::None => {
                if key == Key::plain(Code::Esc) {
                    self.mode = Mode::Normal {
                        prefix: Prefix::Meta,
                    };
                    return (self, fx);
                }
                if key == Key::ctrl('x') {
                    self.mode = Mode::Normal {
                        prefix: Prefix::CtlX,
                    };
                    return (self, fx);
                }
                vec![key]
            }
            Prefix::CtlX => vec![Key::ctrl('x'), key],
            Prefix::Meta => vec![key.with_meta()],
        };
        if let Some(name) = self.keymap.lookup(&keys).map(str::to_owned) {
            self.run_named(&name, fx)
        } else if let (Prefix::None, Some(c)) = (prefix, key.text()) {
            self.command(fx, |e, fx| (e.self_insert(c), fx))
        } else {
            let seq: Vec<String> = keys.iter().map(|k| k.describe()).collect();
            self.message = format!("{} is undefined", seq.join(" "));
            (self, fx)
        }
    }

    /// Runs the command `name`. A built-in runs now. Any other name is sent to Lua as `RunLua`;
    /// its result comes back as `LuaDone`.
    pub fn run_named(self, name: &str, mut fx: Effects) -> (Editor, Effects) {
        match commands::find(name) {
            Some(run) => self.command(fx, run),
            None => {
                let ed = self.begin_command();
                fx.push(Effect::RunLua {
                    name: name.to_owned(),
                });
                (ed, fx)
            }
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
        let (ed, b2) = editor_with("first").add_buffer("b2", Text::from_bytes(b"second"), Backing::None);
        let ed = ed.with_buffer(b2, |b| Buffer {
            modified: true,
            ..b
        });
        assert!(!ed.buffer_by_id(BufferId(0)).modified);
        assert!(ed.buffer_by_id(b2).modified);
    }
}
