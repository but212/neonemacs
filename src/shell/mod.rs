//! Performs effects. The only module that touches the file system, processes, and Lua. The terminal
//! input conversion is in `terminal`.
//!
//! The core returns effects, and this module performs them in order. Each one yields one event,
//! which goes back to the core. A Lua command runs inside the event that started it: the `run`
//! function in Lua calls back into the core synchronously, with no `RefCell` borrow held (I-17).

pub mod atomic;
pub mod file;
pub mod lua;
pub mod session;
pub mod startup;
pub mod terminal;
pub mod terminal_guard;
pub mod wal;

use crate::core::editor::Editor;
use crate::core::effect::{Effect, Effects, Event};
use crate::core::step;
use mlua::Lua;
use std::cell::RefCell;
use std::process::Command;
use std::sync::mpsc;
use std::thread;

/// A running background shell command.
#[derive(Debug)]
pub struct ShellTask {
    pub receiver: mpsc::Receiver<Result<String, String>>,
}

/// The result of an interactive shell command dispatch.
#[derive(Debug)]
pub enum ShellOutcome {
    None,
    Spawned(ShellTask),
    BusyRejected,
}

/// Runs `command` in a background thread without blocking the editor UI (I-24).
#[must_use]
pub fn spawn_shell(command: String) -> ShellTask {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = run_shell(&command);
        let _ = tx.send(res);
    });
    ShellTask { receiver: rx }
}

/// Applies `event` in an interactive session. Spawns `RunShell` in the background (I-24)
/// while performing other effects immediately. If `shell_busy` is true, rejects new shell commands.
#[must_use]
pub fn handle_interactive(
    ed: Editor,
    lua: Option<&Lua>,
    event: Event,
    shell_busy: bool,
) -> (Editor, ShellOutcome) {
    let (ed, effects) = step(ed, event);
    let mut outcome = ShellOutcome::None;
    let mut sync_effects = Vec::new();
    for effect in effects {
        if let Effect::RunShell { command } = effect {
            if shell_busy {
                outcome = ShellOutcome::BusyRejected;
            } else {
                outcome = ShellOutcome::Spawned(spawn_shell(command));
            }
        } else {
            sync_effects.push(effect);
        }
    }
    let ed = perform_all(ed, lua, sync_effects);
    let ed = match &outcome {
        ShellOutcome::Spawned(_) => ed.set_message("[Running...]"),
        ShellOutcome::BusyRejected => {
            ed.set_message("[Running...] A shell command is already running")
        }
        ShellOutcome::None => ed,
    };
    (ed, outcome)
}

/// Applies `event` to the editor and performs the effects it causes. Returns the new editor.
#[must_use]
pub fn handle(ed: Editor, lua: Option<&Lua>, event: Event) -> Editor {
    let (ed, effects) = step(ed, event);
    perform_all(ed, lua, effects)
}

/// Performs `effects` in order, until none are left. Returns the new editor.
#[must_use]
pub fn perform_all(ed: Editor, lua: Option<&Lua>, effects: Effects) -> Editor {
    let cell = RefCell::new(Some(ed));
    let bound = if let Some(lua) = lua {
        // The API stays bound for all of `effects`, so every Lua call in them sees the same editor.
        lua::with_api(lua, &cell, |lua| {
            perform_in(&cell, Some(lua), effects);
            Ok(())
        })
    } else {
        perform_in(&cell, None, effects);
        Ok(())
    };
    let ed = into_editor(cell);
    match bound {
        Ok(()) => ed,
        Err(e) => ed.set_message(e.to_string()),
    }
}

/// Applies `event` to the editor in `cell`, then puts the new editor back. The cell is empty
/// only during this call, since `step` never calls Lua (I-21). Returns the effects to perform.
///
/// # Panics
/// Panics if `cell` does not contain an editor (violating I-21).
pub fn step_in(cell: &RefCell<Option<Editor>>, event: Event) -> Effects {
    let ed = cell
        .borrow_mut()
        .take()
        .expect("editor is in the cell (I-21)");
    let (ed, effects) = step(ed, event);
    *cell.borrow_mut() = Some(ed);
    effects
}

