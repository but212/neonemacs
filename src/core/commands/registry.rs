//! Dynamic command registry (§4, §12).
//! Tracks registered extension command names in pure functional core (I-18).

use std::collections::BTreeSet;

/// Set of registered extension commands available for dispatch and M-x completion.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommandRegistry {
    commands: BTreeSet<String>,
}

impl CommandRegistry {
    pub const fn new() -> Self {
        Self {
            commands: BTreeSet::new(),
        }
    }

    pub fn insert(mut self, name: impl Into<String>) -> Self {
        self.commands.insert(name.into());
        self
    }

    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.commands.iter().map(String::as_str)
    }

    pub fn completions<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> {
        self.commands
            .iter()
            .map(String::as_str)
            .filter(move |name| name.starts_with(prefix))
    }
}

#[cfg(test)]
mod tests {
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
