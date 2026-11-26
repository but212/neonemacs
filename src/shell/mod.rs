//! Performs effects. The only module that touches the file system, processes, and Lua. The terminal
//! input conversion is in `terminal`.
//!
//! The core returns effects, and this module performs them in order. Each one yields one event,
//! which goes back to the core. A Lua command runs inside the event that started it: the `run`
//! function in Lua calls back into the core synchronously, with no `RefCell` borrow held (I-21).

pub mod file;
pub mod lua;
pub mod recovery;
pub mod registore;
pub mod session;
pub mod startup;
pub mod terminal;

use crate::core::Editor;
use crate::core::ShellSpec;
use crate::core::effect::{Effect, Effects, Event};
use crate::core::step;
use mlua::Lua;
use std::cell::RefCell;
use std::env;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

pub(crate) fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(crate) fn xdg_path(
    xdg: Option<&OsStr>,
    home: Option<&Path>,
    fallback: &[&str],
    file: &str,
) -> Option<PathBuf> {
    let base = xdg.filter(|s| !s.is_empty()).map_or_else(
        || {
            home.map(|h| {
                let mut p = h.to_path_buf();
                p.extend(fallback);
                p
            })
        },
        |xdg| Some(PathBuf::from(xdg)),
    )?;
    Some(base.join("neonemacs").join(file))
}

/// The echo-area message while a background shell command runs, before the spinner's first frame.
pub const RUNNING: &str = "[Running...]";

/// What a refused second command adds to the running message.
pub const BUSY_NOTICE: &str = "A shell command is already running";

/// Full message shown when a second shell command is refused while one is already running.
pub const BUSY_MESSAGE: &str = "[Running...] A shell command is already running";

/// The result of an interactive shell command dispatch.
#[derive(Debug)]
pub enum ShellOutcome {
    None,
    Spawned(mpsc::Receiver<Result<String, String>>),
    BusyRejected,
    /// The caller must reload `init.lua`: only it can replace the Lua state.
    ReloadInit,
    /// The caller must suspend the terminal session (`SIGTSTP` on Unix).
    Suspend,
}

/// Runs `command` in a background thread without blocking the editor UI (I-24).
#[must_use]
pub fn spawn_shell(command: String, shell: ShellSpec) -> mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = run_shell_capped(&command, &shell, MAX_SHELL_OUTPUT);
        let _ = tx.send(res);
    });
    rx
}

/// Applies `event` in an interactive session.
///
/// Spawns `RunShell` in the background (I-24) while performing other effects immediately. If
/// `shell_busy` is true, rejects new shell commands.
/// `ReloadInit` is handed back to the caller, since the Lua state is borrowed here.
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
        if matches!(
            effect,
            Effect::RunShell { .. } | Effect::ReloadInit | Effect::Suspend
        ) {
            debug_assert!(
                matches!(outcome, ShellOutcome::None),
                "one step yields at most one RunShell, ReloadInit, or Suspend (I-15)"
            );
        }
        match effect {
            Effect::RunShell { .. } if shell_busy => outcome = ShellOutcome::BusyRejected,
            Effect::RunShell { command, shell } => {
                outcome = ShellOutcome::Spawned(spawn_shell(command, shell));
            }
            Effect::ReloadInit => outcome = ShellOutcome::ReloadInit,
            Effect::Suspend => outcome = ShellOutcome::Suspend,
            effect => sync_effects.push(effect),
        }
    }
    let ed = perform_all(ed, lua, sync_effects);
    let ed = match &outcome {
        ShellOutcome::Spawned(_) => ed.set_message(RUNNING),
        ShellOutcome::BusyRejected => ed.set_message(BUSY_MESSAGE),
        ShellOutcome::None | ShellOutcome::ReloadInit | ShellOutcome::Suspend => ed,
    };
    (ed, outcome)
}

/// Applies `event` to the editor and performs the effects it causes. Returns the new editor.
#[must_use]
pub fn handle(ed: Editor, lua: Option<&Lua>, event: Event) -> Editor {
    let (ed, effects) = step(ed, event);
    perform_all(ed, lua, effects)
}

/// Explicit state machine for the shell's borrowed `Editor` slot (I-21).
pub enum EditorSlot {
    Ready(Box<Editor>),
    InUse,
    Poisoned,
}

impl EditorSlot {
    #[must_use]
    pub(crate) fn new(ed: Editor) -> Self {
        Self::Ready(Box::new(ed))
    }

