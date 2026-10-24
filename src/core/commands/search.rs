//! Incremental search and interactive query-replace commands (§9).

use crate::core::commands::prompt::prompt;
use crate::core::editor::{Editor, Isearch, Mode, Prefix, Purpose, QueryReplace};
use crate::core::effect::Effects;
use crate::core::keymap::{Code, Key};
use crate::core::search::{self, Pattern};
use crate::core::undo::Edit;

pub(crate) fn isearch_forward(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, true, false), fx)
}

pub(crate) fn isearch_backward(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, false, false), fx)
}

pub(crate) fn isearch_forward_regexp(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, true, true), fx)
}

pub(crate) fn isearch_backward_regexp(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, false, true), fx)
}

fn start_isearch(ed: Editor, forward: bool, regex: bool) -> Editor {
    let origin = ed.window().point;
    ed.set_mode(Mode::Isearch(Isearch {
        forward,
        regex,
        text: String::new(),
        origin,
        anchor: origin,
        failed: false,
    }))
    .set_message("I-search: ")
}

/// Keys during an incremental search. `C-s` and `C-r` move to the next match
/// in each direction. Any other non-text key ends the search, then runs normally.
pub fn isearch_key(ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    let Mode::Isearch(mut s) = ed.mode().clone() else {
        return (ed, fx);
    };
    if key == Key::ctrl('g') {
        let ed = ed
            .set_mode(Mode::Normal {
                prefix: Prefix::None,
            })
            .move_point(s.origin)
            .set_message("Quit");
        return (ed, fx);
    }
    if key == Key::plain(Code::Enter) {
        let ed = ed
            .set_mode(Mode::Normal {
                prefix: Prefix::None,
            })
            .clear_message();
        return (ed, fx);
    }
    let (ed, s) = if key == Key::ctrl('s') || key == Key::ctrl('r') {
        s.forward = key == Key::ctrl('s');
        let from = ed.window().point;
        isearch_step(ed, s, from, true)
    } else if key == Key::plain(Code::Backspace) {
        s.text.pop();
        let origin = s.origin;
        s.anchor = origin;
        isearch_step(ed, s, origin, false)
    } else if let Some(c) = key.text() {
        s.text.push(c);
        let anchor = s.anchor;
        isearch_step(ed, s, anchor, false)
    } else {
        let ed = ed.set_mode(Mode::Normal {
            prefix: Prefix::None,
        });
        return crate::core::dispatch::handle_normal_key(ed, Prefix::None, key, fx);
    };
    (ed.set_mode(Mode::Isearch(s)), fx)
}

/// Searches from `from` in the direction of `s` and moves point to the match.
/// With `wrap`, a miss restarts at the other end of the buffer.
fn isearch_step(ed: Editor, mut s: Isearch, from: usize, wrap: bool) -> (Editor, Isearch) {
    if s.text.is_empty() {
        s.failed = false;
        return (ed.set_message("I-search: "), s);
    }
    let pattern = if s.regex {
        Pattern::regex(&s.text)
    } else {
        Ok(Pattern::literal(&s.text))
    };
    let pattern = match pattern {
        Ok(p) => p,
        Err(e) => {
            s.failed = true;
            return (ed.set_message(format!("Invalid regexp: {e}")), s);
        }
    };
    let fold = ed.config().case_fold_search;
    let mut wrapped = false;
    let mut found = find_in_direction(&ed, &pattern, s.forward, from, fold);
    if found.is_none() && wrap {
        let other_end = if s.forward { 0 } else { ed.buffer().text.len() };
        found = find_in_direction(&ed, &pattern, s.forward, other_end, fold);
        wrapped = found.is_some();
    }
    s.failed = found.is_none();
    let ed = if let Some((start, end)) = found {
        s.anchor = start;
        ed.move_point(if s.forward { end } else { start })
    } else {
        ed
    };
    let failing = if s.failed { "Failing " } else { "" };
    let wrap_note = if wrapped { "Wrapped " } else { "" };
    let ed = ed.set_message(format!("{wrap_note}{failing}I-search: {}", s.text));
    (ed, s)
}

