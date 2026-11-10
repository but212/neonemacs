//! Shell commands and their output (§4, A9): asks the shell to run a command, then shows the
//! result in the echo area or in `*Shell Command Output*`.
//!
//! Also the outcome of loading `init.lua`, whose errors go to `*init-log*` (§12).

use crate::commands::buffer_window::{split, switch_to};
use crate::commands::prompt::prompt;
use crate::editor::{Buffer, BufferId, Dir, Editor, Purpose};
use crate::effect::{Effect, Effects, InitLoad};
use crate::file::Backing;
use crate::text::Text;

pub(crate) fn shell_command_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Shell command: ", Purpose::ShellCommand), fx)
}

/// Asks the shell to run `command`. The output comes back as `ShellDone`.
pub(crate) fn run_shell(ed: &Editor, command: &str, mut fx: Effects) -> Effects {
    if command.is_empty() {
        return fx;
    }
    fx.push(Effect::RunShell {
        command: command.to_owned(),
        shell: ed.config().shell_spec(),
    });
    fx
}

/// Shows the output of a shell command. One line goes to the echo area; more goes to `*Shell Command Output*` (A9).
#[must_use]
pub fn shell_done(ed: Editor, result: Result<String, String>) -> Editor {
    let output = match result {
        Ok(output) => output,
        Err(e) => return ed.set_message(e),
    };
    let text = output.replace("\r\n", "\n");
    let trimmed = text.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.lines().collect();
    match lines.as_slice() {
        [] => ed.set_message("(no output)"),
        [only] => ed.set_message(*only),
        _ => show_output(ed, trimmed),
    }
}

/// Makes `text` the whole content of buffer `id` and leaves the buffer clean.
fn fill_buffer(ed: Editor, id: BufferId, text: &str) -> Editor {
    ed.replace_text(id, Text::from_bytes(text.as_bytes()))
        .with_buffer(id, Buffer::mark_clean)
}

/// The buffer named `name` that the editor fills itself (`BufferPool::find_unbacked`).
fn output_buffer(ed: &Editor, name: &str) -> Option<BufferId> {
    ed.buffers().find_unbacked(name).map(Buffer::id)
}

/// Makes `text` the whole content of the buffer named `name`, which is created if it is missing.
fn fill_named_buffer(ed: Editor, name: &str, text: &str) -> (Editor, BufferId) {
    let (ed, id) = match output_buffer(&ed, name) {
        Some(id) => (ed, id),
        None => ed.add_buffer(name, Text::default(), Backing::None),
    };
    (fill_buffer(ed, id, text), id)
}

/// Puts `text` in the output buffer and shows it in the other window.
fn show_output(ed: Editor, text: &str) -> Editor {
    let (mut ed, id) = fill_named_buffer(ed, "*Shell Command Output*", text);
    if ed.leaf_count() == 1 {
        ed = split(ed, Dir::Below);
    }
    let orig = ed.windows().selected();
    let other = if orig + 1 < ed.leaf_count() {
        orig + 1
    } else {
        orig - 1
    };
    let ed = switch_to(ed.with_windows(|wm| wm.set_selected(other)), id);
    ed.with_windows(|wm| wm.set_selected(orig))
}

/// Ends a Lua command: shows the first line of its error, then closes its undo group like any command.
#[must_use]
pub fn lua_done(ed: Editor, result: Result<(), String>) -> Editor {
    let ed = match result {
        Ok(()) => ed,
        Err(e) => ed.set_message(e.lines().next().unwrap_or_default()),
    };
    ed.end_command()
}

/// Holds the whole text of the last `init.lua` error. Open it with `switch-to-buffer`.
const INIT_LOG: &str = "*init-log*";

pub(crate) fn reload_init_cmd(ed: Editor, mut fx: Effects) -> (Editor, Effects) {
    fx.push(Effect::ReloadInit);
    (ed, fx)
}

/// Keeps the whole `text` of a rejected `init.lua` in `*init-log*`, without showing the buffer,
/// and adds its first line to the echo area (I-55, I-56).
#[must_use]
pub fn record_init_error(ed: Editor, text: &str) -> Editor {
    let (ed, _) = fill_named_buffer(ed, INIT_LOG, text);
    ed.append_message(format!(
        "init.lua: {}",
        text.lines().next().unwrap_or_default()
    ))
}

