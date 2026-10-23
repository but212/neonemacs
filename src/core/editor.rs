//! Editor state. `apply` is the single edit point: it enforces the read-only
//! flag (I-06), records undo (§7), and moves every anchor to a boundary (I-01).
//! Every transition takes the editor by value and returns the new one.

use crate::core::commands;
use crate::core::config::Config;
use crate::core::effect::{Effect, Effects};
use crate::core::file::Backing;
use crate::core::keymap::{Code, Key, Keymap};
use crate::core::search::Pattern;
use crate::core::text::Text;
use crate::core::undo::{Edit, UndoLog};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct BufferId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Below,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub buffer: BufferId,
    pub point: usize,
    pub top: usize,
    pub left_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Leaf(Window),
    Split {
        dir: Dir,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// Every window, in layout order.
    pub fn windows(&self) -> Vec<&Window> {
        match self {
            Layout::Leaf(w) => vec![w],
            Layout::Split { first, second, .. } => {
                let mut out = first.windows();
                out.extend(second.windows());
                out
            }
        }
    }

    pub const fn leaf_count(&self) -> usize {
        match self {
            Layout::Leaf(_) => 1,
            Layout::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// The window at layout index `i`. Panics when `i` is out of range (I-19).
    pub fn leaf(&self, i: usize) -> &Window {
        match self {
            Layout::Leaf(w) if i == 0 => w,
            Layout::Leaf(_) => panic!("window index out of range (I-19)"),
            Layout::Split { first, second, .. } => {
                let k = first.leaf_count();
                if i < k {
                    first.leaf(i)
                } else {
                    second.leaf(i - k)
                }
            }
        }
    }

    /// Replaces the window at layout index `i` by the layout `f` builds from it.
    /// Panics when `i` is out of range (I-19).
    pub fn replace_leaf(self, i: usize, f: impl FnOnce(Window) -> Layout) -> Layout {
        match self {
            Layout::Leaf(w) if i == 0 => f(w),
            Layout::Leaf(_) => panic!("window index out of range (I-19)"),
            Layout::Split { dir, first, second } => {
                let k = first.leaf_count();
                if i < k {
                    Layout::Split {
                        dir,
                        first: Box::new((*first).replace_leaf(i, f)),
                        second,
                    }
                } else {
                    Layout::Split {
                        dir,
                        first,
                        second: Box::new((*second).replace_leaf(i - k, f)),
                    }
                }
            }
        }
    }

    /// Replaces the window at layout index `i`. Panics when `i` is out of range (I-19).
    pub fn set_leaf(self, i: usize, w: Window) -> Layout {
        self.replace_leaf(i, |_| Layout::Leaf(w))
    }

    /// Applies `f` to every window. Edits use this to move anchors (I-01).
    pub fn map_windows(self, f: &impl Fn(Window) -> Window) -> Layout {
        match self {
            Layout::Leaf(w) => Layout::Leaf(f(w)),
            Layout::Split { dir, first, second } => Layout::Split {
                dir,
                first: Box::new((*first).map_windows(f)),
                second: Box::new((*second).map_windows(f)),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    pub id: BufferId,
    pub name: String,
    pub text: Text,
    pub backing: Backing,
    pub undo: UndoLog,
    pub modified: bool,
    pub view_only: bool,
    pub mark: Option<usize>,
    /// Point when this buffer was last left by its window (anchor, I-01).
    pub saved_point: usize,
}

impl Buffer {
    pub fn new(id: BufferId, name: &str, text: Text, backing: Backing, undo_limit: usize) -> Self {
        Self {
            id,
            name: name.to_owned(),
            text,
            backing,
            undo: UndoLog::new(undo_limit),
            modified: false,
            view_only: false,
            mark: None,
            saved_point: 0,
        }
    }
}
/// Kind of a command, for commands that chain (§6, §7, I-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastCommand {
    Other,
    Insert {
        count: usize,
    },
    Undo {
        next: usize,
    },
    Kill,
    /// Goal display column for consecutive next-line and previous-line.
    Vertical {
        col: usize,
    },
    /// Bytes inserted by the last yank, for `yank-pop`.
    Yank {
        start: usize,
        end: usize,
    },
}

/// Key prefix waiting for its second key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    None,
    CtlX,
    Meta,
}

/// A line of input, read after a prompt label (§4, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub label: String,
    pub input: String,
    pub purpose: Purpose,
}

/// What a submitted prompt does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    ExecuteCommand,
    FindFile,
    WriteFile,
    SwitchBuffer,
    KillBuffer,
    GotoLine,
    ShellCommand,
    QueryFrom { regex: bool },
    QueryTo { regex: bool, from: String },
}

/// A yes-or-no question that must be answered before the command continues (I-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    SaveTruncated(BufferId),
    SaveExternal(BufferId),
    QuitUnsaved,
    KillModified(BufferId),
    RevertModified(BufferId),
}

