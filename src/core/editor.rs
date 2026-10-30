//! Editor state. `apply` is the single edit point: it enforces the read-only
//! flag (I-06), records undo (§7), and moves every anchor to a boundary (I-01).
//! Every transition takes the editor by value and returns the new one.

mod documents;
mod edit;
mod interaction;
mod settings;

use crate::core::buffer::BufferPool;
use crate::core::commands::CommandRegistry;
use crate::core::config::Config;
use crate::core::eol::Eol;
use crate::core::file::{Backing, FilePath};
use crate::core::keymap::Keymap;
pub use crate::core::kill_ring::KillRing;
use crate::core::syntax::SyntaxRegistry;
use crate::core::text::Text;
use crate::core::window::WindowManager;
use documents::Documents;
use interaction::Interaction;
use settings::Settings;
use std::path::{Path, PathBuf};

pub use crate::core::buffer::{Buffer, BufferId};
pub use crate::core::mode::{
    Confirm, Direction, Isearch, LastCommand, MatchKind, Mode, Prefix, Prompt, Purpose,
    QueryReplace,
};
pub use crate::core::window::{Dir, Layout, Window, snap_point as snap};

/// Snaps an end-of-range offset to a unit boundary (moves to end of unit if mid-unit).
#[must_use]
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

impl Default for Editor {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Editor {
    #[must_use]
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

    #[must_use]
    pub const fn edit_seq(&self) -> u64 {
        self.documents.edit_seq
    }

    #[must_use]
    pub fn clear_wal_dirty(self, id: BufferId) -> Self {
        self.with_buffer(id, Buffer::clear_wal_dirty)
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.interaction.message
    }

    #[must_use]
    pub fn set_message(mut self, message: impl Into<String>) -> Self {
        self.interaction.message = message.into();
        self
    }

    #[must_use]
    pub fn clear_message(mut self) -> Self {
        self.interaction.message.clear();
        self
    }

    #[must_use]
    pub const fn mode(&self) -> &Mode {
        &self.interaction.mode
    }

    #[must_use]
    pub fn set_mode(mut self, mode: Mode) -> Self {
        self.interaction.mode = mode;
        self
    }

    #[must_use]
    pub fn confirm(self, question: Confirm, message: impl Into<String>) -> Self {
        self.set_mode(Mode::Confirm(question)).set_message(message)
    }

    #[must_use]
    pub const fn buffers(&self) -> &BufferPool {
        &self.documents.buffers
    }

    #[must_use]
    pub const fn windows(&self) -> &WindowManager {
        &self.documents.windows
    }

    #[must_use]
    pub fn with_windows(mut self, f: impl FnOnce(WindowManager) -> WindowManager) -> Self {
        self.documents.windows = f(self.documents.windows);
        self
    }

    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.settings.config
    }

    #[must_use]
    pub const fn set_config(mut self, config: Config) -> Self {
        self.settings.config = config;
        self
    }

