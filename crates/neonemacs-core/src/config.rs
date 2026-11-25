//! Typed editor settings (docs/context.md §2.6, §4.4). Lua reads and writes them by name through
//! `get` and `set`, and every value is checked on the way in (I-11).

use crate::eol::Eol;
use crate::width::Metrics;
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use neobit::neobit;

neobit! {
    /// The on/off settings of `Config`, one bit each.
    pub struct Toggles: u8 {
        /// TAB inserts spaces up to the next tab stop instead of a tab character.
        const EXPAND_TABS = 1 << 0;
        const CASE_FOLD_SEARCH = 1 << 1;
        const SYNTAX_HIGHLIGHT = 1 << 2;
        /// A failed repeated C-s or C-r restarts at the other end of the buffer.
        const ISEARCH_WRAP = 1 << 3;
        /// A pending prefix key (`C-x-`, `ESC-`) replaces the echo-area message.
        const ECHO_PREFIX = 1 << 4;
        /// East Asian ambiguous characters take 2 columns (Lua `ambiguous-width` 2) instead of 1.
        const CJK_AMBIGUOUS = 1 << 5;
        /// Permit delta WAL writes where a replayable baseline is available.
        const WAL_DELTAS = 1 << 6;
    }
}

impl Toggles {
    pub const DEFAULT: Self = Self::CASE_FOLD_SEARCH
        .union(Self::SYNTAX_HIGHLIGHT)
        .union(Self::ISEARCH_WRAP)
        .union(Self::ECHO_PREFIX);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub fill_column: u16,
    pub tab_width: u8,
    pub toggles: Toggles,
    pub max_file_size: u64,
    pub undo_limit: usize,
    pub kill_ring_max: usize,
    /// Typed characters that share one undo group.
    pub amalgamating_undo_limit: usize,
    /// Line ending of a new buffer, a missing file, and a file without any line break.
    pub default_eol: Eol,
    /// Program that runs `shell-command`, called as `program switch command`.
    pub shell_file_name: String,
    pub shell_command_switch: String,
}

/// The shell that runs a `shell-command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: String,
    pub switch: String,
}

impl Default for ShellSpec {
    fn default() -> Self {
        let (program, switch) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        Self {
            program: program.to_owned(),
            switch: switch.to_owned(),
        }
    }
}

impl Config {
    #[must_use]
    pub fn new() -> Self {
        let shell = ShellSpec::default();
        Self {
            fill_column: 70,
            tab_width: 8,
            toggles: Toggles::DEFAULT,
            max_file_size: 512 << 20,
            undo_limit: 4 << 20,
            kill_ring_max: 120,
            amalgamating_undo_limit: 20,
            default_eol: Eol::Lf,
            shell_file_name: shell.program,
            shell_command_switch: shell.switch,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// A setting value crossing the Lua boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
}

/// One configurable setting. Every name Lua can pass to `get` and `set` is a variant (I-52).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    FillColumn,
    TabWidth,
    ExpandTabs,
    CaseFoldSearch,
    MaxFileSize,
    UndoLimit,
    KillRingMax,
    AmalgamatingUndoLimit,
    SyntaxHighlight,
    IsearchWrap,
    EchoPrefix,
    DefaultEol,
    ShellFileName,
    ShellCommandSwitch,
    AmbiguousWidth,
    WalDeltas,
}

impl Setting {
    pub const ALL: &'static [Self] = &[
        Self::FillColumn,
        Self::TabWidth,
        Self::ExpandTabs,
        Self::CaseFoldSearch,
        Self::MaxFileSize,
        Self::UndoLimit,
        Self::KillRingMax,
        Self::AmalgamatingUndoLimit,
        Self::SyntaxHighlight,
        Self::IsearchWrap,
        Self::EchoPrefix,
        Self::DefaultEol,
        Self::ShellFileName,
        Self::ShellCommandSwitch,
        Self::AmbiguousWidth,
        Self::WalDeltas,
    ];