/// An incremental search in progress (§9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isearch {
    pub forward: bool,
    pub regex: bool,
    pub text: String,
    /// Point when the search started; restored on cancel and used after backspace.
    pub origin: usize,
    /// Where the next incremental search starts: the start of the current match.
    pub anchor: usize,
    pub failed: bool,
}

/// A query-replace session in progress (§9, A8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryReplace {
    pub pattern: Pattern,
    pub replacement: String,
    pub regex: bool,
    pub fold: bool,
    /// The match waiting for an answer, as `start..end`.
    pub current: Option<(usize, usize)>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal { prefix: Prefix },
    Confirm(Confirm),
    Isearch(Isearch),
    QueryReplace(QueryReplace),
    Prompt(Prompt),
}

pub struct Editor {
    pub buffers: Vec<Buffer>,
    pub layout: Layout,
    pub selected: usize,
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
    next_id: u32,
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
        let scratch = Buffer::new(
            BufferId(0),
            "*scratch*",
            Text::default(),
            Backing::None,
            config.undo_limit,
        );
        Self {
            buffers: vec![scratch],
            layout: Layout::Leaf(Window {
                buffer: BufferId(0),
                point: 0,
                top: 0,
                left_col: 0,
            }),
            selected: 0,
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
            next_id: 1,
        }
    }

    pub fn window(&self) -> &Window {
        self.layout.leaf(self.selected)
    }

    /// Replaces the selected window with `f` applied to it (I-19).
    pub fn with_window(mut self, f: impl FnOnce(Window) -> Window) -> Self {
        let w = f(self.window().clone());
        self.layout = self.layout.set_leaf(self.selected, w);
        self
    }

    pub const fn leaf_count(&self) -> usize {
        self.layout.leaf_count()
    }

    pub fn buffer(&self) -> &Buffer {
        buffer_ref(&self.buffers, self.window().buffer)
    }

    /// Replaces the buffer with `id` by `f` applied to it. Panics when `id` is unknown (I-20).
    pub fn with_buffer(mut self, id: BufferId, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let i = buffer_index(&self.buffers, id);
        let buf = self.buffers.remove(i);
        self.buffers.insert(i, f(buf));
        self
    }

    /// Like `with_buffer`, for the buffer of the selected window.
    pub fn with_current_buffer(self, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let id = self.window().buffer;
        self.with_buffer(id, f)
    }

    /// Restores the content of the buffer visiting `path` from WAL bytes (I-23).
    pub fn restore_buffer_content(self, path: &std::path::Path, bytes: &[u8]) -> Self {
        let found = self
            .buffers
            .iter()
            .find(|b| match &b.backing {
                Backing::File { path: p, .. } => p == path,
                Backing::None => false,
            })
            .map(|b| b.id);
        if let Some(id) = found {
            self.with_buffer(id, |mut buf| {
                buf.text = Text::from_bytes(bytes);
                buf.modified = true;
                buf
            })
        } else {
            self
        }
    }

    /// Adds a buffer and returns its id. Ids are never reused.
    pub fn add_buffer(mut self, name: &str, text: Text, backing: Backing) -> (Self, BufferId) {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        self.buffers
            .push(Buffer::new(id, name, text, backing, self.config.undo_limit));
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
        let i = buffer_index(&self.buffers, id);
        let mut buf = self.buffers.remove(i);
        buf.text = edited(buf.text, &edit);
        buf.modified = true;

        let anchor = |a| snap(&buf.text, remap(&edit, a));
        self.layout = self.layout.map_windows(&|w| {
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
        self.buffers.insert(i, buf);
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
        buffer_ref(&self.buffers, id)
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
        let at = self.window().point;
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

fn buffer_index(buffers: &[Buffer], id: BufferId) -> usize {
    buffers
        .iter()
        .position(|b| b.id == id)
        .expect("window refers to a live buffer (I-08)")
}

fn buffer_ref(buffers: &[Buffer], id: BufferId) -> &Buffer {
    &buffers[buffer_index(buffers, id)]
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

/// Moves an anchor that landed inside a unit back to that unit's start.
pub fn snap(text: &Text, a: usize) -> usize {
    if text.is_boundary(a) {
        a
    } else {
        text.prev_boundary(a + 1)
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
        // Typing is one group, so the first undo removes the whole run.
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
        // Another command in between: the next undo undoes the previous undo.
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
        // Consecutive undo continues backward.
        let ed = undo(ed);
        assert_eq!(text(&ed), "");
    }

    #[test]
    fn anchors_follow_edit() {
        let mut ed = editor_with("abcdef");
        let scratch = ed.window().buffer;
        ed.layout = Layout::Split {
            dir: Dir::Below,
            first: Box::new(Layout::Leaf(Window {
                buffer: scratch,
                point: 5,
                top: 0,
                left_col: 0,
            })),
            second: Box::new(Layout::Leaf(Window {
                buffer: scratch,
                point: 2,
                top: 0,
                left_col: 0,
            })),
        };
        let ed = ed.with_current_buffer(|b| Buffer { mark: Some(4), ..b });
        let (ed, _) = ed.apply(Edit::Delete {
            pos: 0,
            bytes: b"abc".to_vec(),
        });

        let windows = ed.layout.windows();
        assert_eq!(windows[0].point, 2);
        assert_eq!(windows[1].point, 0);
        assert_eq!(ed.buffer().mark, Some(1));
    }

    #[test]
    fn update_leaf_changes_only_selected() {
        let mut ed = editor_with("abc");
        let scratch = ed.window().buffer;
        ed.layout = Layout::Split {
            dir: Dir::Right,
            first: Box::new(Layout::Leaf(Window {
                buffer: scratch,
                point: 1,
                top: 0,
                left_col: 0,
            })),
            second: Box::new(Layout::Leaf(Window {
                buffer: scratch,
                point: 2,
                top: 0,
                left_col: 0,
            })),
        };
        ed.selected = 1;
        let ed = ed.with_window(|w| Window { point: 3, ..w });
        let windows = ed.layout.windows();
        assert_eq!(windows[0].point, 1);
        assert_eq!(windows[1].point, 3);
    }

    #[test]
    fn with_buffer_updates_only_target() {
        let ed = editor_with("abc");
        let (ed, other) = ed.add_buffer("other", Text::from_bytes(b"xyz"), Backing::None);
        let ed = ed.with_buffer(other, |b| Buffer {
            modified: true,
            ..b
        });
        assert!(!ed.buffer_by_id(ed.window().buffer).modified);
        assert!(ed.buffer_by_id(other).modified);
        assert_eq!(ed.buffer_by_id(other).text.to_bytes(), b"xyz");
    }

    #[test]
    fn anchor_snaps_to_unit_start() {
        // "가" is E0 80 80 here, a three-byte unit; delete its middle byte.
        let ed = editor_with("가x").with_window(|w| Window { point: 3, ..w });
        let (ed, _) = ed.apply(Edit::Delete {
            pos: 1,
            bytes: b"\xAC\x80".to_vec(),
        });
        assert!(ed.buffer().text.is_boundary(ed.window().point));
    }

    #[test]
    fn keys_route_through_prefixes() {
        let ed = editor_with("");
        let ed = press(ed, Key::plain(Code::Esc));
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::Meta
            }
        );
        let ed = press(ed, Key::plain(Code::Char('z')));
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
        assert_eq!(ed.message, "M-z is undefined");

        let ed = press(ed, Key::ctrl('x'));
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::CtlX
            }
        );
        let ed = press(ed, Key::ctrl('q'));
        assert_eq!(ed.message, "C-x C-q is undefined");
    }

    #[test]
    fn keys_type_and_undo() {
        let ed = editor_with("");
        let ed = press(ed, Key::plain(Code::Char('a')));
        let ed = press(ed, Key::plain(Code::Enter));
        assert_eq!(text(&ed), "a\n");
        let ed = press(ed, Key::ctrl('/'));
        assert_eq!(text(&ed), "");
    }

    #[test]
    fn view_mode_blocks_edits() {
        let ed = editor_with("keep").with_current_buffer(|b| Buffer {
            view_only: true,
            ..b
        });
        let ed = type_char(ed, 'x');
        assert_eq!(text(&ed), "keep");
        assert_eq!(ed.message, "Buffer is read-only");
    }
}
