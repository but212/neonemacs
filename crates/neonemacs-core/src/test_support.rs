//! Helpers for unit tests inside the core. They drive `step` and never perform an effect: a
//! transition that asks the shell for anything panics, so such a test belongs in `tests/`,
//! where the shell performs the effects (I-14, I-15).

use crate::config::Config;
use crate::dispatch;
use crate::editor::{BufferId, Editor};
use crate::effect::{Effect, Effects, Event};
use crate::eol::Eol;
use crate::file::{Backing, DiskState, FilePath};
use crate::keymap::{Code, Key, parse_keys};
use crate::step;
use crate::text::Text;
use std::path::Path;

fn no_effects(ed: Editor, fx: Effects) -> Editor {
    assert!(fx.is_empty(), "effect needs shell: {fx:?}");
    ed
}

/// A fresh editor with the default configuration.
#[must_use]
pub fn editor() -> Editor {
    Editor::new(Config::default())
}

/// Handles a key.
///
/// # Panics
///
/// Panics if the key causes an effect.
#[must_use]
pub fn press(ed: Editor, key: Key) -> Editor {
    let (ed, fx) = step(ed, Event::Key(key));
    no_effects(ed, fx)
}

/// Presses each key of `s`, in `parse_keys` notation such as `C-x C-s`.
///
/// # Panics
///
/// Panics if `s` contains an invalid key notation or a key causes an effect.
#[must_use]
pub fn keys(mut ed: Editor, s: &str) -> Editor {
    for k in parse_keys(s).unwrap() {
        ed = press(ed, k);
    }
    ed
}

/// Presses each key of `s` like `keys`, and returns the effects the keys caused instead of
/// refusing them.
///
/// # Panics
///
/// Panics if `s` contains an invalid key notation.
#[must_use]
pub fn keys_with_effects(mut ed: Editor, s: &str) -> (Editor, Effects) {
    let mut all = Vec::new();
    for k in parse_keys(s).unwrap() {
        let (next, fx) = step(ed, Event::Key(k));
        ed = next;
        all.extend(fx);
    }
    (ed, all)
}

/// Types `s` as plain characters. A newline is RET.
///
/// # Panics
///
/// Panics if a key causes an effect.
#[must_use]
pub fn typ(mut ed: Editor, s: &str) -> Editor {
    for c in s.chars() {
        let code = if c == '\n' {
            Code::Enter
        } else {
            Code::Char(c)
        };
        ed = press(ed, Key::plain(code));
    }
    ed
}

/// Sets the mark at point the way a user without a key for it does: `M-x set-mark-command RET`.
#[must_use]
pub fn set_mark(ed: Editor) -> Editor {
    keys(typ(keys(ed, "M-x"), "set-mark-command"), "RET")
}

/// Runs a command.
///
/// # Panics
///
/// Panics if the command causes an effect.
pub fn run_command(ed: Editor, run: impl FnOnce(Editor, Effects) -> (Editor, Effects)) -> Editor {
    let (ed, fx) = dispatch::command(ed, Vec::new(), run);
    no_effects(ed, fx)
}

/// The text of the selected buffer.
#[must_use]
pub fn text(ed: &Editor) -> String {
    String::from_utf8_lossy(&ed.buffer().text().to_bytes()).into_owned()
}

/// Point of the selected window.
#[must_use]
pub fn point(ed: &Editor) -> usize {
    ed.window().point
}

/// Replaces the selected buffer with an unmodified file-backed buffer holding `bytes`.
pub(crate) fn file_buffer(ed: Editor, bytes: &[u8], eol: Eol) -> Editor {
    ed.with_current_buffer(|b| {
        b.with_text(Text::from_bytes(bytes))
            .set_name("f.txt")
            .set_backing(Backing::file(
                FilePath::resolve(Path::new(""), Path::new("f.txt")),
                DiskState::Unknown,
            ))
            .set_eol(eol)
            .mark_clean()
    })
}

/// The bytes that saving buffer `id` would write.
pub(crate) fn saved_bytes(ed: &Editor, id: BufferId) -> Vec<u8> {
    let fx = crate::commands::file::write_buffer(ed, id, Vec::new());
    let Effect::WriteFile { bytes, .. } = &fx[0] else {
        panic!("expected WriteFile");
    };
    bytes.clone()
}

pub(crate) fn known(size: u64) -> DiskState {
    DiskState::Known {
        mtime: std::time::UNIX_EPOCH,
        size,
    }
}
