//! Helpers shared by unit tests. Each one drives the editor the way the event loop does (§5, I-15).

use crate::core::config::Config;
use crate::core::editor::Editor;
use crate::core::effect::{Effects, Event};
use crate::core::keymap::{Code, Key, parse_keys};
use crate::shell::{handle, perform_all};

/// A fresh editor with the default configuration.
pub fn editor() -> Editor {
    Editor::new(Config::default())
}

/// Handles a key and performs its effects.
pub fn press(ed: Editor, key: Key) -> Editor {
    handle(ed, None, Event::Key(key))
}

/// Presses each key of `s`, in `parse_keys` notation such as `C-x C-s`.
pub fn keys(mut ed: Editor, s: &str) -> Editor {
    for k in parse_keys(s).unwrap() {
        ed = press(ed, k);
    }
    ed
}

/// Types `s` as plain characters. A newline is RET.
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

/// Runs a command and performs its effects.
pub fn run_command(ed: Editor, run: impl FnOnce(Editor, Effects) -> (Editor, Effects)) -> Editor {
    let (ed, fx) = crate::core::dispatch::command(ed, Vec::new(), run);
    perform_all(ed, None, fx)
}

/// The text of the selected buffer.
pub fn text(ed: &Editor) -> String {
    String::from_utf8_lossy(&ed.buffer().text.to_bytes()).into_owned()
}

/// Point of the selected window.
pub fn point(ed: &Editor) -> usize {
    ed.window().point
}
