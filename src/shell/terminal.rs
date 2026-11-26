//! Terminal input and mode management: converts a crossterm key event into the editor's `Key` (I-12)
//! and restores raw mode/alternate screen on drop or panic (§13).

use crate::core::{Code, Key};
use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use std::io;
use std::sync::Once;

/// Enters raw mode and the alternate screen.
fn enter_terminal() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    Ok(())
}

/// Puts the terminal back as it was. Safe to call more than once.
fn restore_terminal() {
    let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Restores the terminal when dropped, including on early return or panic (§13).
pub struct TerminalGuard;

impl TerminalGuard {
    /// Enters raw mode and the alternate screen.
    ///
    /// # Errors
    /// Returns `io::Error` if enabling raw mode or entering alternate screen fails.
    pub fn enter() -> io::Result<Self> {
        Self::install_panic_hook();
        enter_terminal()?;
        Ok(Self)
    }

    /// Temporarily leaves raw mode and the alternate screen to suspend (`SIGTSTP`), then re-enters.
    ///
    /// # Errors
    /// Returns `io::Error` if raising `SIGTSTP` or re-entering raw mode / the alternate screen fails.
    #[cfg(unix)]
    pub fn suspend(&mut self) -> io::Result<()> {
        restore_terminal();
        let res = nix::sys::signal::raise(nix::sys::signal::Signal::SIGTSTP).map_err(io::Error::from);
        enter_terminal()?;
        res
    }

    fn install_panic_hook() {
        static PANIC_HOOK: Once = Once::new();
        PANIC_HOOK.call_once(|| {
            let default = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                default(info);
            }));
        });
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Canonical key for a terminal event (I-12). Several terminal encodings
/// map to one key: `C-_`, `C-/`, and `C-7` all mean undo.
#[must_use]
pub const fn from_terminal(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
    let meta = mods.contains(KeyModifiers::ALT);
    let key = match code {
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => Key::ctrl(c),
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
    use crate::core::parse_keys;
    use crossterm::event::{KeyModifiers as M, MediaKeyCode, ModifierKeyCode};

    fn norm(c: KeyCode, m: M) -> Key {
        from_terminal(c, m).unwrap()
    }

    #[test]
    fn normalize() {
        let undo = parse_keys("C-_").unwrap()[0];
        assert_eq!(norm(KeyCode::Char('7'), M::CONTROL), undo);
        assert_eq!(norm(KeyCode::Char('/'), M::CONTROL), undo);
        assert_eq!(norm(KeyCode::Char('_'), M::CONTROL), undo);

        assert_eq!(
            norm(KeyCode::Char('x'), M::ALT),
            parse_keys("M-x").unwrap()[0]
        );
        assert_eq!(norm(KeyCode::Left, M::NONE), parse_keys("left").unwrap()[0]);
    }

    #[test]
    fn reject_unmapped_keys_under_hostile_modifiers() {
        // Any unmapped KeyCode must return None regardless of modifier bitmask
        let unmapped_codes = [
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Insert,
            KeyCode::Delete,
            KeyCode::BackTab,
            KeyCode::Null,
            KeyCode::CapsLock,
            KeyCode::ScrollLock,
            KeyCode::NumLock,
            KeyCode::PrintScreen,
            KeyCode::Pause,
            KeyCode::Menu,
            KeyCode::KeypadBegin,
            KeyCode::F(0),
            KeyCode::F(1),
            KeyCode::F(12),
            KeyCode::F(255),
            KeyCode::Media(MediaKeyCode::Play),
            KeyCode::Modifier(ModifierKeyCode::LeftControl),
        ];

        let hostile_modifiers = [
            M::NONE,
            M::CONTROL,
            M::ALT,
            M::SHIFT,
            M::CONTROL | M::ALT,
            M::CONTROL | M::ALT | M::SHIFT,
            M::SUPER,
            M::HYPER,
            M::META,
            M::all(),
        ];

        for &code in &unmapped_codes {
            for &mods in &hostile_modifiers {
                assert_eq!(
                    from_terminal(code, mods),
                    None,
                    "Expected None for unmapped code {code:?} with mods {mods:?}"
                );
            }
        }
    }

    #[test]
    fn all_supported_non_char_keys_and_modifiers() {
        let cases = [
            (KeyCode::Enter, Code::Enter),
            (KeyCode::Tab, Code::Tab),
            (KeyCode::Backspace, Code::Backspace),
            (KeyCode::Esc, Code::Esc),
            (KeyCode::Left, Code::Left),
            (KeyCode::Right, Code::Right),
            (KeyCode::Up, Code::Up),
            (KeyCode::Down, Code::Down),
            (KeyCode::Home, Code::Home),
            (KeyCode::End, Code::End),
        ];

        for (key_code, expected_code) in cases {
            // 1. Plain
            let plain = from_terminal(key_code, M::NONE).expect("plain should be mapped");
            assert_eq!(plain, Key::plain(expected_code));
            assert!(!plain.ctrl);
            assert!(!plain.meta);

            // 2. Alt (Meta)
            let meta = from_terminal(key_code, M::ALT).expect("alt should be mapped");
            assert_eq!(meta, Key::plain(expected_code).with_meta());
            assert!(!meta.ctrl);
            assert!(meta.meta);

            // 3. Control: non-char keys do not bind Control in from_terminal (ctrl remains false)
            let ctrl = from_terminal(key_code, M::CONTROL).expect("control should be mapped");
            assert_eq!(ctrl, Key::plain(expected_code));
            assert!(!ctrl.ctrl);

            // 4. Control + Alt: meta is preserved, ctrl remains false for non-char keys
            let ctrl_alt =
                from_terminal(key_code, M::CONTROL | M::ALT).expect("ctrl+alt should be mapped");
            assert_eq!(ctrl_alt, Key::plain(expected_code).with_meta());
            assert!(!ctrl_alt.ctrl);
            assert!(ctrl_alt.meta);

            // 5. Shift modifier alone should not set ctrl or meta
            let shifted = from_terminal(key_code, M::SHIFT).expect("shift should be mapped");
            assert_eq!(shifted, Key::plain(expected_code));
            assert!(!shifted.ctrl);
            assert!(!shifted.meta);
        }
    }

    #[test]
    fn character_keys_compound_modifiers_and_edge_unicodes() {
        // C-M compound keys
        let cm_f = from_terminal(KeyCode::Char('f'), M::CONTROL | M::ALT).unwrap();
        assert_eq!(cm_f, parse_keys("C-M-f").unwrap()[0]);
        assert!(cm_f.ctrl);
        assert!(cm_f.meta);

        // C-M on undo aliases
        let cm_undo = from_terminal(KeyCode::Char('7'), M::CONTROL | M::ALT).unwrap();
        assert_eq!(cm_undo, parse_keys("C-M-_").unwrap()[0]);

        // Case folding under Control: C-A should equal C-a
        let ca_upper = from_terminal(KeyCode::Char('A'), M::CONTROL).unwrap();
        let ca_lower = from_terminal(KeyCode::Char('a'), M::CONTROL).unwrap();
        assert_eq!(ca_upper, ca_lower);
        assert_eq!(ca_upper, parse_keys("C-a").unwrap()[0]);

        // Multibyte Unicode characters
        let hangul = from_terminal(KeyCode::Char('가'), M::NONE).unwrap();
        assert_eq!(hangul, Key::plain(Code::Char('가')));

        let hangul_meta = from_terminal(KeyCode::Char('가'), M::ALT).unwrap();
        assert_eq!(hangul_meta, Key::plain(Code::Char('가')).with_meta());

        let emoji = from_terminal(KeyCode::Char('🦀'), M::NONE).unwrap();
        assert_eq!(emoji, Key::plain(Code::Char('🦀')));

        // Boundary/control chars as Char
        let nul = from_terminal(KeyCode::Char('\0'), M::NONE).unwrap();
        assert_eq!(nul, Key::plain(Code::Char('\0')));

        let max_unicode = from_terminal(KeyCode::Char('\u{10FFFF}'), M::NONE).unwrap();
        assert_eq!(max_unicode, Key::plain(Code::Char('\u{10FFFF}')));

        // Irrelevant flags (SUPER, HYPER, META) without CONTROL or ALT must remain plain
        let super_x = from_terminal(KeyCode::Char('x'), M::SUPER).unwrap();
        assert_eq!(super_x, Key::plain(Code::Char('x')));
        assert!(!super_x.ctrl);
        assert!(!super_x.meta);
    }

    #[test]
    fn fuzz_ascii_and_modifiers_never_panics() {
        let modifier_combinations = [
            M::NONE,
            M::CONTROL,
            M::ALT,
            M::SHIFT,
            M::SUPER,
            M::HYPER,
            M::META,
            M::CONTROL | M::ALT,
            M::CONTROL | M::SHIFT,
            M::ALT | M::SHIFT,
            M::all(),
        ];

        // Sweep full ASCII character space
        for b in 0..=127u8 {
            let ch = b as char;
            for &mods in &modifier_combinations {
                let res = from_terminal(KeyCode::Char(ch), mods);
                assert!(res.is_some(), "ASCII char '{ch:?}' must map to a Key");
                let key = res.unwrap();
                if mods.contains(M::ALT) {
                    assert!(key.meta);
                }
                if mods.contains(M::CONTROL) {
                    assert!(key.ctrl);
                }
            }
        }
    }
}
