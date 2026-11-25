//! Settings group: user-set values and the registries they feed (I-11, I-27).
//! Fields are visible only to `editor`, so no other group can write them (I-32).

use crate::config::Config;
use crate::keymap::{KeyAliases, Keymap};
use crate::syntax::SyntaxRegistry;
use alloc::collections::BTreeSet;
use alloc::string::String;

#[derive(Debug, Clone)]
pub struct Settings {
    pub(super) config: Config,
    pub(super) keymap: Keymap,
    pub(super) key_aliases: KeyAliases,
    pub(super) syntax_registry: SyntaxRegistry,
    pub(super) commands: BTreeSet<String>,
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
            commands: BTreeSet::new(),
            cwd: String::new(),
        }
    }
}