fn find_in_direction(
    ed: &Editor,
    pattern: &Pattern,
    forward: bool,
    from: usize,
    fold: bool,
) -> Option<(usize, usize)> {
    let text = &ed.buffer().text;
    if forward {
        search::find_forward(text, pattern, from, fold)
    } else {
        search::find_backward(text, pattern, from, fold)
    }
}

/// Starts a query-replace session from point. Errors on a bad regexp or an empty pattern.
pub fn start_query_replace(
    ed: Editor,
    pattern: &str,
    replacement: &str,
    regex: bool,
) -> (Editor, Result<(), String>) {
    if pattern.is_empty() {
        return (ed, Err("empty pattern".to_owned()));
    }
    let pattern = if regex {
        match Pattern::regex(pattern) {
            Ok(p) => p,
            Err(e) => return (ed, Err(e)),
        }
    } else {
        Pattern::literal(pattern)
    };
    let mut q = QueryReplace {
        pattern,
        replacement: replacement.to_owned(),
        regex,
        fold: ed.config().case_fold_search,
        current: None,
        count: 0,
    };
    let from = ed.window().point;
    q.current = next_match(&ed, &q, from);
    if q.current.is_none() {
        return (ed.set_message("No match"), Ok(()));
    }
    let ed = ed
        .set_mode(Mode::QueryReplace(q))
        .set_message("Query replacing (y, n, !, q)");
    (ed, Ok(()))
}

fn next_match(ed: &Editor, q: &QueryReplace, from: usize) -> Option<(usize, usize)> {
    if from > ed.buffer().text.len() {
        return None;
    }
    search::find_forward(&ed.buffer().text, &q.pattern, from, q.fold)
}

/// Answers for query-replace: `y`/SPC replace, `n`/DEL skip, `!` replace the rest,
/// `q`/RET/`C-g` stop (A8). The whole session is one undo group.
pub fn query_key(ed: Editor, key: Key) -> Editor {
    let Mode::QueryReplace(mut q) = ed.mode().clone() else {
        return ed;
    };
    let Some((start, end)) = q.current else {
        return finish(ed, &q);
    };
    match key.text() {
        Some('y' | ' ') => match replace_match(ed, &q, start, end) {
            (ed, Some(from)) => {
                q.count += 1;
                step(ed, q, from)
            }
            (ed, None) => finish(ed, &q),
        },
        Some('n') => {
            let from = resume(&ed, end, start == end);
            step(ed, q, from)
        }
        None if key == Key::plain(Code::Backspace) => {
            let from = resume(&ed, end, start == end);
            step(ed, q, from)
        }
        Some('!') => {
            let mut ed = ed;
            let mut current = Some((start, end));
            while let Some((s, e)) = current {
                let (next, from) = replace_match(ed, &q, s, e);
                ed = next;
                let Some(from) = from else {
                    break;
                };
                q.count += 1;
                current = next_match(&ed, &q, from);
            }
            q.current = None;
            finish(ed, &q)
        }
        Some('q' | '\n') => finish(ed, &q),
        None if key == Key::ctrl('g') => finish(ed, &q),
        _ => ed,
    }
}

/// Replaces `start..end` and returns the editor and where the next search starts.
/// `None` when the buffer refuses the edit.
fn replace_match(
    ed: Editor,
    q: &QueryReplace,
    start: usize,
    end: usize,
) -> (Editor, Option<usize>) {
    let matched = ed.buffer().text.bytes(start..end);
    let replacement = if q.regex {
        search::expand_replacement(&q.replacement, &matched)
    } else {
        q.replacement.as_bytes().to_vec()
    };
    let after = start + replacement.len();
    let (ed, applied) = ed.apply(Edit::Delete {
        pos: start,
        bytes: matched,
    });
    if applied.is_none() {
        return (ed, None);
    }
    let (ed, applied) = ed.apply(Edit::Insert {
        pos: start,
        bytes: replacement,
    });
    if applied.is_none() {
        return (ed, None);
    }
    let ed = ed.move_point(after);
    let from = resume(&ed, after, start == end);
    (ed, Some(from))
}

/// Next search start after a match ending at `after`. An empty match must move
/// past one unit, or the search would find it again forever.
fn resume(ed: &Editor, after: usize, matched_empty: bool) -> usize {
    let text = &ed.buffer().text;
    if !matched_empty {
        after
    } else if after >= text.len() {
        // Past the end: no later match can start, so the search finishes.
        text.len() + 1
    } else {
        text.next_boundary(after)
    }
}