    pub(crate) fn take(&mut self) -> Editor {
        match std::mem::replace(self, Self::InUse) {
            Self::Ready(ed) => *ed,
            Self::InUse => panic!("editor is not in the cell (re-entrant borrow, I-21)"),
            Self::Poisoned => panic!("editor slot poisoned by prior panic"),
        }
    }

    pub(crate) fn as_ref(&self) -> &Editor {
        match self {
            Self::Ready(ed) => ed,
            Self::InUse => panic!("editor is not in the cell (I-21)"),
            Self::Poisoned => panic!("editor slot poisoned by prior panic"),
        }
    }

    pub(crate) fn into_editor(self) -> Editor {
        match self {
            Self::Ready(ed) => *ed,
            Self::InUse => panic!("editor is not in the cell (I-21)"),
            Self::Poisoned => panic!("editor slot poisoned by prior panic"),
        }
    }
}

/// Performs `effects` in order, until none are left. Returns the new editor.
#[must_use]
pub fn perform_all(ed: Editor, lua: Option<&Lua>, effects: Effects) -> Editor {
    perform_all_with_recovery_dir(ed, lua, effects, None)
}

/// Performs `effects` in order with an explicit recovery directory override (for tests).
#[must_use]
pub(crate) fn perform_all_with_recovery_dir(
    ed: Editor,
    lua: Option<&Lua>,
    effects: Effects,
    rec_dir: Option<&Path>,
) -> Editor {
    let cell = RefCell::new(EditorSlot::new(ed));
    let bound = if let Some(lua) = lua {
        // The API stays bound for all of `effects`, so every Lua call in them sees the same editor.
        lua::with_api(lua, &cell, |lua| {
            perform_in_with_recovery_dir(&cell, Some(lua), effects, rec_dir);
            Ok(())
        })
    } else {
        perform_in_with_recovery_dir(&cell, None, effects, rec_dir);
        Ok(())
    };
    let ed = cell.into_inner().into_editor();
    match bound {
        Ok(()) => ed,
        Err(e) => ed.set_message(e.to_string()),
    }
}

/// Runs `f` against the editor in `cell`, putting the returned editor back. The cell is `InUse`
/// only during this call (I-21), and transitions to `Poisoned` if `f` unwinds.
///
/// # Panics
/// Panics if `cell` is `InUse` (violating I-21) or `Poisoned` from a prior panic.
pub(crate) fn with_editor_cell<R>(
    cell: &RefCell<EditorSlot>,
    f: impl FnOnce(Editor) -> (Editor, R),
) -> R {
    struct PoisonGuard<'a> {
        cell: &'a RefCell<EditorSlot>,
        restored: bool,
    }

    impl Drop for PoisonGuard<'_> {
        fn drop(&mut self) {
            if !self.restored {
                *self.cell.borrow_mut() = EditorSlot::Poisoned;
            }
        }
    }

    let ed = cell.borrow_mut().take();
    let mut guard = PoisonGuard {
        cell,
        restored: false,
    };
    let (ed, out) = f(ed);
    *cell.borrow_mut() = EditorSlot::Ready(Box::new(ed));
    guard.restored = true;
    out
}

/// Performs `effects` against the editor in `cell`. Each event goes back to the core.
/// No `RefCell` borrow is held while an effect runs (I-21).
pub fn perform_in(cell: &RefCell<EditorSlot>, lua: Option<&Lua>, effects: Effects) {
    perform_in_with_recovery_dir(cell, lua, effects, None);
}

fn perform_in_with_recovery_dir(
    cell: &RefCell<EditorSlot>,
    lua: Option<&Lua>,
    effects: Effects,
    rec_dir: Option<&Path>,
) {
    for effect in effects {
        if let Some(event) = perform_with_recovery_dir(cell, effect, lua, rec_dir) {
            let next = with_editor_cell(cell, |ed| step(ed, event));
            perform_in_with_recovery_dir(cell, lua, next, rec_dir);
        }
    }
}

/// Runs Lua source with the API bound to `ed`. Commands that `run` performs take effect as they do from a key.
/// `name` is the chunk name that error messages show: `=init.lua` reads as `init.lua:<line>:` (I-55).
pub fn run_source(ed: Editor, lua: &Lua, name: &str, source: &str) -> (Editor, mlua::Result<()>) {
    let cell = RefCell::new(EditorSlot::new(ed));
    let result = lua::with_api(lua, &cell, |lua| lua.load(source).set_name(name).exec());
    (cell.into_inner().into_editor(), result)
}

