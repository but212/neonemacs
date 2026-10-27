//! Keys, their normalization from the terminal (I-12), the key-string parser,
//! and the survival-set bindings (§3).

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub meta: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct KeyError(pub String);

impl Key {
    #[must_use]
    pub const fn plain(code: Code) -> Key {
        Key {
            code,
            ctrl: false,
            meta: false,
        }
    }

    #[must_use]
    pub const fn ctrl(c: char) -> Key {
        ctrl_key(c)
    }

    #[must_use]
    pub const fn with_meta(self) -> Key {
        Key { meta: true, ..self }
    }

    /// The character this key inserts, if it is plain text.
    #[must_use]
    pub const fn text(self) -> Option<char> {
        match (self.code, self.ctrl, self.meta) {
            (Code::Char(c), false, false) => Some(c),
            (Code::Enter, false, false) => Some('\n'),
            (Code::Tab, false, false) => Some('\t'),
            _ => None,
        }
    }

    /// Emacs-style name, such as `C-x` or `M-x`.
    #[must_use]
    pub fn describe(self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("C-");
        }
        if self.meta {
            s.push_str("M-");
        }
        s.push_str(match self.code {
            Code::Char(' ') => "SPC",
            Code::Char(c) => return format!("{s}{c}"),
            Code::Enter => "RET",
            Code::Tab => "TAB",
            Code::Backspace => "DEL",
            Code::Esc => "ESC",
            Code::Left => "left",
            Code::Right => "right",
            Code::Up => "up",
            Code::Down => "down",
            Code::Home => "home",
            Code::End => "end",
        });
        s
    }
}

#[must_use]
pub const fn ctrl_key(c: char) -> Key {
    let code = match c.to_ascii_lowercase() {
        '_' | '/' | '7' => Code::Char('_'),
        ' ' | '@' | '2' => Code::Char(' '),
        other => Code::Char(other),
    };
    Key {
        code,
        ctrl: true,
        meta: false,
    }
}

/// Parses key tokens such as `"C-x C-f"`, `"M-x"`, `"C-SPC"`, or `"left"`.
///
/// # Errors
/// Returns `KeyError` if any token cannot be recognized as a valid key.
pub fn parse_keys(s: &str) -> Result<Vec<Key>, KeyError> {
    s.split_whitespace().map(parse_key).collect()
}

/// Parses a binding key sequence. Only single keys or C-x 2-key sequences are valid bindings (I-12).
///
/// # Errors
/// Returns `KeyError` if `s` is empty, contains unrecognized keys, or is an unsupported sequence.
pub fn parse_key_seq(s: &str) -> Result<Vec<Key>, KeyError> {
    let keys = parse_keys(s)?;
    match keys.as_slice() {
        [] => Err(KeyError("empty key sequence".to_owned())),
        [_] => Ok(keys),
        [k1, _] if *k1 == Key::ctrl('x') => Ok(keys),
        _ => Err(KeyError(format!(
            "unsupported key binding: '{s}' (only single keys or C-x prefix supported)"
        ))),
    }
}

fn parse_key(tok: &str) -> Result<Key, KeyError> {
    let mut rest = tok;
    let mut ctrl = false;
    let mut meta = false;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            meta = true;
            rest = r;
        } else {
            break;
        }
    }
    let code = match rest {
        "SPC" => Code::Char(' '),
        "RET" => Code::Enter,
        "TAB" => Code::Tab,
        "DEL" => Code::Backspace,
        "ESC" => Code::Esc,
        "left" => Code::Left,
        "right" => Code::Right,
        "up" => Code::Up,
        "down" => Code::Down,
        "home" => Code::Home,
        "end" => Code::End,
        _ => {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Code::Char(c),
                _ => return Err(KeyError(format!("unknown key: {tok}"))),
            }
        }
    };
    let key = match (code, ctrl) {
        (Code::Char(c), true) => ctrl_key(c),
        (_, true) => return Err(KeyError(format!("C- cannot apply to: {tok}"))),
        (_, false) => Key::plain(code),
    };
    Ok(if meta { key.with_meta() } else { key })
}

/// Maps key sequences to command names.
#[derive(Debug, Default, Clone)]
pub struct Keymap {
    bindings: HashMap<Vec<Key>, String>,
}

