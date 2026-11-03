//! Shell commands and their output (§4, A9): asks the shell to run a command, then shows the
//! result in the echo area or in `*Shell Command Output*`.

use crate::core::commands::buffer_window::{split, switch_to};
use crate::core::commands::prompt::prompt;
use crate::core::editor::{Buffer, Dir, Editor, Purpose};
use crate::core::effect::{Effect, Effects};
use crate::core::file::Backing;
use crate::core::text::Text;

pub(crate) fn shell_command_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Shell command: ", Purpose::ShellCommand), fx)
}

/// Asks the shell to run `command`. The output comes back as `ShellDone`.
pub(crate) fn run_shell(command: &str, mut fx: Effects) -> Effects {
    if command.is_empty() {
        return fx;
    }
    fx.push(Effect::RunShell {
        command: command.to_owned(),
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

/// Puts `text` in the output buffer and shows it in the other window.
fn show_output(ed: Editor, text: &str) -> Editor {
    const NAME: &str = "*Shell Command Output*";
    let found = ed.buffers().find_by_name(NAME).map(Buffer::id);
    let (ed, id) = match found {
        Some(id) => (ed, id),
        None => ed.add_buffer(NAME, Text::default(), Backing::None),
    };
    let mut ed = ed
        .replace_text(id, Text::from_bytes(text.as_bytes()))
        .with_buffer(id, |b| b.mark_clean());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::window::Window;
    use crate::test_support::{editor, run_command, typ};

    #[test]
    fn shell_command_one_line_echoes_many_lines_open_buffer() {
        let ed = editor();
        let ed = run_command(ed, |e, fx| (e, run_shell("echo hi", fx)));
        assert_eq!(ed.message(), "hi");

        let ed = run_command(ed, |e, fx| (e, run_shell("echo a&& echo b", fx)));
        let out = ed
            .buffers()
            .iter()
            .find(|b| b.name() == "*Shell Command Output*")
            .expect("output buffer");
        assert_eq!(out.text().to_bytes(), b"a\nb");
        assert_eq!(ed.leaf_count(), 2);
    }

    fn output_id(ed: &Editor) -> crate::core::editor::BufferId {
        ed.buffers()
            .iter()
            .find(|b| b.name() == "*Shell Command Output*")
            .map(Buffer::id)
            .expect("output buffer")
    }

    #[test]
    fn shell_output_replacement_resets_undo() {
        let ed = run_command(editor(), |e, fx| (e, run_shell("echo a&& echo b", fx)));
        let out = output_id(&ed);
        let ed = switch_to(ed, out);
        let ed = typ(ed, "X");

        let ed = run_command(ed, |e, fx| (e, run_shell("echo c&& echo d", fx)));
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(ed.message(), "No further undo information");
        assert_eq!(
            ed.buffers().get_by_id(output_id(&ed)).text().to_bytes(),
            b"c\nd"
        );
    }

    #[test]
    fn shell_output_replacement_snaps_windows() {
        let ed = run_command(editor(), |e, fx| {
            (e, run_shell("echo aaaaaaaaaaaa&& echo b", fx))
        });
        let out = output_id(&ed);
        let ed = switch_to(ed, out);
        let ed = ed.with_window(|w| Window {
            point: 12,
            top: 1,
            ..w
        });

        let ed = run_command(ed, |e, fx| (e, run_shell("echo c&& echo d", fx)));
        let len = ed.buffers().get_by_id(output_id(&ed)).text().len();
        assert!(ed.window().point <= len);
        assert_eq!(ed.window().top, 0);
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
        let fx = run_shell("", Vec::new());
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
