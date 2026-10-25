//! Minibuffer prompts, M-x completions, and command submission (§4, §12).

use crate::core::commands::buffer_window::{kill_buffer, split, switch_to};
use crate::core::commands::file::{find_file, write_file};
use crate::core::commands::search::start_query_replace;
use crate::core::commands::table::BUILTINS;
use crate::core::editor::{Buffer, Dir, Editor, Mode, Prefix, Prompt, Purpose};
use crate::core::effect::{Effect, Effects};
use crate::core::file::Backing;
use crate::core::keymap::{Code, Key};
use crate::core::text::Text;

pub(crate) fn execute_extended_command(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "M-x ", Purpose::ExecuteCommand), fx)
}

pub(crate) fn find_file_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Find file: ", Purpose::FindFile), fx)
}

pub(crate) fn write_file_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Write file: ", Purpose::WriteFile), fx)
}

pub(crate) fn switch_to_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Switch to buffer: ", Purpose::SwitchBuffer), fx)
}

pub(crate) fn kill_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Kill buffer: ", Purpose::KillBuffer), fx)
}

pub(crate) fn goto_line_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Goto line: ", Purpose::GotoLine), fx)
}

pub(crate) fn prompt(ed: Editor, label: &str, purpose: Purpose) -> Editor {
    ed.set_message(label).set_mode(Mode::Prompt(Prompt {
        label: label.to_owned(),
        input: String::new(),
        purpose,
    }))
}

/// Every built-in command name and registered Lua command with the keys bound to it,
/// for `M-x` completion (§4, §12).
pub fn completions(ed: &Editor, prefix: &str) -> Vec<(String, String)> {
    let names: std::collections::BTreeSet<&str> = BUILTINS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| n.starts_with(prefix))
        .chain(ed.commands().completions(prefix))
        .collect();

    names
        .into_iter()
        .map(|n| (n.to_string(), ed.keymap().keys_for(n).join(", ")))
        .collect()
}

fn candidates(ed: &Editor, purpose: &Purpose, input: &str) -> Vec<(String, String)> {
    match purpose {
        Purpose::ExecuteCommand => completions(ed, input),
        Purpose::SwitchBuffer | Purpose::KillBuffer => ed
            .buffers()
            .iter()
            .filter(|b| b.name().starts_with(input))
            .map(|b| (b.name().to_owned(), String::new()))
            .collect(),
        _ => Vec::new(),
    }
}

/// TAB: extends the input to the common prefix, or lists the candidates.
/// Returns the prompt and the message to show, if any.
fn complete(ed: &Editor, mut p: Prompt) -> (Prompt, Option<String>) {
    let cands = candidates(ed, &p.purpose, &p.input);
    match cands.as_slice() {
        [] => (p, Some("No match".to_owned())),
        [(name, _)] => {
            p.input = name.clone();
            (p, None)
        }
        _ => {
            let names: Vec<&str> = cands.iter().map(|(n, _)| n.as_str()).collect();
            let common = common_prefix(&names);
            if common.len() > p.input.len() {
                p.input = common;
            }
            let list = cands
                .iter()
                .map(|(n, k)| {
                    if k.is_empty() {
                        n.clone()
                    } else {
                        format!("{n} ({k})")
                    }
                })
                .collect::<Vec<_>>()
                .join("  ");
            (p, Some(list))
        }
    }
}

