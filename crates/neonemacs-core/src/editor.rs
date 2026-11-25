//! Editor state.
//!
//! `apply` is the single edit point: it enforces the read-only
//! flag (I-06), records undo (§7), and moves every anchor to a boundary (I-01).
//! Every transition takes the editor by value and returns the new one.

use alloc::borrow::ToOwned;
use alloc::string::String;
mod documents;
mod edit;
mod interaction;
mod settings;

use crate::buffer::{BufferPool, RecoveredDelta};
use crate::config::{Config, Toggles};
use crate::effect::{Effect, WalTarget};
use crate::eol::Eol;
use crate::file::{Backing, FilePath};
use crate::keymap::{KeyAliases, Keymap};
use crate::syntax::SyntaxRegistry;
use crate::weave_tree::Doc;
use crate::window::WindowManager;
use documents::Documents;
pub use documents::KillRing;
use interaction::Interaction;
pub use interaction::{
    Confirm, Direction, Isearch, LastCommand, MatchKind, Mode, Prefix, Prompt, Purpose,
    QueryReplace, Question,
};
pub use settings::CommandRegistry;
use settings::Settings;

pub use crate::buffer::{Buffer, BufferId};
pub use crate::window::{Dir, Layout, Window};

/// The echo-area message that cancelling (`C-g`, `Esc`) leaves. The shell reads it to tell that nothing
/// else has replaced the spinner.
pub const CANCEL_MESSAGE: &str = "Quit";

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
            height: 0,
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

    /// Keeps the unsaved text of the scratch buffer in `slot`. `None` keeps it in memory only (I-43).
    #[must_use]
    pub fn with_scratch_slot(mut self, slot: Option<String>) -> Self {
        self.documents.scratch_slot = slot;
        self
    }

    #[must_use]
    pub fn scratch_slot(&self) -> Option<&str> {
        self.documents.scratch_slot.as_deref()
    }

    /// Where the WAL of buffer `id` lives, if it can have one: beside its file, or the scratch slot.
    #[must_use]
    pub fn wal_target(&self, id: BufferId) -> Option<WalTarget> {
        match self.buffer_by_id(id).backing() {
            Backing::File { path, .. } => Some(WalTarget::Sibling(path.as_str().to_owned())),
            Backing::Scratch { .. } => self.scratch_slot().map(|s| WalTarget::Slot(s.to_owned())),
            Backing::None => None,
        }
    }

    /// The effect that removes the WAL of buffer `id`, if it can have one.
    #[must_use]
    pub fn remove_wal_effect(&self, id: BufferId) -> Option<Effect> {
        self.wal_target(id)
            .map(|target| Effect::RemoveWal { target })
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

    /// Adds `note` after the message already shown, joined by `; `. The only place that joins
    /// notices, so none replaces an earlier one (I-56).
    #[must_use]
    pub fn append_message(mut self, note: impl Into<String>) -> Self {
        let note = note.into();
        if self.interaction.message.is_empty() {
            self.interaction.message = note;
        } else {
            self.interaction.message.push_str("; ");
            self.interaction.message.push_str(&note);
        }
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
    pub fn confirm(self, kind: Confirm, message: impl Into<String>) -> Self {
        let text = message.into();
        self.set_mode(Mode::Confirm(Question {
            kind,
            text: text.clone(),
        }))
        .set_message(text)
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
    pub fn set_config(self, config: Config) -> Self {
        self.update_settings(|settings| Settings { config, ..settings })
    }

    pub(crate) fn tracks_wal_deltas(&self) -> bool {
        self.config().toggles.contains(Toggles::WAL_DELTAS)
    }

    fn update_settings(mut self, f: impl FnOnce(Settings) -> Settings) -> Self {
        let tracked = self.tracks_wal_deltas();
        self.settings = f(self.settings);
        if tracked && !self.tracks_wal_deltas() {
            self.documents.buffers = self.documents.buffers.clear_wal_deltas();
        }
        self
    }

    /// Sets the directory that relative file paths resolve against (I-37).
    #[must_use]
    pub fn with_cwd(mut self, cwd: String) -> Self {
        self.settings.cwd = cwd;
        self
    }

    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.settings.cwd
    }

    /// Takes the settings of `from` and keeps everything else: buffers, windows, kill ring, and
    /// the interaction state (I-58).
    #[must_use]
    pub fn adopt_settings(self, from: Self) -> Self {
        self.update_settings(|_| from.settings)
    }

    /// The absolute path that `input` names, resolved against the working directory (I-37).
    #[must_use]
    pub fn resolve_path(&self, input: &str) -> FilePath {
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
    pub const fn key_aliases(&self) -> &KeyAliases {
        &self.settings.key_aliases
    }

    #[must_use]
    pub fn with_key_aliases(mut self, f: impl FnOnce(KeyAliases) -> KeyAliases) -> Self {
        self.settings.key_aliases = f(self.settings.key_aliases);
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
            point: text.floor_boundary(w.point),
            top: text.floor_boundary(w.top),
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
    pub fn replace_text(self, id: BufferId, text: Doc) -> Self {
        self.with_buffer(id, |b| b.with_text(text))
            .snap_windows_for_buffer(id)
    }

    /// Snaps `point` and resets `top` for every window displaying buffer `id` (I-01).
    #[must_use]
    pub fn snap_windows_for_buffer(self, id: BufferId) -> Self {
        self.reset_windows_for_buffer(id, |w| w.point)
    }

    /// Moves every window displaying buffer `id` to `point(window)`, snapped, with `top` reset.
    fn reset_windows_for_buffer(mut self, id: BufferId, point: impl Fn(&Window) -> usize) -> Self {
        let text = self.documents.buffers.get_by_id(id).text();
        self.documents.windows = self.documents.windows.map_windows(&|w| {
            if w.buffer == id {
                Window {
                    point: text.floor_boundary(point(&w)),
                    top: 0,
                    ..w
                }
            } else {
                w
            }
        });
        self
    }

    /// Restores the content of the buffer visiting `path` from a WAL snapshot (I-23).
    #[must_use]
    pub fn restore_buffer_content(
        mut self,
        path: &str,
        doc: Doc,
        eol: Eol,
        line: Option<usize>,
    ) -> Self {
        let (buffers, id) = self.documents.buffers.restore_content(path, doc, eol, line);
        self.documents.buffers = buffers;
        self.snap_recovered_windows(id, line)
    }

    /// Installs the replayed delta WAL for a buffer visiting `path` (I-23).
    #[must_use]
    pub(crate) fn restore_buffer_deltas(
        mut self,
        path: &str,
        delta: RecoveredDelta,
        line: Option<usize>,
    ) -> Self {
        let (buffers, id) = self.documents.buffers.restore_deltas(path, delta, line);
        self.documents.buffers = buffers;
        self.snap_recovered_windows(id, line)
    }

    fn snap_recovered_windows(self, id: Option<BufferId>, line: Option<usize>) -> Self {
        match id {
            Some(id) if line.is_some() => {
                let target = self.documents.buffers.get_by_id(id).saved_point();
                self.reset_windows_for_buffer(id, |_| target)
            }
            Some(id) => self.snap_windows_for_buffer(id),
            None => self,
        }
    }

    /// Puts the text of a recovered scratch WAL into the scratch buffer, as unsaved text (I-23).
    #[must_use]
    pub fn restore_scratch(self, doc: Doc, eol: Eol) -> Self {
        let Some(id) = self
            .documents
            .buffers
            .iter()
            .find(|b| b.backing().is_scratch())
            .map(Buffer::id)
        else {
            return self;
        };
        self.with_buffer(id, |b| {
            b.with_text(doc)
                .set_eol(eol)
                .mark_modified()
                .clear_wal_dirty()
        })
        .snap_windows_for_buffer(id)
    }

    /// Adds a buffer and returns its id. Ids are never reused.
    #[must_use]
    pub fn add_buffer(mut self, name: &str, text: Doc, backing: Backing) -> (Self, BufferId) {
        let (buffers, id) = self.documents.buffers.add(name, text, backing);
        self.documents.buffers = buffers;
        let eol = self.settings.config.default_eol;
        let track = self.tracks_wal_deltas();
        let ed = self.with_buffer(id, |b| b.set_eol(eol).track_wal_baseline(track));
        (ed, id)
    }

    /// Moves point to `pos`, snapped to a unit boundary (I-01).
    #[must_use]
    pub fn move_point(self, pos: usize) -> Self {
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
    use crate::keymap::{Code, Key};
    use crate::test_support::{press, text};

    fn editor_with(s: &str) -> Editor {
        Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(s.as_bytes())))
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
            editor_with("first").add_buffer("b2", Doc::from_bytes(b"second"), Backing::None);
        let ed = ed.with_buffer(b2, Buffer::mark_modified);
        assert!(!ed.buffer_by_id(BufferId(0)).modified());
        assert!(ed.buffer_by_id(b2).modified());
    }

    #[test]
    fn new_buffers_take_the_default_eol() {
        let ed = Editor::new(Config {
            default_eol: Eol::Crlf,
            ..Config::default()
        });
        let (ed, id) = ed.add_buffer("n", Doc::default(), Backing::None);
        assert_eq!(ed.buffer_by_id(id).eol(), Eol::Crlf);
    }

    #[test]
    fn kill_ring_operations() {
        let kr = KillRing::new();
        assert!(kr.is_empty());
        assert_eq!(kr.len(), 0);
        assert_eq!(kr.last(), None);

        let kr = kr.push(Doc::from_bytes(b"hello"), 120);
        assert_eq!(kr.len(), 1);
        assert_eq!(kr.last(), Some(&Doc::from_bytes(b"hello")));

        let kr = kr.append_last(Doc::from_bytes(b" world"));
        assert_eq!(kr.len(), 1);
        assert_eq!(kr.last(), Some(&Doc::from_bytes(b"hello world")));

        let kr = kr.push(Doc::from_bytes(b"second"), 120);
        assert_eq!(kr.len(), 2);
        assert_eq!(kr.last(), Some(&Doc::from_bytes(b"second")));

        let kr = kr.rotate_right(1);
        assert_eq!(kr.last(), Some(&Doc::from_bytes(b"hello world")));
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

        let ed = ed.with_kill_ring(|kr| kr.push(Doc::from_bytes(b"sample"), 120));
        assert_eq!(ed.kill_ring().last(), Some(&Doc::from_bytes(b"sample")));

        let ed = ed.with_keymap(|km| km.bind_str("C-x 2", "split-window-below").unwrap());
        let key = crate::keymap::parse_key_seq("C-x 2").unwrap();
        assert_eq!(ed.keymap().lookup(&key), Some("split-window-below"));
    }

    #[test]
    fn append_message_joins_notices_in_order() {
        // I-56
        let ed = Editor::new(Config::default()).append_message("first");
        assert_eq!(ed.message(), "first");
        let ed = ed.append_message("second").append_message("third");
        assert_eq!(ed.message(), "first; second; third");
    }

    #[test]
    fn adopt_settings_moves_settings_and_keeps_documents() {
        // I-58
        let ed = editor_with("kept").set_message("kept message");
        let from = Editor::new(Config {
            fill_column: 85,
            ..Config::default()
        })
        .with_cwd("elsewhere".to_owned())
        .with_keymap(|km| km.bind_str("C-x 9", "forward-char").unwrap())
        .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"dropped")));
        let ed = ed.adopt_settings(from);
        assert_eq!(ed.config().fill_column, 85);
        assert_eq!(ed.cwd(), "elsewhere");
        let key = crate::keymap::parse_key_seq("C-x 9").unwrap();
        assert_eq!(ed.keymap().lookup(&key), Some("forward-char"));
        assert_eq!(text(&ed), "kept");
        assert_eq!(ed.message(), "kept message");
    }

    #[test]
    fn with_windows_and_snap_windows_for_buffer() {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes("가나다라".as_bytes())))
            .with_window(|w| Window {
                point: 9,
                top: 6,
                ..w
            })
            .with_windows(|wm| wm.split(Dir::Below));
        let id = ed.window().buffer;
        let ed = ed
            .with_buffer(id, |b| b.with_text(Doc::from_bytes("가".as_bytes())))
            .snap_windows_for_buffer(id);
        assert_eq!(ed.window().point, 3);
        assert_eq!(ed.window().top, 0);
    }
}