/// Empties `*init-log*` if it exists: the error it holds is no longer the current one.
fn clear_init_log(ed: Editor) -> Editor {
    match output_buffer(&ed, INIT_LOG) {
        Some(id) => fill_buffer(ed, id, ""),
        None => ed,
    }
}

/// Reports how `reload-init` ended. `None` means that the shell could not reload: the command
/// was run from Lua.
#[must_use]
pub fn init_loaded(ed: Editor, load: Option<InitLoad>) -> Editor {
    match load {
        Some(InitLoad::Applied) => clear_init_log(ed).set_message("Loaded init.lua"),
        Some(InitLoad::Absent) => clear_init_log(ed).set_message("No init.lua"),
        Some(InitLoad::Rejected(text)) => record_init_error(ed.clear_message(), &text),
        None => ed.set_message("reload-init: not available from Lua"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{editor, typ};
    use crate::window::Window;

    #[test]
    fn the_effect_carries_the_configured_shell() {
        use crate::config::{Config, ShellSpec};
        let ed = crate::editor::Editor::new(Config {
            shell_file_name: "pwsh".to_owned(),
            shell_command_switch: "-Command".to_owned(),
            ..Config::default()
        });
        let fx = run_shell(&ed, "echo hi", Vec::new());
        assert_eq!(
            fx,
            [Effect::RunShell {
                command: "echo hi".to_owned(),
                shell: ShellSpec {
                    program: "pwsh".to_owned(),
                    switch: "-Command".to_owned()
                },
            }]
        );
    }

    #[test]
    fn shell_command_one_line_echoes_many_lines_open_buffer() {
        let ed = shell_done(
            editor(),
            Ok("hi
"
            .to_owned()),
        );
        assert_eq!(ed.message(), "hi");

        let ed = shell_done(
            ed,
            Ok("a
b
"
            .to_owned()),
        );
        let out = ed
            .buffers()
            .iter()
            .find(|b| b.name() == "*Shell Command Output*")
            .expect("output buffer");
        assert_eq!(out.text().to_bytes(), b"a\nb");
        assert_eq!(ed.leaf_count(), 2);
    }

    fn output_id(ed: &Editor) -> crate::editor::BufferId {
        ed.buffers()
            .iter()
            .find(|b| b.name() == "*Shell Command Output*")
            .map(Buffer::id)
            .expect("output buffer")
    }

    #[test]
    fn shell_output_replacement_resets_undo() {
        let ed = shell_done(
            editor(),
            Ok("a
b
"
            .to_owned()),
        );
        let out = output_id(&ed);
        let ed = switch_to(ed, out);
        let ed = typ(ed, "X");

        let ed = shell_done(
            ed,
            Ok("c
d
"
            .to_owned()),
        );
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(ed.message(), "No further undo information");
        assert_eq!(
            ed.buffers().get_by_id(output_id(&ed)).text().to_bytes(),
            b"c\nd"
        );
    }

    #[test]
    fn shell_output_replacement_snaps_windows() {
        let ed = shell_done(
            editor(),
            Ok("aaaaaaaaaaaa
b
"
            .to_owned()),
        );
        let out = output_id(&ed);
        let ed = switch_to(ed, out);
        let ed = ed.with_window(|w| Window {
            point: 12,
            top: 1,
            ..w
        });

        let ed = shell_done(
            ed,
            Ok("c
d
"
            .to_owned()),
        );
        let len = ed.buffers().get_by_id(output_id(&ed)).text().len();
        assert!(ed.window().point <= len);
        assert_eq!(ed.window().top, 0);
    }

    fn init_log(ed: &Editor) -> Option<Vec<u8>> {
        ed.buffers()
            .find_by_name("*init-log*")
            .map(|b| b.text().to_bytes())
    }

    #[test]
    fn a_rejected_init_is_logged_in_full_and_echoed_by_its_first_line() {
        // I-55
        let ed = record_init_error(
            editor().set_message("earlier"),
            "init.lua:2: boom
traceback",
        );
        assert_eq!(ed.message(), "earlier; init.lua: init.lua:2: boom");
        assert_eq!(
            init_log(&ed),
            Some(
                b"init.lua:2: boom
traceback"
                    .to_vec()
            )
        );
        assert_eq!(ed.buffer().name(), "*scratch*");
        assert_eq!(ed.leaf_count(), 1);
        assert!(!ed.buffers().find_by_name("*init-log*").unwrap().modified());
    }

    #[test]
    fn init_loaded_reports_every_outcome() {
        let ed = init_loaded(
            editor().set_message("old"),
            Some(InitLoad::Rejected(
                "bad
more"
                    .into(),
            )),
        );
        assert_eq!(ed.message(), "init.lua: bad");
        assert_eq!(
            init_log(&ed),
            Some(
                b"bad
more"
                    .to_vec()
            )
        );

        let ok = init_loaded(ed.clone(), Some(InitLoad::Applied));
        assert_eq!(ok.message(), "Loaded init.lua");
        assert_eq!(init_log(&ok), Some(Vec::new()));

        let none = init_loaded(ed, Some(InitLoad::Absent));
        assert_eq!(none.message(), "No init.lua");
        assert_eq!(init_log(&none), Some(Vec::new()));

        let fresh = init_loaded(editor(), Some(InitLoad::Applied));
        assert_eq!(init_log(&fresh), None);

        let lua = init_loaded(editor(), None);
        assert_eq!(lua.message(), "reload-init: not available from Lua");
    }

    #[test]
    fn a_file_buffer_named_like_the_log_is_never_overwritten() {
        use crate::file::{DiskState, FilePath};
        use std::path::Path;
        let path = FilePath::resolve(Path::new(""), Path::new("*init-log*"));
        let (ed, file) = editor().add_buffer(
            "*init-log*",
            Text::from_bytes(b"mine"),
            Backing::file(path, DiskState::Unknown),
        );
        let ed = ed.with_buffer(file, Buffer::mark_modified);

        let ed = record_init_error(ed, "bad");
        let ed = init_loaded(ed, Some(InitLoad::Applied));
        let kept = ed.buffers().get_by_id(file);
        assert_eq!(kept.text().to_bytes(), b"mine");
        assert!(kept.modified());
        let logs = ed
            .buffers()
            .iter()
            .filter(|b| b.name() == "*init-log*" && b.file_path().is_none())
            .count();
        assert_eq!(logs, 1);
    }

    #[test]
    fn reload_init_asks_the_shell() {
        let (_, fx) = reload_init_cmd(editor(), Vec::new());
        assert_eq!(fx, [Effect::ReloadInit]);
    }

    #[test]
    fn shell_done_and_lua_done_hostile_and_edge_branches() {
        let ed = editor();

        // Empty shell output
        let ed_empty = shell_done(ed.clone(), Ok(String::new()));
        assert_eq!(ed_empty.message(), "(no output)");
        let ed_crlf = shell_done(ed.clone(), Ok("\r\n\r\n".into()));
        assert_eq!(ed_crlf.message(), "(no output)");

        // Shell error output
        let ed_err = shell_done(ed.clone(), Err("command failed: 127".into()));
        assert_eq!(ed_err.message(), "command failed: 127");

        // Shell command with empty string produces no effects
        let fx = run_shell(&ed, "", Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);

        // Multi-line shell output when windows are already split
        let ed_split = split(ed.clone(), Dir::Below);
        assert_eq!(ed_split.leaf_count(), 2);
        let ed_out1 = shell_done(ed_split, Ok("line 1\r\nline 2".into()));
        assert_eq!(ed_out1.leaf_count(), 2);
        assert_eq!(
            ed_out1
                .buffers()
                .iter()
                .find(|b| b.name() == "*Shell Command Output*")
                .map(|b| b.text().to_bytes()),
            Some(b"line 1\nline 2".to_vec())
        );

        // Re-running reuses the existing *Shell Command Output* buffer and selects other window from leaf
        let ed_out2 = shell_done(ed_out1, Ok("new 1\nnew 2\nnew 3".into()));
        assert_eq!(ed_out2.leaf_count(), 2);
        assert_eq!(
            ed_out2
                .buffers()
                .iter()
                .filter(|b| b.name() == "*Shell Command Output*")
                .count(),
            1
        );

        // lua_done success and multi-line error
        let ed_lua_ok = lua_done(ed.clone(), Ok(()));
        assert_eq!(ed_lua_ok.message(), "");

        let ed_lua_err = lua_done(
            ed,
            Err("first error line\nsecond traceback line\nthird line".into()),
        );
        assert_eq!(ed_lua_err.message(), "first error line");
    }
}