/// Performs `effects` against the editor in `cell`. Each event goes back to the core.
/// No `RefCell` borrow is held while an effect runs (I-17).
pub fn perform_in(cell: &RefCell<Option<Editor>>, lua: Option<&Lua>, effects: Effects) {
    for effect in effects {
        let event = perform(effect, lua);
        let next = step_in(cell, event);
        perform_in(cell, lua, next);
    }
}

/// Runs Lua source with the API bound to `ed`. Commands that `run` performs take effect as they do from a key.
pub fn run_source(ed: Editor, lua: &Lua, source: &str) -> (Editor, mlua::Result<()>) {
    let cell = RefCell::new(Some(ed));
    let result = lua::with_api(lua, &cell, |lua| lua.load(source).exec());
    (into_editor(cell), result)
}

const fn into_editor(cell: RefCell<Option<Editor>>) -> Editor {
    cell.into_inner().expect("editor is in the cell (I-21)")
}

/// Performs one effect and returns the single event that answers it (I-15).
#[must_use]
pub fn perform(effect: Effect, lua: Option<&Lua>) -> Event {
    match effect {
        Effect::StatDisk { id, path } => Event::Disk {
            id,
            state: file::disk_state(&path),
        },
        Effect::WriteFile { id, path, bytes } => {
            let result = file::save(&path, &bytes).map_err(|e| e.to_string());
            if result.is_ok() {
                wal::remove_wal(&path);
            }
            Event::Written { id, path, result }
        }
        Effect::ReadFile {
            target,
            path,
            max_size,
        } => {
            let result = file::read_raw(&path, max_size).map_err(|e| e.to_string());
            Event::FileRead {
                target,
                path,
                result,
            }
        }
        Effect::RemoveWal { path } => {
            wal::remove_wal(&path);
            Event::WalRemoved
        }
        Effect::RunShell { command } => Event::ShellDone(run_shell(&command)),
        Effect::RunLua { name } => Event::LuaDone(run_lua(lua, &name)),
    }
}

/// Runs `command` with the platform shell and returns its stdout followed by its stderr.
fn run_shell(command: &str) -> Result<String, String> {
    let output = if cfg!(windows) {
        Command::new("cmd").args(["/C", command]).output()
    } else {
        Command::new("sh").args(["-c", command]).output()
    };
    let output = output.map_err(|e| format!("Cannot run command: {e}"))?;
    let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(s)
}

fn run_lua(lua: Option<&Lua>, name: &str) -> Result<(), String> {
    match lua {
        Some(lua) => lua::call_lua(lua, name).map_err(|e| e.to_string()),
        None => Err(format!("No command named {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::BufferId;
    use crate::core::effect::{Read, ReadTarget};
    use crate::core::file::DiskState;

    #[test]
    fn effect_round_trip_yields_one_event_each() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let id = BufferId(1);

        let stat = perform(
            Effect::StatDisk {
                id,
                path: path.clone(),
            },
            None,
        );
        assert_eq!(
            stat,
            Event::Disk {
                id,
                state: DiskState::Unknown
            }
        );
        let write = perform(
            Effect::WriteFile {
                id,
                path: path.clone(),
                bytes: b"hi".to_vec(),
            },
            None,
        );
        assert!(matches!(
            write,
            Event::Written {
                result: Ok(DiskState::Known { size: 2, .. }),
                ..
            }
        ));
        let read = perform(
            Effect::ReadFile {
                target: ReadTarget::Revert(id),
                path,
                max_size: 10,
            },
            None,
        );
        assert!(matches!(
            read,
            Event::FileRead {
                result: Ok(Read::Found(_)),
                ..
            }
        ));
        let shell = perform(
            Effect::RunShell {
                command: "echo hi".to_owned(),
            },
            None,
        );
        assert!(matches!(shell, Event::ShellDone(Ok(s)) if s.starts_with("hi")));
        let lua_missing = perform(
            Effect::RunLua {
                name: "nope".to_owned(),
            },
            None,
        );
        assert_eq!(
            lua_missing,
            Event::LuaDone(Err("No command named nope".to_owned()))
        );
    }
}
