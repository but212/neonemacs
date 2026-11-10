//! Pure editor transitions. Nothing here touches the file system, a process, Lua, or the terminal (I-14).

pub mod buffer;
pub mod commands;
pub mod config;
pub mod dispatch;
pub mod editor;
pub mod effect;
pub mod eol;
pub mod file;
pub mod keymap;
pub mod kill_ring;
pub mod mode;
pub mod render;
pub mod search;
pub mod syntax;
#[cfg(test)]
mod test_support;
pub mod text;
pub mod undo;
pub mod width;
pub mod window;

use crate::editor::Editor;
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
        assert_eq!(ed.buffer().text().to_bytes(), b"a");
        assert_eq!(fx, [] as [Effect; 0]);
    }
}