    /// Sets the directory that relative file paths resolve against (I-37).
    #[must_use]
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.settings.cwd = cwd;
        self
    }

    /// The absolute path that `input` names, resolved against the working directory (I-37).
    #[must_use]
    pub fn resolve_path(&self, input: &Path) -> FilePath {
        FilePath::resolve(&self.settings.cwd, input)
    }

    #[must_use]
    pub const fn syntax_registry(&self) -> &SyntaxRegistry {
        &self.settings.syntax_registry
    }

    #[must_use]
    pub fn with_syntax_registry(
        mut self,
        f: impl FnOnce(SyntaxRegistry) -> SyntaxRegistry,
    ) -> Self {
        self.settings.syntax_registry = f(self.settings.syntax_registry);
        self
    }

    #[must_use]
    pub const fn commands(&self) -> &CommandRegistry {
        &self.settings.commands
    }

    #[must_use]
    pub fn register_command(mut self, name: impl Into<String>) -> Self {
        self.settings.commands = self.settings.commands.insert(name);
        self
    }

    #[must_use]
    pub const fn quit(&self) -> bool {
        self.interaction.quit
    }

    #[must_use]
    pub const fn set_quit(mut self, quit: bool) -> Self {
        self.interaction.quit = quit;
        self
    }

    #[must_use]
    pub const fn kill_ring(&self) -> &KillRing {
        &self.documents.kill_ring
    }

    #[must_use]
    pub fn with_kill_ring(mut self, f: impl FnOnce(KillRing) -> KillRing) -> Self {
        self.documents.kill_ring = f(self.documents.kill_ring);
        self
    }

    #[must_use]
    pub const fn last(&self) -> LastCommand {
        self.interaction.last
    }

    #[must_use]
    pub const fn set_last(mut self, last: LastCommand) -> Self {
        self.interaction.last = last;
        self
    }

    #[must_use]
    pub const fn prev(&self) -> LastCommand {
        self.interaction.prev
    }

    #[must_use]
    pub const fn keymap(&self) -> &Keymap {
        &self.settings.keymap
    }

    #[must_use]
    pub fn with_keymap(mut self, f: impl FnOnce(Keymap) -> Keymap) -> Self {
        self.settings.keymap = f(self.settings.keymap);
        self
    }

    #[must_use]
    pub fn window(&self) -> &Window {
        self.documents.windows.current()
    }

    /// Removes buffer `id`. Windows that showed it move to another buffer (I-08).
    /// Killing the last buffer leaves an empty `*scratch*` (A4).
    #[must_use]
    pub fn kill_buffer_id(mut self, id: BufferId) -> Self {
        let (buffers, replacement) = self.documents.buffers.kill(id);
        self.documents.buffers = buffers;
        let saved = self.buffer_by_id(replacement).saved_point();
        self.documents.windows = self.documents.windows.retarget(id, replacement, saved);
        self
    }

    /// Replaces the selected window with `f` applied to it (I-19).
    #[must_use]
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

    #[must_use]
    pub const fn leaf_count(&self) -> usize {
        self.documents.windows.leaf_count()
    }

    #[must_use]
    pub fn buffer(&self) -> &Buffer {
        self.documents.buffers.get_by_id(self.window().buffer)
    }

    /// Replaces the buffer with `id` by `f` applied to it. Panics when `id` is unknown (I-20).
    #[must_use]
    pub fn with_buffer(mut self, id: BufferId, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        self.documents.buffers = self.documents.buffers.with_buffer(id, f);
        self
    }

    /// Like `with_buffer`, for the buffer of the selected window.
    #[must_use]
    pub fn with_current_buffer(self, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let id = self.window().buffer;
        self.with_buffer(id, f)
    }

    /// Replaces the text of buffer `id` outside `apply`. The undo history restarts (I-34), and the
    /// windows showing it are snapped to the new text (I-01).
    #[must_use]
    pub fn replace_text(self, id: BufferId, text: Text) -> Self {
        self.with_buffer(id, |b| b.with_text(text))
            .snap_windows_for_buffer(id)
    }

    /// Snaps `point` and resets `top` for every window displaying buffer `id` (I-01).
    #[must_use]
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
    #[must_use]
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
            Some(id) => {
                if line.is_some() {
                    let target_point = self.documents.buffers.get_by_id(id).saved_point();
                    self.documents.windows = self.documents.windows.map_windows(&|w| {
                        if w.buffer == id {
                            Window {
                                point: target_point,
                                top: 0,
                                ..w
                            }
                        } else {
                            w
                        }
                    });
                    self
                } else {
                    self.snap_windows_for_buffer(id)
                }
            }
            None => self,
        }
    }

    /// Adds a buffer and returns its id. Ids are never reused.
    #[must_use]
    pub fn add_buffer(mut self, name: &str, text: Text, backing: Backing) -> (Self, BufferId) {
        let (buffers, id) = self.documents.buffers.add(name, text, backing);
        self.documents.buffers = buffers;
        (self, id)
    }

    /// Moves point to `pos`, snapped to a unit boundary (I-01).
    #[must_use]
    pub fn move_point(self, pos: usize) -> Self {
        let pos = snap(self.buffer().text(), pos);
        self.with_window(|w| Window { point: pos, ..w })
    }

    #[must_use]
    pub fn buffer_by_id(&self, id: BufferId) -> &Buffer {
        self.documents.buffers.get_by_id(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keymap::{Code, Key};
    use crate::test_support::{press, text};

    fn editor_with(s: &str) -> Editor {
        Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Text::from_bytes(s.as_bytes())))
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