    /// Parses a setting name coming from Lua.
    ///
    /// # Errors
    /// Returns an error if `name` is not a recognized configuration setting.
    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|s| s.name() == name)
            .ok_or_else(|| format!("unknown setting: {name}"))
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FillColumn => "fill-column",
            Self::TabWidth => "tab-width",
            Self::ExpandTabs => "expand-tabs",
            Self::CaseFoldSearch => "case-fold-search",
            Self::MaxFileSize => "max-file-size",
            Self::UndoLimit => "undo-limit",
            Self::KillRingMax => "kill-ring-max",
            Self::AmalgamatingUndoLimit => "amalgamating-undo-limit",
            Self::SyntaxHighlight => "syntax-highlight",
            Self::IsearchWrap => "isearch-wrap",
            Self::EchoPrefix => "echo-prefix",
            Self::DefaultEol => "default-eol",
            Self::ShellFileName => "shell-file-name",
            Self::ShellCommandSwitch => "shell-command-switch",
            Self::AmbiguousWidth => "ambiguous-width",
            Self::WalDeltas => "wal-deltas",
        }
    }
}

impl Config {
    /// Tab stops and character widths for laying out text.
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        Metrics {
            tab: usize::from(self.tab_width),
            cjk: self.toggles.contains(Toggles::CJK_AMBIGUOUS),
        }
    }

    #[must_use]
    pub fn shell_spec(&self) -> ShellSpec {
        ShellSpec {
            program: self.shell_file_name.clone(),
            switch: self.shell_command_switch.clone(),
        }
    }

    /// Returns the current value of `setting`.
    #[must_use]
    pub fn get(&self, setting: Setting) -> Value {
        match setting {
            Setting::FillColumn => Value::Int(i64::from(self.fill_column)),
            Setting::TabWidth => Value::Int(i64::from(self.tab_width)),
            Setting::ExpandTabs => Value::Bool(self.toggles.contains(Toggles::EXPAND_TABS)),
            Setting::CaseFoldSearch => {
                Value::Bool(self.toggles.contains(Toggles::CASE_FOLD_SEARCH))
            }
            Setting::MaxFileSize => {
                Value::Int(i64::try_from(self.max_file_size).unwrap_or(i64::MAX))
            }
            Setting::UndoLimit => Value::Int(i64::try_from(self.undo_limit).unwrap_or(i64::MAX)),
            Setting::KillRingMax => {
                Value::Int(i64::try_from(self.kill_ring_max).unwrap_or(i64::MAX))
            }
            Setting::AmalgamatingUndoLimit => {
                Value::Int(i64::try_from(self.amalgamating_undo_limit).unwrap_or(i64::MAX))
            }
            Setting::SyntaxHighlight => {
                Value::Bool(self.toggles.contains(Toggles::SYNTAX_HIGHLIGHT))
            }
            Setting::IsearchWrap => Value::Bool(self.toggles.contains(Toggles::ISEARCH_WRAP)),
            Setting::EchoPrefix => Value::Bool(self.toggles.contains(Toggles::ECHO_PREFIX)),
            Setting::WalDeltas => Value::Bool(self.toggles.contains(Toggles::WAL_DELTAS)),
            Setting::DefaultEol => Value::Str(
                match self.default_eol {
                    Eol::Lf => "lf",
                    Eol::Crlf => "crlf",
                }
                .to_owned(),
            ),
            Setting::ShellFileName => Value::Str(self.shell_file_name.clone()),
            Setting::ShellCommandSwitch => Value::Str(self.shell_command_switch.clone()),
            Setting::AmbiguousWidth => {
                Value::Int(if self.toggles.contains(Toggles::CJK_AMBIGUOUS) {
                    2
                } else {
                    1
                })
            }
        }
    }

    /// Sets `setting` after checking the type and range. Errors leave the setting unchanged.
    ///
    /// # Errors
    /// Returns an error if `value` is out of bounds or has the wrong type.
    pub fn set(&self, setting: Setting, value: Value) -> Result<Self, String> {
        let name = setting.name();
        let mut next = self.clone();
        match (setting, value) {
            (Setting::FillColumn, Value::Int(n)) => {
                next.fill_column = bounded(name, n, 1, i64::from(u16::MAX))?;
            }
            (Setting::TabWidth, Value::Int(n)) => next.tab_width = bounded(name, n, 1, 16)?,
            (Setting::ExpandTabs, Value::Bool(b)) => next.toggles.set(Toggles::EXPAND_TABS, b),
            (Setting::CaseFoldSearch, Value::Bool(b)) => {
                next.toggles.set(Toggles::CASE_FOLD_SEARCH, b);
            }
            (Setting::MaxFileSize, Value::Int(n)) => {
                next.max_file_size = bounded(name, n, 1, i64::MAX)?;
            }
            (Setting::UndoLimit, Value::Int(n)) => next.undo_limit = bounded(name, n, 0, i64::MAX)?,
            (Setting::KillRingMax, Value::Int(n)) => {
                next.kill_ring_max = bounded(name, n, 1, i64::MAX)?;
            }
            (Setting::AmalgamatingUndoLimit, Value::Int(n)) => {
                next.amalgamating_undo_limit = bounded(name, n, 1, i64::MAX)?;
            }
            (Setting::SyntaxHighlight, Value::Bool(b)) => {
                next.toggles.set(Toggles::SYNTAX_HIGHLIGHT, b);
            }
            (Setting::IsearchWrap, Value::Bool(b)) => next.toggles.set(Toggles::ISEARCH_WRAP, b),
            (Setting::EchoPrefix, Value::Bool(b)) => next.toggles.set(Toggles::ECHO_PREFIX, b),
            (Setting::WalDeltas, Value::Bool(b)) => next.toggles.set(Toggles::WAL_DELTAS, b),
            (Setting::AmbiguousWidth, Value::Int(n)) => {
                let wide = bounded::<u8>(name, n, 1, 2)? == 2;
                next.toggles.set(Toggles::CJK_AMBIGUOUS, wide);
            }
            (Setting::ShellFileName, Value::Str(s)) => {
                next.shell_file_name = non_empty(name, s)?;
            }
            (Setting::ShellCommandSwitch, Value::Str(s)) => {
                next.shell_command_switch = non_empty(name, s)?;
            }
            (Setting::DefaultEol, Value::Str(s)) => {
                next.default_eol = match s.as_str() {
                    "lf" => Eol::Lf,
                    "crlf" => Eol::Crlf,
                    _ => return Err(format!("{name} must be \"lf\" or \"crlf\"")),
                };
            }
            (
                Setting::ExpandTabs
                | Setting::CaseFoldSearch
                | Setting::SyntaxHighlight
                | Setting::IsearchWrap
                | Setting::EchoPrefix
                | Setting::WalDeltas,
                _,
            ) => {
                return Err(format!("{name} takes a boolean"));
            }
            (Setting::DefaultEol | Setting::ShellFileName | Setting::ShellCommandSwitch, _) => {
                return Err(format!("{name} takes a string"));
            }
            (_, _) => return Err(format!("{name} takes an integer")),
        }
        Ok(next)
    }
}

