//! Settings group: user-set values and the registries they feed (I-11, I-27).
//! Fields are visible only to `editor`, so no other group can write them (I-32).

use crate::core::commands::CommandRegistry;
use crate::core::config::Config;
use crate::core::keymap::{AnswerAliases, Keymap};
use crate::core::syntax::SyntaxRegistry;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Settings {
    pub(super) config: Config,
    pub(super) keymap: Keymap,
    pub(super) answer_aliases: AnswerAliases,
    pub(super) syntax_registry: SyntaxRegistry,
    pub(super) commands: CommandRegistry,
    /// Directory that relative file paths resolve against (I-37). Empty until the shell sets it.
    pub(super) cwd: PathBuf,
}

impl Settings {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            keymap: Keymap::survival(),
            answer_aliases: AnswerAliases::default(),
            syntax_registry: SyntaxRegistry::new(),
            commands: CommandRegistry::new(),
            cwd: PathBuf::new(),
        }
    }
}