fn common_prefix(names: &[&str]) -> String {
    let Some((first, rest)) = names.split_first() else {
        return String::new();
    };
    let mut end = first.len();
    for n in rest {
        let shared: usize = first
            .chars()
            .zip(n.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        end = end.min(shared);
    }
    first[..end].to_owned()
}

/// Keys inside a prompt: text edits the input, RET submits, C-g cancels, TAB completes.
pub fn prompt_key(ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    let Mode::Prompt(mut p) = ed.mode().clone() else {
        return (ed, fx);
    };
    if key == Key::ctrl('g') {
        let ed = ed
            .set_mode(Mode::Normal {
                prefix: Prefix::None,
            })
            .set_message("Quit");
        return (ed, fx);
    }
    if key == Key::plain(Code::Enter) {
        let ed = ed
            .set_mode(Mode::Normal {
                prefix: Prefix::None,
            })
            .clear_message();
        return submit(ed, p, fx);
    }
    let mut note = None;
    if key == Key::plain(Code::Backspace) {
        p.input.pop();
    } else if key == Key::plain(Code::Tab) {
        (p, note) = complete(&ed, p);
    } else if let Some(c) = key.text() {
        p.input.push(c);
    }
    let msg = note.unwrap_or_else(|| format!("{}{}", p.label, p.input));
    let ed = ed.set_message(msg).set_mode(Mode::Prompt(p));
    (ed, fx)
}

fn submit(ed: Editor, p: Prompt, fx: Effects) -> (Editor, Effects) {
    let input = p.input;
    match p.purpose {
        Purpose::ExecuteCommand => crate::core::dispatch::run_named(ed, &input, fx),
        Purpose::FindFile => find_file(ed, &input, fx),
        Purpose::WriteFile => write_file(ed, &input, fx),
        Purpose::SwitchBuffer => (switch_to_named(ed, &input), fx),
        Purpose::KillBuffer => kill_named(ed, &input, fx),
        Purpose::GotoLine => (goto_line(ed, &input), fx),
        Purpose::ShellCommand => {
            let fx = run_shell(&input, fx);
            (ed, fx)
        }
        Purpose::QueryFrom { regex } => {
            if input.is_empty() {
                (ed.set_message("Query replace needs a pattern"), fx)
            } else {
                let label = format!("Query replace {input} with: ");
                (
                    prompt(ed, &label, Purpose::QueryTo { regex, from: input }),
                    fx,
                )
            }
        }
        Purpose::QueryTo { regex, from } => crate::core::dispatch::command(ed, fx, |e, fx| {
            let (e, result) = start_query_replace(e, &from, &input, regex);
            let e = match result {
                Ok(()) => e,
                Err(msg) => e.set_message(msg),
            };
            (e, fx)
        }),
    }
}

fn switch_to_named(ed: Editor, name: &str) -> Editor {
    if name.is_empty() {
        return ed;
    }
    let found = ed
        .buffers()
        .iter()
        .find(|b| b.name() == name)
        .map(Buffer::id);
    match found {
        Some(id) => switch_to(ed, id),
        None => {
            let (ed, id) = ed.add_buffer(name, Text::default(), Backing::None);
            switch_to(ed, id)
        }
    }
}

fn kill_named(ed: Editor, name: &str, fx: Effects) -> (Editor, Effects) {
    let id = if name.is_empty() {
        Some(ed.window().buffer)
    } else {
        ed.buffers()
            .iter()
            .find(|b| b.name() == name)
            .map(Buffer::id)
    };
    match id {
        Some(id) => kill_buffer(ed, id, fx),
        None => (ed.set_message(format!("No buffer named {name}")), fx),
    }
}

fn goto_line(ed: Editor, input: &str) -> Editor {
    let Ok(n) = input.trim().parse::<usize>() else {
        return ed.set_message(format!("Not a line number: {input}"));
    };
    let pos = ed.buffer().text().nth_line_start(n);
    ed.move_point(pos)
}

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
    let found = ed
        .buffers()
        .iter()
        .find(|b| b.name() == NAME)
        .map(Buffer::id);
    let (ed, id) = match found {
        Some(id) => (ed, id),
        None => ed.add_buffer(NAME, Text::default(), Backing::None),
    };
    let mut ed = ed
        .replace_text(id, Text::from_bytes(text.as_bytes()))
        .with_buffer(id, |b| b.set_modified(false));
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
    use crate::test_support::{editor, keys, run_command, typ};

    #[test]
    fn mx_completes_and_cancels() {
        let ed = keys(editor(), "M-x");
        assert!(matches!(ed.mode(), Mode::Prompt(_)));
        let ed = typ(ed, "for");
        let ed = keys(ed, "TAB");
        assert!(matches!(ed.mode(), Mode::Prompt(p) if p.input == "forward-char"));
        let ed = keys(ed, "C-g");
        assert_eq!(
            ed.mode(),
            &Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn mx_lists_candidates_with_their_keys() {
        let ed = editor();
        assert!(completions(&ed, "und").contains(&("undo".to_owned(), "C-_".to_owned())));
        assert!(completions(&ed, "zzz").is_empty());
    }

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
    fn common_prefix_edge_cases() {
        assert_eq!(common_prefix(&[]), "");
        assert_eq!(common_prefix(&["only"]), "only");
        assert_eq!(common_prefix(&["abc", "xyz"]), "");
        assert_eq!(
            common_prefix(&["prefix_a", "prefix_b", "prefix_c"]),
            "prefix_"
        );
        assert_eq!(common_prefix(&["short", "shorthand"]), "short");
        // UTF-8 multibyte characters and emojis
        assert_eq!(common_prefix(&["가나다라", "가나다마"]), "가나다");
        assert_eq!(common_prefix(&["가나", "각"]), "");
        assert_eq!(common_prefix(&["🦀rust", "🦀ruby"]), "🦀ru");
    }

    #[test]
    fn completion_multi_candidate_and_no_match() {
        let ed = editor();
        // Multi candidate expands to common prefix and formats candidate notes
        let p = Prompt {
            label: "M-x ".into(),
            input: "is".into(),
            purpose: Purpose::ExecuteCommand,
        };
        let (p, note) = complete(&ed, p);
        assert_eq!(p.input, "isearch-");
        let note = note.expect("should produce candidates note");
        assert!(note.contains("isearch-forward"));
        assert!(note.contains("isearch-backward"));

        // No match candidate
        let p_none = Prompt {
            label: "M-x ".into(),
            input: "nonexistent_command_prefix".into(),
            purpose: Purpose::ExecuteCommand,
        };
        let (p_none, note_none) = complete(&ed, p_none);
        assert_eq!(p_none.input, "nonexistent_command_prefix");
        assert_eq!(note_none.as_deref(), Some("No match"));

        // Buffer candidates completion
        let (ed, _) = ed.add_buffer("alpha_1", Text::default(), Backing::None);
        let (ed, _) = ed.add_buffer("alpha_2", Text::default(), Backing::None);
        let p_buf = Prompt {
            label: "Switch to buffer: ".into(),
            input: "alph".into(),
            purpose: Purpose::SwitchBuffer,
        };
        let (p_buf, note_buf) = complete(&ed, p_buf);
        assert_eq!(p_buf.input, "alpha_");
        let note_buf = note_buf.expect("should produce buffer candidates note");
        assert!(note_buf.contains("alpha_1"));
        assert!(note_buf.contains("alpha_2"));
    }

    #[test]
    fn prompt_key_non_prompt_mode_is_noop() {
        let ed = editor();
        assert_eq!(
            ed.mode(),
            &Mode::Normal {
                prefix: Prefix::None
            }
        );
        let (same_ed, same_fx) = prompt_key(ed.clone(), Key::plain(Code::Char('a')), Vec::new());
        assert_eq!(same_ed.mode(), ed.mode());
        assert!(same_fx.is_empty());
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
        assert!(fx.is_empty());

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
        assert!(ed_lua_ok.message().is_empty());

        let ed_lua_err = lua_done(
            ed,
            Err("first error line\nsecond traceback line\nthird line".into()),
        );
        assert_eq!(ed_lua_err.message(), "first error line");
    }

    #[test]
    fn completions_includes_registered_lua_commands() {
        let ed = editor()
            .register_command("forward-word")
            .register_command("fill-paragraph");
        let cands = completions(&ed, "fill-");
        assert_eq!(cands, vec![("fill-paragraph".to_string(), String::new())]);

        let all = completions(&ed, "");
        assert!(all.iter().any(|(name, _)| name == "forward-word"));
        assert!(all.iter().any(|(name, _)| name == "fill-paragraph"));
        assert!(all.iter().any(|(name, _)| name == "kill-line"));
    }
}