/// Performs one effect and returns the event that answers it, if any.
///
/// # Panics
/// Panics if `cell` does not contain a `Ready` editor (violating I-21 or poisoned).
#[must_use]
pub fn perform(cell: &RefCell<EditorSlot>, effect: Effect, lua: Option<&Lua>) -> Option<Event> {
    perform_with_recovery_dir(cell, effect, lua, None)
}

fn perform_with_recovery_dir(
    cell: &RefCell<EditorSlot>,
    effect: Effect,
    lua: Option<&Lua>,
    rec_dir: Option<&Path>,
) -> Option<Event> {
    let event = match effect {
        Effect::StatDisk { target, path } => Event::Disk {
            target,
            state: file::disk_state(Path::new(&path)),
            path,
        },
        Effect::WriteFile { id, path } => {
            let result = {
                let guard = cell.borrow();
                let ed = guard.as_ref();
                let buf = ed.buffer_by_id(id);
                file::save_doc(Path::new(&path), buf.eol(), buf.text()).map_err(|e| e.to_string())
            };
            Event::Written { id, path, result }
        }
        Effect::ReadFile {
            target,
            path,
            max_size,
        } => {
            let result = file::read_raw(Path::new(&path), max_size).map_err(|e| e.to_string());
            Event::FileRead {
                target,
                path,
                result,
            }
        }
        Effect::ProbeRecovery { id, path } => {
            let rec = rec_dir
                .map(Path::to_path_buf)
                .or_else(recovery::recovery_dir)
                .and_then(|dir| recovery::find_latest_recovery(&dir, &path, std::process::id()))
                .map(
                    |(rec_paths, _meta, doc, eol)| crate::core::effect::RecoveredFile {
                        doc,
                        eol,
                        rec_paths: rec_paths
                            .into_iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect(),
                    },
                );
            Event::RecoveryProbed { id, rec }
        }
        Effect::DeleteRecovery { rec_paths } => {
            for rec_path in rec_paths {
                let _ = std::fs::remove_file(rec_path);
            }
            return None;
        }
        Effect::RunShell { command, shell } => {
            Event::ShellDone(run_shell_capped(&command, &shell, MAX_SHELL_OUTPUT))
        }
        Effect::RunLua { name } => Event::LuaDone(lua.map_or_else(
            || Err(format!("No command named {name}")),
            |lua| lua::call_lua(lua, &name).map_err(|e| e.to_string()),
        )),
        // Reached from a Lua command, which runs inside the state that a reload would replace.
        Effect::ReloadInit => Event::InitLoaded(crate::core::effect::InitLoad::Ignored),
        Effect::Suspend => {
            with_editor_cell(cell, |ed| {
                (ed.set_message("suspend-emacs: not available from Lua"), ())
            });
            return None;
        }
    };
    Some(event)
}

/// The most bytes of stdout or stderr that a shell command may produce before it is killed.
const MAX_SHELL_OUTPUT: usize = 4 << 20;

/// Reads at most `cap + 1` bytes of `stream` on a thread and sends them with `index`.
fn spawn_reader(
    index: usize,
    stream: impl Read + Send + 'static,
    cap: usize,
    tx: mpsc::Sender<(usize, Vec<u8>)>,
) {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.take(cap as u64 + 1).read_to_end(&mut bytes);
        let _ = tx.send((index, bytes));
    });
}

