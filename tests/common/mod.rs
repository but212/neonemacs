//! Shared test helpers for integration tests (§2, ADR 0002).

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::effect::Event;
use neonemacs::core::keymap::{Code, Key, parse_keys};
use neonemacs::shell::handle;

#[allow(dead_code)]
pub fn press(code: KeyCode, modifiers: KeyModifiers) -> TermEvent {
    TermEvent::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

#[allow(dead_code)]
pub fn editor() -> Editor {
    Editor::new(Config::default())
}

#[allow(dead_code)]
pub fn keys(mut ed: Editor, s: &str) -> Editor {
    for k in parse_keys(s).unwrap() {
        ed = handle(ed, None, Event::Key(k));
    }
    ed
}

#[allow(dead_code)]
pub fn typ(mut ed: Editor, s: &str) -> Editor {
    for c in s.chars() {
        let code = if c == '\n' {
            Code::Enter
        } else {
            Code::Char(c)
        };
        ed = handle(ed, None, Event::Key(Key::plain(code)));
    }
    ed
}

#[allow(dead_code)]
pub fn text(ed: &Editor) -> String {
    String::from_utf8_lossy(&ed.buffer().text().to_bytes()).into_owned()
}

#[allow(dead_code)]
pub fn point(ed: &Editor) -> usize {
    ed.window().point
}
