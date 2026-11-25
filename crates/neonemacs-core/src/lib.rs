//! Pure editor transitions. Nothing here touches the file system, a process, Lua, or the terminal (I-14).

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

mod buffer;
mod commands;
mod config;
mod dispatch;
mod editor;
pub mod effect;
mod eol;
mod file;
mod keymap;
pub mod render;
#[cfg(test)]
mod scenarios;
mod search;
mod syntax;
#[cfg(test)]
mod test_support;
mod undo;
pub mod weave_tree;
mod width;
mod window;

pub use buffer::{
    Buffer, BufferId, MAX_WAL_DELTA_BYTES, MAX_WAL_DELTA_RECORDS, WAL_DELTA_RECORD_BYTES,
    WalBaseDigest, wal_base_digest,
};
pub use commands::{confirmed, is_builtin, record_init_error, request_open, switch_to};
pub use config::{Config, Setting, ShellSpec, Toggles, Value};
pub use dispatch::command;
pub use editor::{CANCEL_MESSAGE, Confirm, Editor, LastCommand, Mode, Prefix, Window};
pub use eol::{Eol, EolStreamDecoder};
pub use file::{Backing, DiskState, FilePath, RawFile, Read, loaded_from};
pub use keymap::{AliasTarget, Code, Key, KeyError, ctrl_key, parse_key_seq, parse_keys};
pub use syntax::{SyntaxDef, SyntaxRegistry, TokenKind, highlight_line};
pub use undo::Edit;
pub use weave_tree::{Doc, DocError, Replace, Unit};
pub use width::str_width;

use crate::effect::{Effects, Event};

/// Applies `event` to the editor. Returns the new editor and the effects the shell must perform.
#[must_use]
pub fn step(ed: Editor, event: Event) -> (Editor, Effects) {
    let fx = Vec::new();
    match event {
        Event::Key(key) => dispatch::handle_key(ed, key, fx),
        Event::Run(name) => dispatch::run_named(ed, &name, fx),
        Event::Disk {
            target,
            path,
            state,
        } => commands::disk_checked(ed, target, &path, state, fx),
        Event::Written { id, path, result } => commands::written(ed, id, &path, result, fx),
        Event::FileRead {
            target,
            path,
            result,
        } => commands::file_read(ed, target, path, result, fx),
        Event::WalRemoved => (ed, fx),
        Event::Idle => commands::idle(ed, fx),
        Event::WalFlushed(writes) => (commands::wal_flushed(ed, writes), fx),
        Event::WalProbed { path, line, probe } => commands::wal_probed(ed, &path, line, probe, fx),
        Event::ShellDone(result) => (commands::shell_done(ed, result), fx),
        Event::LuaDone(result) => (commands::lua_done(ed, result), fx),
        Event::InitLoaded(load) => (commands::init_loaded(ed, load), fx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::effect::{Effect, Event};
    use crate::keymap::{Code, Key};

    #[test]
    fn step_returns_editor_and_effects() {
        let ed = Editor::new(Config::default());
        let (ed, fx) = step(ed, Event::Key(Key::plain(Code::Char('a'))));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"a"));
        assert_eq!(fx, [] as [Effect; 0]);
    }
}
