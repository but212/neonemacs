//! Settings group: user-set values and the registries they feed (I-11, I-27).
//! Fields are visible only to `editor`, so no other group can write them (I-32).

use crate::config::Config;
use crate::keymap::{KeyAliases, Keymap};
use crate::syntax::SyntaxRegistry;
use alloc::collections::BTreeSet;
use alloc::string::String;

/// Set of registered extension commands available for dispatch and M-x completion (§4, §12).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommandRegistry {
    commands: BTreeSet<String>,
}

impl CommandRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            commands: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn insert(mut self, name: impl Into<String>) -> Self {
        self.commands.insert(name.into());
        self
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains(name)
    }

    pub fn completions<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> {
        self.commands
            .iter()
            .map(String::as_str)
            .filter(move |name| name.starts_with(prefix))
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub(super) config: Config,
    pub(super) keymap: Keymap,
    pub(super) key_aliases: KeyAliases,
    pub(super) syntax_registry: SyntaxRegistry,
    pub(super) commands: CommandRegistry,
    /// Directory that relative file paths resolve against (I-37). Empty until the shell sets it.
    pub(super) cwd: String,
}

impl Settings {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            keymap: Keymap::survival(),
            key_aliases: KeyAliases::default(),
            syntax_registry: SyntaxRegistry::new(),
            commands: CommandRegistry::new(),
            cwd: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn registry_insert_contains_and_completions() {
        let reg = CommandRegistry::new();
        assert!(!reg.contains("hello"));

        let reg = reg.insert("hello");
        assert!(reg.contains("hello"));

        let reg = reg.insert("help").insert("world");

        let cands: Vec<&str> = reg.completions("hel").collect();
        assert_eq!(cands, vec!["hello", "help"]);
    }
}