fn step(ed: Editor, mut q: QueryReplace, from: usize) -> Editor {
    q.current = next_match(&ed, &q, from);
    if q.current.is_none() {
        finish(ed, &q)
    } else {
        ed.set_mode(Mode::QueryReplace(q))
    }
}

fn finish(ed: Editor, q: &QueryReplace) -> Editor {
    ed.set_mode(Mode::Normal {
        prefix: Prefix::None,
    })
    .boundary()
    .set_message(format!("Replaced {} occurrence(s)", q.count))
}

pub(crate) fn query_replace_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        prompt(ed, "Query replace: ", Purpose::QueryFrom { regex: false }),
        fx,
    )
}

pub(crate) fn query_replace_regexp_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        prompt(
            ed,
            "Query replace regexp: ",
            Purpose::QueryFrom { regex: true },
        ),
        fx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{editor, keys, point, run_command, text, typ};

    fn start_query(ed: Editor, pattern: &str, replacement: &str, regex: bool) -> Editor {
        let (ed, result) = start_query_replace(ed, pattern, replacement, regex);
        result.unwrap();
        ed
    }

    #[test]
    fn isearch_moves_and_cancels() {
        let ed = typ(editor(), "abc abd");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "a");
        assert_eq!(point(&ed), 1);
        let ed = typ(ed, "b");
        assert_eq!(point(&ed), 2);
        let ed = keys(ed, "C-s");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "DEL");
        assert_eq!(point(&ed), 1);
        let ed = keys(ed, "C-g");
        assert_eq!(point(&ed), 0);
        assert_eq!(
            ed.mode(),
            &Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn isearch_wraps_after_last_match() {
        let ed = typ(editor(), "abc abd");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "c");
        assert_eq!(point(&ed), 3);
        let ed = keys(ed, "C-s");
        assert_eq!(point(&ed), 3);
        assert!(ed.message().contains("Wrapped"));
    }

    #[test]
    fn isearch_regexp_extends_pattern() {
        let ed = typ(editor(), "x1 x22");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, isearch_forward_regexp);
        let ed = typ(ed, "x[0-9]*");
        assert_eq!(point(&ed), 2);
    }

    #[test]
    fn query_replace_answers_each_match() {
        let ed = typ(editor(), "foo bar foo baz foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "foo", "qux", false), fx));
        let ed = keys(ed, "y");
        assert_eq!(text(&ed), "qux bar foo baz foo");
        let ed = keys(ed, "n");
        let ed = keys(ed, "y");
        assert_eq!(text(&ed), "qux bar foo baz qux");
        assert_eq!(
            ed.mode(),
            &Mode::Normal {
                prefix: Prefix::None
            }
        );
        // The whole session is one undo group.
        let ed = keys(ed, "C-/");
        assert_eq!(text(&ed), "foo bar foo baz foo");
    }

    #[test]
    fn query_replace_quits_early() {
        let ed = typ(editor(), "foo bar foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "foo", "qux", false), fx));
        let ed = keys(ed, "y q");
        assert_eq!(text(&ed), "qux bar foo");
        assert_eq!(
            ed.mode(),
            &Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn query_replace_empty_matches_terminate() {
        let ed = typ(editor(), "ab");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "x*", "-", true), fx));
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "-a-b-");
    }

    #[test]
    fn query_replace_regexp_bang_and_ampersand() {
        let ed = typ(editor(), "a1 a2");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "a[0-9]", "<&>", true), fx));
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "<a1> <a2>");

        let lit = typ(editor(), "a1 a2");
        let lit = keys(lit, "C-a");
        let lit = run_command(lit, |e, fx| (start_query(e, "a1", "<&>", false), fx));
        let lit = keys(lit, "!");
        assert_eq!(text(&lit), "<&> a2");

        let esc = typ(editor(), "a1 a2");
        let esc = keys(esc, "C-a");
        let esc = run_command(esc, |e, fx| (start_query(e, "a[0-9]", "\\&", true), fx));
        let esc = keys(esc, "!");
        assert_eq!(text(&esc), "& &");
    }
}
