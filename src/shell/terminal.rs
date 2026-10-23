//! Terminal input: converts a crossterm key event into the editor's `Key` (I-12).

use crate::core::keymap::{Code, Key, ctrl_key};
use crossterm::event::{KeyCode, KeyModifiers};

/// Canonical key for a terminal event (I-12). Several terminal encodings
/// map to one key: `C-_`, `C-/`, and `C-7` all mean undo; `C-SPC`, `C-@`,
/// and `C-2` all mean mark.
pub fn from_terminal(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
    let meta = mods.contains(KeyModifiers::ALT);
    let key = match code {
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => ctrl_key(c),
        KeyCode::Char(c) => Key::plain(Code::Char(c)),
        KeyCode::Enter => Key::plain(Code::Enter),
        KeyCode::Tab => Key::plain(Code::Tab),
        KeyCode::Backspace => Key::plain(Code::Backspace),
        KeyCode::Esc => Key::plain(Code::Esc),
        KeyCode::Left => Key::plain(Code::Left),
        KeyCode::Right => Key::plain(Code::Right),
        KeyCode::Up => Key::plain(Code::Up),
        KeyCode::Down => Key::plain(Code::Down),
        KeyCode::Home => Key::plain(Code::Home),
        KeyCode::End => Key::plain(Code::End),
        _ => return None,
    };
    Some(if meta { key.with_meta() } else { key })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keymap::parse_keys;
    use crossterm::event::KeyModifiers as M;

    fn norm(c: KeyCode, m: M) -> Key {
        from_terminal(c, m).unwrap()
    }

    #[test]
    fn normalize() {
        let undo = parse_keys("C-_").unwrap()[0];
        assert_eq!(norm(KeyCode::Char('7'), M::CONTROL), undo);
        assert_eq!(norm(KeyCode::Char('/'), M::CONTROL), undo);
        assert_eq!(norm(KeyCode::Char('_'), M::CONTROL), undo);

        let mark = parse_keys("C-SPC").unwrap()[0];
        assert_eq!(norm(KeyCode::Char(' '), M::CONTROL), mark);
        assert_eq!(norm(KeyCode::Char('@'), M::CONTROL), mark);
        assert_eq!(norm(KeyCode::Char('2'), M::CONTROL), mark);

        assert_eq!(
            norm(KeyCode::Char('x'), M::ALT),
            parse_keys("M-x").unwrap()[0]
        );
        assert_eq!(norm(KeyCode::Left, M::NONE), parse_keys("left").unwrap()[0]);
    }
}