fn run_shell_capped(command: &str, shell: &ShellSpec, cap: usize) -> Result<String, String> {
    let mut child = Command::new(&shell.program)
        .args([shell.switch.as_str(), command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run command: {e}"))?;
    let (tx, rx) = mpsc::channel();
    // Both streams are `Some`: the command was spawned with `Stdio::piped()` for each.
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    spawn_reader(0, stdout, cap, tx.clone());
    spawn_reader(1, stderr, cap, tx);
    let mut parts = [Vec::new(), Vec::new()];
    let mut too_long = false;
    for (index, bytes) in rx {
        if bytes.len() > cap {
            // Do not wait for the other stream: a surviving descendant of the shell
            // may hold its pipe open after the shell itself is killed.
            too_long = true;
            let _ = child.kill();
            break;
        }
        parts[index] = bytes;
    }
    let _ = child.wait();
    if too_long {
        return Err(format!(
            "Command output exceeds {cap} bytes; command killed"
        ));
    }
    let mut s = String::from_utf8_lossy(&parts[0]).into_owned();
    s.push_str(&String::from_utf8_lossy(&parts[1]));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DiskState;
    use crate::core::effect::{ReadTarget, StatTarget};

    #[test]
    fn effect_round_trip_yields_one_event_each() {
        use crate::core::{Config, Doc};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt").to_string_lossy().into_owned();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"hi")));
        let id = ed.buffer().id();
        let cell = RefCell::new(EditorSlot::new(ed));

        let stat = perform(
            &cell,
            Effect::StatDisk {
                target: StatTarget::Save(id),
                path: path.clone(),
            },
            None,
        );
        assert_eq!(
            stat,
            Some(Event::Disk {
                target: StatTarget::Save(id),
                path: path.clone(),
                state: DiskState::Unknown
            })
        );
        let write = perform(
            &cell,
            Effect::WriteFile {
                id,
                path: path.clone(),
            },
            None,
        );
        assert!(matches!(
            write,
            Some(Event::Written {
                result: Ok(DiskState::Known { size: 2, .. }),
                ..
            })
        ));
        let read = perform(
            &cell,
            Effect::ReadFile {
                target: ReadTarget::Revert(id),
                path,
                max_size: 10,
            },
            None,
        );
        assert!(matches!(
            read,
            Some(Event::FileRead {
                result: Ok(Some(_)),
                ..
            })
        ));
        let shell = perform(
            &cell,
            Effect::RunShell {
                command: "echo hi".to_owned(),
                shell: ShellSpec::default(),
            },
            None,
        );
        assert!(matches!(shell, Some(Event::ShellDone(Ok(s))) if s.starts_with("hi")));
        let lua_missing = perform(
            &cell,
            Effect::RunLua {
                name: "nope".to_owned(),
            },
            None,
        );
        assert_eq!(
            lua_missing,
            Some(Event::LuaDone(Err("No command named nope".to_owned())))
        );
    }

    #[test]
    fn endless_shell_output_is_cut_off() {
        let endless = if cfg!(windows) {
            "for /L %i in (1,0,2) do @echo xxxxxxxx"
        } else {
            "yes"
        };
        let result = run_shell_capped(endless, &ShellSpec::default(), 1024);
        assert!(matches!(result, Err(e) if e.contains("exceeds 1024 bytes")));
        let ok = run_shell_capped("echo hi", &ShellSpec::default(), 1024);
        assert!(matches!(ok, Ok(s) if s.starts_with("hi")));
    }

    #[test]
    fn reload_init_is_handed_to_the_session_and_refused_elsewhere() {
        use crate::core::Config;
        let ed = Editor::new(Config::default());
        let (ed, outcome) =
            handle_interactive(ed, None, Event::Run("reload-init".to_owned()), false);
        assert!(matches!(outcome, ShellOutcome::ReloadInit));
        assert_eq!(ed.message(), "");

        let ed = handle(ed, None, Event::Run("reload-init".to_owned()));
        assert_eq!(ed.message(), "reload-init: not available from Lua");
    }

    #[test]
    fn suspend_emacs_is_handed_to_the_session() {
        use crate::core::Config;
        let ed = Editor::new(Config::default());
        let (ed, outcome) =
            handle_interactive(ed, None, Event::Run("suspend-emacs".to_owned()), false);
        assert!(matches!(outcome, ShellOutcome::Suspend));
        assert_eq!(ed.message(), "");

        let ed = handle(ed, None, Event::Run("suspend-emacs".to_owned()));
        assert_eq!(ed.message(), "suspend-emacs: not available from Lua");
    }

    #[test]
    fn with_editor_cell_poisons_slot_on_panic() {
        use crate::core::Config;
        let cell = RefCell::new(EditorSlot::new(Editor::new(Config::default())));
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_editor_cell(&cell, |_ed| -> (Editor, ()) { panic!("boom") });
        }));
        assert!(res.is_err());
        assert!(matches!(&*cell.borrow(), EditorSlot::Poisoned));

        let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_editor_cell(&cell, |ed| (ed, ()));
        }));
        let msg = second
            .unwrap_err()
            .downcast_ref::<&str>()
            .copied()
            .unwrap_or("");
        assert!(msg.contains("poisoned"), "unexpected panic message: {msg}");
    }
}
