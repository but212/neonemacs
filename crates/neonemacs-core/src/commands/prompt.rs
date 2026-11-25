//! Minibuffer prompts, M-x completions, and command submission (§4, §12).

use crate::buffer::SCRATCH_NAME;
use crate::commands::BUILTINS;
use crate::commands::basic::{kill_buffer, switch_to};
use crate::commands::file::write_file;
use crate::commands::load::find_file;
use crate::commands::search::start_query_replace;
use crate::commands::shell_output::run_shell;
use crate::editor::{Buffer, CANCEL_MESSAGE, Editor, Mode, Prompt, Purpose};
use crate::effect::Effects;
use crate::file::Backing;
use crate::keymap::{Code, Key};
use crate::{Doc, goto_line_offset};
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

pub(crate) fn prompt(ed: Editor, label: &str, purpose: Purpose) -> Editor {
    ed.set_message(label)
        .set_mode(Mode::Prompt(Prompt::new(label, purpose)))
}

/// Every built-in command name and registered Lua command with the keys bound to it,
/// for `M-x` completion (§4, §12).
pub fn completions(ed: &Editor, prefix: &str) -> Vec<(String, String)> {
    let names: alloc::collections::BTreeSet<&str> = BUILTINS
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
    let input = p.input();
    let cands = candidates(ed, &p.purpose, &input);
    match cands.as_slice() {
        [] => (p, Some("No match".to_owned())),
        [(name, _)] => {
            p.before.clone_from(name);
            p.after.clear();
            (p, None)
        }
        _ => {
            let names: Vec<&str> = cands.iter().map(|(n, _)| n.as_str()).collect();
            let common = common_prefix(&names);
            if common.len() > input.len() {
                p.before = common;
                p.after.clear();
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
#[must_use]
pub fn prompt_key(ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    let Mode::Prompt(mut p) = ed.mode().clone() else {
        return (ed, fx);
    };
    if key == Key::ctrl('g') || key == Key::plain(Code::Esc) {
        let ed = ed.set_mode(Mode::normal()).set_message(CANCEL_MESSAGE);
        return (ed, fx);
    }
    if key == Key::plain(Code::Enter) {
        let ed = ed.set_mode(Mode::normal()).clear_message();
        return submit(ed, p, fx);
    }
    let mut note = None;
    if key == Key::plain(Code::Backspace) {
        p.before.pop();
    } else if key == Key::plain(Code::Tab) {
        (p, note) = complete(&ed, p);
    } else if key == Key::ctrl('b') || key == Key::plain(Code::Left) {
        if let Some(c) = p.before.pop() {
            p.after.insert(0, c);
        }
    } else if key == Key::ctrl('f') || key == Key::plain(Code::Right) {
        if !p.after.is_empty() {
            let c = p.after.remove(0);
            p.before.push(c);
        }
    } else if key == Key::ctrl('a') || key == Key::plain(Code::Home) {
        p.after = p.input();
        p.before.clear();
    } else if key == Key::ctrl('e') || key == Key::plain(Code::End) {
        p.before = p.input();
        p.after.clear();
    } else if key == Key::ctrl('d') {
        if !p.after.is_empty() {
            p.after.remove(0);
        }
    } else if let Some(c) = key.text() {
        p.before.push(c);
    }
    let msg = note.unwrap_or_else(|| p.echo());
    let ed = ed.set_message(msg).set_mode(Mode::Prompt(p));
    (ed, fx)
}

fn submit(ed: Editor, p: Prompt, fx: Effects) -> (Editor, Effects) {
    let input = p.input();
    match p.purpose {
        Purpose::ExecuteCommand => crate::dispatch::run_named(ed, &input, fx),
        Purpose::FindFile => find_file(ed, &input, fx),
        Purpose::WriteFile => write_file(ed, &input, fx),
        Purpose::SwitchBuffer => (switch_to_named(ed, &input), fx),
        Purpose::KillBuffer => kill_named(ed, &input, fx),
        Purpose::GotoLine => (goto_line(ed, &input), fx),
        Purpose::ShellCommand => {
            let fx = run_shell(&ed, &input, fx);
            (ed, fx)
        }
        Purpose::QueryFrom { kind } => {
            if input.is_empty() {
                (ed.set_message("Query replace needs a pattern"), fx)
            } else {
                let label = format!("Query replace {input} with: ");
                (
                    prompt(ed, &label, Purpose::QueryTo { kind, from: input }),
                    fx,
                )
            }
        }
        Purpose::QueryTo { kind, from } => crate::dispatch::command(ed, fx, |e, fx| {
            let (e, result) = start_query_replace(e, &from, &input, kind);
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
    let found = ed.buffers().find_named(name).map(Buffer::id);
    if let Some(id) = found {
        switch_to(ed, id)
    } else {
        let backing = if name == SCRATCH_NAME {
            Backing::scratch()
        } else {
            Backing::None
        };
        let (ed, id) = ed.add_buffer(name, Doc::default(), backing);
        switch_to(ed, id)
    }
}

fn kill_named(ed: Editor, name: &str, fx: Effects) -> (Editor, Effects) {
    let id = if name.is_empty() {
        Some(ed.window().buffer)
    } else {
        ed.buffers().find_named(name).map(Buffer::id)
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
    let pos = goto_line_offset(ed.buffer().text(), n);
    ed.move_point(pos)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::editor::Prefix;
    use crate::effect::Effect;
    use crate::test_support::{editor, keys, typ};

    #[test]
    fn mx_completes_and_cancels_on_ctrl_g_and_esc() {
        for cancel in ["C-g", "ESC"] {
            let ed = keys(editor(), "M-x");
            assert!(matches!(ed.mode(), Mode::Prompt(_)));
            let ed = typ(ed, "for");
            let ed = keys(ed, "TAB");
            assert!(matches!(ed.mode(), Mode::Prompt(p) if p.input() == "forward-char"));
            let ed = keys(ed, cancel);
            assert_eq!(ed.mode(), &Mode::normal());
            assert_eq!(ed.message(), "Quit");
        }
    }

    #[test]
    fn mx_lists_candidates_with_their_keys() {
        let ed = editor();
        assert!(completions(&ed, "und").contains(&("undo".to_owned(), "C-_".to_owned())));
        assert_eq!(completions(&ed, "zzz"), [] as [(String, String); 0]);
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
            before: "is".into(),
            after: String::new(),
            purpose: Purpose::ExecuteCommand,
        };
        let (p, note) = complete(&ed, p);
        assert_eq!(p.input(), "isearch-");
        let note = note.expect("should produce candidates note");
        assert!(note.contains("isearch-forward"));
        assert!(note.contains("isearch-backward"));

        // No match candidate
        let p_none = Prompt {
            label: "M-x ".into(),
            before: "nonexistent_command_prefix".into(),
            after: String::new(),
            purpose: Purpose::ExecuteCommand,
        };
        let (p_none, note_none) = complete(&ed, p_none);
        assert_eq!(p_none.input(), "nonexistent_command_prefix");
        assert_eq!(note_none.as_deref(), Some("No match"));

        // Buffer candidates completion
        let (ed, _) = ed.add_buffer("alpha_1", Doc::default(), Backing::None);
        let (ed, _) = ed.add_buffer("alpha_2", Doc::default(), Backing::None);
        let p_buf = Prompt {
            label: "Switch to buffer: ".into(),
            before: "alph".into(),
            after: String::new(),
            purpose: Purpose::SwitchBuffer,
        };
        let (p_buf, note_buf) = complete(&ed, p_buf);
        assert_eq!(p_buf.input(), "alpha_");
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
        assert_eq!(same_fx, [] as [Effect; 0]);
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
        assert!(all.iter().any(|(name, _)| name == "kill-region"));
    }

    fn prompt_of(ed: &Editor) -> Prompt {
        match ed.mode() {
            Mode::Prompt(p) => p.clone(),
            other => panic!("not a prompt: {other:?}"),
        }
    }

    #[test]
    fn prompt_inline_editing_moves_inserts_and_deletes_at_cursor() {
        let ed = keys(editor(), "M-x");
        let ed = keys(ed, "a b c");
        let ed = keys(ed, "C-b C-b");
        let p = prompt_of(&ed);
        assert_eq!((p.before.as_str(), p.after.as_str()), ("a", "bc"));
        let ed = keys(ed, "x");
        assert_eq!(prompt_of(&ed).input(), "axbc");
        let ed = keys(ed, "C-d");
        assert_eq!(prompt_of(&ed).input(), "axc");
        let ed = keys(ed, "DEL");
        assert_eq!(prompt_of(&ed).input(), "ac");
        let ed = keys(ed, "C-a");
        assert_eq!(prompt_of(&ed).before, "");
        let ed = keys(ed, "C-d C-e");
        let p = prompt_of(&ed);
        assert_eq!((p.before.as_str(), p.after.as_str()), ("c", ""));
        assert_eq!(ed.message(), "M-x c");
    }

    #[test]
    fn prompt_cursor_stops_at_both_ends_and_handles_hangul() {
        let ed = keys(editor(), "M-x");
        let ed = typ_prompt(ed, "가나");
        let ed = keys(ed, "left left left right");
        let p = prompt_of(&ed);
        assert_eq!((p.before.as_str(), p.after.as_str()), ("가", "나"));
        let ed = keys(ed, "right right C-d");
        assert_eq!(prompt_of(&ed).input(), "가나");
        assert_eq!(
            prompt_of(&ed).cursor_width(crate::width::Metrics { tab: 8, cjk: false }),
            8
        );
    }

    #[test]
    fn switching_to_a_missing_scratch_makes_the_protected_one() {
        // I-42: the user cannot end up with an unprotected buffer named *scratch*.
        let (ed, id) = editor().add_buffer("other", Doc::default(), Backing::None);
        let ed = switch_to_named(ed, "other");
        let (ed, _) = kill_named(ed, "*scratch*", Vec::new());
        let ed = switch_to_named(ed, "*scratch*");
        let found = ed.buffers().find_by_name("*scratch*").unwrap();
        assert!(found.backing().is_scratch());
        assert_ne!(found.id(), id);
    }

    #[test]
    fn switching_prefers_the_buffer_without_a_file_among_equal_names() {
        // A file named like `*init-log*` must not hide the log, whichever was made first.
        use crate::file::{DiskState, FilePath};
        let path = FilePath::resolve("", "*init-log*");
        let (ed, file) = editor().add_buffer(
            "*init-log*",
            Doc::default(),
            Backing::file(path, DiskState::Unknown),
        );
        let ed = switch_to_named(ed, "*init-log*");
        assert_eq!(ed.window().buffer, file);

        let (ed, log) = ed.add_buffer("*init-log*", Doc::default(), Backing::None);
        let ed = switch_to_named(ed, "*init-log*");
        assert_eq!(ed.window().buffer, log);

        // `kill-buffer` resolves the name the same way.
        let (ed, _) = kill_named(ed, "*init-log*", Vec::new());
        assert!(ed.buffers().iter().all(|b| b.id() != log));
        assert!(ed.buffers().iter().any(|b| b.id() == file));
    }

    fn typ_prompt(ed: Editor, s: &str) -> Editor {
        s.chars().fold(ed, |ed, c| {
            prompt_key(ed, Key::plain(Code::Char(c)), Vec::new()).0
        })
    }

    #[test]
    fn prompt_completion_replaces_whole_input_and_moves_cursor_to_end() {
        let ed = typ_prompt(keys(editor(), "M-x"), "kill-reg");
        let ed = keys(ed, "C-b");
        let ed = keys(ed, "TAB");
        let p = prompt_of(&ed);
        assert_eq!((p.before.as_str(), p.after.as_str()), ("kill-region", ""));
    }
}