fn non_empty(name: &str, s: String) -> Result<String, String> {
    if s.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    Ok(s)
}

fn bounded<T: TryFrom<i64>>(name: &str, n: i64, min: i64, max: i64) -> Result<T, String> {
    if !(min..=max).contains(&n) {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    T::try_from(n).map_err(|_| format!("{name} is out of range"))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn get_and_set_validate_values_and_update_config() {
        let c = Config::default();
        assert_eq!(c.get(Setting::FillColumn), Value::Int(70));
        assert_eq!(c.get(Setting::CaseFoldSearch), Value::Bool(true));
        let c = c.set(Setting::FillColumn, Value::Int(33)).unwrap();
        assert_eq!(c.fill_column, 33);
        assert!(c.set(Setting::TabWidth, Value::Int(0)).is_err());
        assert!(c.set(Setting::FillColumn, Value::Bool(true)).is_err());
        assert_eq!(c.tab_width, 8);

        assert_eq!(c.get(Setting::DefaultEol), Value::Str("lf".to_owned()));
        let c = c
            .set(Setting::DefaultEol, Value::Str("crlf".to_owned()))
            .unwrap();
        assert_eq!(c.default_eol, Eol::Crlf);
        assert!(
            c.set(Setting::DefaultEol, Value::Str("cr".to_owned()))
                .is_err()
        );
        assert_eq!(
            c.set(Setting::DefaultEol, Value::Int(1)),
            Err("default-eol takes a string".to_owned())
        );

        assert_eq!(c.shell_spec(), ShellSpec::default());
        let c = c
            .set(Setting::ShellFileName, Value::Str("pwsh".to_owned()))
            .unwrap()
            .set(
                Setting::ShellCommandSwitch,
                Value::Str("-Command".to_owned()),
            )
            .unwrap();
        assert_eq!(
            c.shell_spec(),
            ShellSpec {
                program: "pwsh".to_owned(),
                switch: "-Command".to_owned()
            }
        );
        assert_eq!(
            c.set(Setting::ShellFileName, Value::Str(String::new())),
            Err("shell-file-name must not be empty".to_owned())
        );
        assert_eq!(
            c.set(Setting::ShellCommandSwitch, Value::Bool(true)),
            Err("shell-command-switch takes a string".to_owned())
        );

        assert_eq!(c.get(Setting::AmbiguousWidth), Value::Int(1));
        assert!(!c.metrics().cjk);
        let c = c.set(Setting::AmbiguousWidth, Value::Int(2)).unwrap();
        assert!(c.metrics().cjk);
        assert_eq!(c.metrics().tab, 8);
        assert_eq!(
            c.set(Setting::AmbiguousWidth, Value::Int(3)),
            Err("ambiguous-width must be between 1 and 2".to_owned())
        );

        assert_eq!(c.get(Setting::ExpandTabs), Value::Bool(false));
        let c = c.set(Setting::ExpandTabs, Value::Bool(true)).unwrap();
        assert!(c.toggles.contains(Toggles::EXPAND_TABS));
        assert_eq!(
            c.set(Setting::ExpandTabs, Value::Int(1)),
            Err("expand-tabs takes a boolean".to_owned())
        );
    }

    #[test]
    fn every_setting_round_trips_and_parses_by_name() {
        let c = Config::default();
        for &s in Setting::ALL {
            assert_eq!(Setting::parse(s.name()), Ok(s));
            assert_eq!(c.set(s, c.get(s)), Ok(c.clone()));
        }
        assert_eq!(
            Setting::parse("nope"),
            Err("unknown setting: nope".to_owned())
        );
    }

    #[test]
    fn all_lists_every_variant_once() {
        // The match has no wildcard, so a new variant fails to compile until it gets an index;
        // the length check then fails until the variant is added to `ALL`.
        const fn index(s: Setting) -> usize {
            match s {
                Setting::FillColumn => 0,
                Setting::TabWidth => 1,
                Setting::ExpandTabs => 2,
                Setting::CaseFoldSearch => 3,
                Setting::MaxFileSize => 4,
                Setting::UndoLimit => 5,
                Setting::KillRingMax => 6,
                Setting::AmalgamatingUndoLimit => 7,
                Setting::SyntaxHighlight => 8,
                Setting::IsearchWrap => 9,
                Setting::EchoPrefix => 10,
                Setting::DefaultEol => 11,
                Setting::ShellFileName => 12,
                Setting::ShellCommandSwitch => 13,
                Setting::AmbiguousWidth => 14,
                Setting::WalDeltas => 15,
            }
        }
        let indexes: Vec<usize> = Setting::ALL.iter().map(|&s| index(s)).collect();
        assert_eq!(indexes, (0..16).collect::<Vec<_>>());
    }
}