impl Keymap {
    #[must_use]
    pub fn bind(mut self, keys: Vec<Key>, name: &str) -> Self {
        self.bindings.insert(keys, name.to_owned());
        self
    }

    /// Binds `keys` parsed from string notation to command `name`.
    ///
    /// # Errors
    /// Returns `KeyError` if `keys` notation cannot be parsed or is unsupported.
    pub fn bind_str(self, keys: &str, name: &str) -> Result<Self, KeyError> {
        Ok(self.bind(parse_key_seq(keys)?, name))
    }

    #[must_use]
    pub fn unbind(mut self, keys: &[Key]) -> Self {
        self.bindings.remove(keys);
        self
    }

    /// Key sequences bound to `name`, for display.
    #[must_use]
    pub fn keys_for(&self, name: &str) -> Vec<String> {
        let mut keys: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, n)| n.as_str() == name)
            .map(|(seq, _)| {
                seq.iter()
                    .map(|k| k.describe())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        keys.sort();
        keys
    }

    pub fn lookup(&self, keys: &[Key]) -> Option<&str> {
        self.bindings.get(keys).map(String::as_str)
    }

    /// The survival set (§3), plus `M-x` (§4).
    ///
    /// # Panics
    /// Panics if static survival key sequences fail to parse.
    #[must_use]
    pub fn survival() -> Self {
        let mut map = Keymap::default();
        let bindings = [
            ("C-x C-f", "find-file"),
            ("C-x C-s", "save-buffer"),
            ("C-x C-c", "save-buffers-kill-emacs"),
            ("C-f", "forward-char"),
            ("C-b", "backward-char"),
            ("C-n", "next-line"),
            ("C-p", "previous-line"),
            ("C-a", "move-beginning-of-line"),
            ("C-e", "move-end-of-line"),
            ("right", "forward-char"),
            ("left", "backward-char"),
            ("down", "next-line"),
            ("up", "previous-line"),
            ("home", "move-beginning-of-line"),
            ("end", "move-end-of-line"),
            ("DEL", "delete-backward-char"),
            ("C-d", "delete-char"),
            ("C-SPC", "set-mark-command"),
            ("C-w", "kill-region"),
            ("M-w", "kill-ring-save"),
            ("C-y", "yank"),
            ("C-/", "undo"),
            ("C-_", "undo"),
            ("C-s", "isearch-forward"),
            ("M-x", "execute-extended-command"),
        ];
        for (keys, name) in bindings {
            map = map.bind_str(keys, name).expect("survival keys parse");
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sequences_and_rejects_garbage() {
        assert_eq!(
            parse_keys("C-x C-f").unwrap(),
            vec![
                Key {
                    code: Code::Char('x'),
                    ctrl: true,
                    meta: false
                },
                Key {
                    code: Code::Char('f'),
                    ctrl: true,
                    meta: false
                }
            ]
        );
        assert!(parse_keys("C-left").is_err());
        assert!(parse_keys("nope").is_err());
        assert!(parse_key_seq("").is_err());
        assert!(parse_key_seq("a b").is_err());
        assert!(parse_key_seq("C-x C-f C-s").is_err());
    }

    #[test]
    fn survival_bindings_resolve() {
        let map = Keymap::survival();
        assert_eq!(
            map.lookup(&parse_keys("C-x C-s").unwrap()),
            Some("save-buffer")
        );
        assert_eq!(map.lookup(&parse_keys("C-_").unwrap()), Some("undo"));
        assert_eq!(map.lookup(&parse_keys("C-/").unwrap()), Some("undo"));
        assert_eq!(
            map.lookup(&parse_keys("C-SPC").unwrap()),
            Some("set-mark-command")
        );
    }

    #[test]
    fn unbind_removes_existing_binding() {
        let map = Keymap::survival();
        let key = parse_key_seq("C-x C-s").unwrap();
        assert_eq!(map.lookup(&key), Some("save-buffer"));

        let unmapped = map.unbind(&key);
        assert_eq!(unmapped.lookup(&key), None);
    }

    #[test]
    fn unbind_nonexistent_is_noop() {
        let map = Keymap::survival();
        let key = parse_key_seq("C-k").unwrap();
        assert_eq!(map.lookup(&key), None);

        let map2 = map.clone().unbind(&key);
        assert_eq!(map2.lookup(&key), None);
    }
}
