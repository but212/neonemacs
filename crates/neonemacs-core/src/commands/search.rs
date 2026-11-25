//! Incremental search and interactive query-replace commands (§9).

use crate::commands::prompt::prompt;
use crate::config::Toggles;
use crate::dispatch::answer_char;
use crate::editor::{
    CANCEL_MESSAGE, Direction, Editor, Isearch, MatchKind, Mode, Prefix, Purpose, QueryReplace,
};
use crate::effect::Effects;
use crate::keymap::{Code, Key};
use crate::search::{self, Pattern};
use crate::undo::Edit;
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub(crate) fn isearch_forward(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        start_isearch(ed, Direction::Forward, MatchKind::Literal),
        fx,
    )
}

pub(crate) fn isearch_backward(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        start_isearch(ed, Direction::Backward, MatchKind::Literal),
        fx,
    )
}

pub(crate) fn isearch_forward_regexp(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, Direction::Forward, MatchKind::Regex), fx)
}

pub(crate) fn isearch_backward_regexp(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, Direction::Backward, MatchKind::Regex), fx)
}

fn start_isearch(ed: Editor, direction: Direction, kind: MatchKind) -> Editor {
    let origin = ed.window().point;
    ed.set_mode(Mode::Isearch(Isearch {
        direction,
        kind,
        text: String::new(),
        origin,
        anchor: origin,
        found: None,
    }))
    .set_message("I-search: ")
}

/// Keys during an incremental search. `C-s` and `C-r` move to the next match
/// in each direction. Any other non-text key ends the search, then runs normally.
#[must_use]
pub fn isearch_key(ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    let Mode::Isearch(mut s) = ed.mode().clone() else {
        return (ed, fx);
    };
    if key == Key::ctrl('g') || key == Key::plain(Code::Esc) {
        let ed = ed
            .set_mode(Mode::normal())
            .move_point(s.origin)
            .set_message(CANCEL_MESSAGE);
        return (ed, fx);
    }
    if key == Key::plain(Code::Enter) {
        let ed = ed.set_mode(Mode::normal()).clear_message();
        return (ed, fx);
    }
    let (ed, s) = if key == Key::ctrl('s') || key == Key::ctrl('r') {
        s.direction = if key == Key::ctrl('s') {
            Direction::Forward
        } else {
            Direction::Backward
        };
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
        let ed = ed.set_mode(Mode::normal());
        return crate::dispatch::handle_normal_key(ed, Prefix::None, key, fx);
    };
    (ed.set_mode(Mode::Isearch(s)), fx)
}

/// Searches from `from` in the direction of `s` and moves point to the match.
/// With `wrap`, a miss restarts at the other end of the buffer.
fn isearch_step(ed: Editor, mut s: Isearch, from: usize, wrap: bool) -> (Editor, Isearch) {
    if s.text.is_empty() {
        s.found = None;
        return (ed.set_message("I-search: "), s);
    }
    let pattern = match s.kind {
        MatchKind::Regex => Pattern::regex(&s.text),
        MatchKind::Literal => Ok(Pattern::literal(&s.text)),
    };
    let pattern = match pattern {
        Ok(p) => p,
        Err(e) => {
            s.found = None;
            return (ed.set_message(format!("Invalid regexp: {e}")), s);
        }
    };
    let fold = ed.config().toggles.contains(Toggles::CASE_FOLD_SEARCH);
    let mut wrapped = false;
    let text = ed.buffer().text();
    let mut found = if !wrap && s.direction == Direction::Backward && from <= text.total_bytes() {
        pattern
            .match_at(text, from, fold)
            .map(|end| (from, end))
            .or_else(|| find_in_direction(&ed, &pattern, s.direction, from, fold))
    } else {
        find_in_direction(&ed, &pattern, s.direction, from, fold)
    };
    if found.is_none() && wrap && ed.config().toggles.contains(Toggles::ISEARCH_WRAP) {
        let other_end = if s.direction == Direction::Forward {
            0
        } else {
            ed.buffer().text().total_bytes()
        };
        found = find_in_direction(&ed, &pattern, s.direction, other_end, fold);
        wrapped = found.is_some();
    }
    s.found = found;
    let ed = if let Some((start, end)) = found {
        s.anchor = start;
        ed.move_point(if s.direction == Direction::Forward {
            end
        } else {
            start
        })
    } else {
        ed
    };
    let failing = if s.failed() { "Failing " } else { "" };
    let wrap_note = if wrapped { "Wrapped " } else { "" };
    let ed = ed.set_message(format!("{wrap_note}{failing}I-search: {}", s.text));
    (ed, s)
}

fn find_in_direction(
    ed: &Editor,
    pattern: &Pattern,
    direction: Direction,
    from: usize,
    fold: bool,
) -> Option<(usize, usize)> {
    let text = ed.buffer().text();
    match direction {
        Direction::Forward => search::find_forward(text, pattern, from, fold),
        Direction::Backward => search::find_backward(text, pattern, from, fold),
    }
}

/// Starts a query-replace session from point. Errors on a bad regexp or an empty pattern.
pub fn start_query_replace(
    ed: Editor,
    pattern: &str,
    replacement: &str,
    kind: MatchKind,
) -> (Editor, Result<(), String>) {
    // Checked once here: `view_only` cannot change during a session, and every replacement
    // is a Delete then an Insert, so `replace_match` never sees a refused half-edit (I-06).
    if ed.buffer().view_only() {
        return (ed, Err("Buffer is read-only".to_owned()));
    }
    if pattern.is_empty() {
        return (ed, Err("empty pattern".to_owned()));
    }
    let pattern = match kind {
        MatchKind::Regex => match Pattern::regex(pattern) {
            Ok(p) => p,
            Err(e) => return (ed, Err(e)),
        },
        MatchKind::Literal => Pattern::literal(pattern),
    };
    let fold = ed.config().toggles.contains(Toggles::CASE_FOLD_SEARCH);
    let from = ed.window().point;
    let Some(current) = next_match(&ed, &pattern, fold, from) else {
        return (ed.set_message("No match"), Ok(()));
    };
    let q = QueryReplace {
        pattern,
        replacement: replacement.to_owned(),
        kind,
        fold,
        current,
        count: 0,
    };
    let ed = ed
        .set_mode(Mode::QueryReplace(q))
        .set_message("Query replacing from point (y, n, !, q)");
    (ed, Ok(()))
}

fn next_match(ed: &Editor, pattern: &Pattern, fold: bool, from: usize) -> Option<(usize, usize)> {
    if from > ed.buffer().text().total_bytes() {
        return None;
    }
    search::find_forward(ed.buffer().text(), pattern, from, fold)
}

/// Answers for query-replace: `y`/SPC replace, `n`/DEL skip, `!` replace the rest,
/// `q`/RET/`C-g` stop (A8). The whole session is one undo group.
#[must_use]
pub fn query_key(ed: Editor, key: Key) -> Editor {
    let Mode::QueryReplace(mut q) = ed.mode().clone() else {
        return ed;
    };
    let (start, end) = q.current;
    match answer_char(ed.key_aliases(), key) {
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
                current = next_match(&ed, &q.pattern, q.fold, from);
            }
            finish(ed, &q)
        }
        Some('q' | '\n') => finish(ed, &q),
        None if key == Key::ctrl('g') || key == Key::plain(Code::Esc) => finish(ed, &q),
        _ => ed.set_message("Answer y, n, !, or q"),
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
    let replacement = match q.kind {
        MatchKind::Regex => {
            let mut matched = Vec::with_capacity(end.saturating_sub(start));
            let _ = ed.buffer().text().try_for_each_slice(start..end, |chunk| {
                matched.extend_from_slice(chunk);
                core::ops::ControlFlow::<()>::Continue(())
            });
            search::expand_replacement(&q.replacement, &matched)
        }
        MatchKind::Literal => q.replacement.as_bytes().to_vec(),
    };
    let after = start + replacement.len();
    let (ed, applied) = ed.apply(Edit::replace(
        start,
        end - start,
        crate::weave_tree::Doc::from_bytes(&replacement),
    ));
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
    let text = ed.buffer().text();
    if !matched_empty {
        // An edit can join raw bytes into a unit that straddles `after`.
        text.ceil_boundary(after)
    } else if after >= text.total_bytes() {
        // Past the end: no later match can start, so the search finishes.
        text.total_bytes() + 1
    } else {
        text.next_boundary(after)
    }
}

fn step(ed: Editor, q: QueryReplace, from: usize) -> Editor {
    match next_match(&ed, &q.pattern, q.fold, from) {
        Some(current) => ed.set_mode(Mode::QueryReplace(QueryReplace { current, ..q })),
        None => finish(ed, &q),
    }
}

fn finish(ed: Editor, q: &QueryReplace) -> Editor {
    ed.set_mode(Mode::normal())
        .boundary()
        .set_message(format!("Replaced {} occurrence(s)", q.count))
}

pub(crate) fn query_replace_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        prompt(
            ed,
            "Query replace: ",
            Purpose::QueryFrom {
                kind: MatchKind::Literal,
            },
        ),
        fx,
    )
}

pub(crate) fn query_replace_regexp_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        prompt(
            ed,
            "Query replace regexp: ",
            Purpose::QueryFrom {
                kind: MatchKind::Regex,
            },
        ),
        fx,
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::test_support::{editor, keys, point, run_command, text, typ};

    fn start_query(ed: Editor, pattern: &str, replacement: &str, kind: MatchKind) -> Editor {
        let (ed, result) = start_query_replace(ed, pattern, replacement, kind);
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

    fn with_jamo() -> Editor {
        use crate::keymap::AliasTarget;
        let to = |s| AliasTarget::parse(s).unwrap();
        editor().with_key_aliases(|a| a.alias('ㅜ', to("n")).alias('ㅂ', to("q")))
    }

    #[test]
    fn query_replace_folds_answers_and_esc_stops() {
        for stop in ["ESC", "ㅂ", "Q"] {
            let ed = typ(with_jamo(), "foo foo");
            let ed = keys(ed, "C-a");
            let ed = run_command(ed, |e, fx| {
                (start_query(e, "foo", "qux", MatchKind::Literal), fx)
            });
            let ed = if stop == "ESC" {
                keys(ed, "y ESC")
            } else {
                keys(ed, "y")
            };
            let ed = if stop == "ESC" { ed } else { typ(ed, stop) };
            assert_eq!(text(&ed), "qux foo", "{stop}");
            assert_eq!(ed.mode(), &Mode::normal(), "{stop}");
            assert_eq!(ed.message(), "Replaced 1 occurrence(s)", "{stop}");
        }
        let ed = typ(with_jamo(), "foo foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "foo", "qux", MatchKind::Literal), fx)
        });
        let ed = typ(ed, "Y");
        assert_eq!(text(&ed), "qux foo");
        let ed = typ(ed, "ㅜ");
        assert_eq!(ed.mode(), &Mode::normal());
        assert_eq!(text(&ed), "qux foo");
    }

    #[test]
    fn query_replace_start_says_it_searches_from_point() {
        let ed = typ(editor(), "foo");
        let ed = keys(ed, "C-a");
        let ed = start_query(ed, "foo", "qux", MatchKind::Literal);
        assert_eq!(ed.message(), "Query replacing from point (y, n, !, q)");
    }

    #[test]
    fn query_replace_unknown_key_hints_and_keeps_session() {
        let ed = typ(editor(), "foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "foo", "qux", MatchKind::Literal), fx)
        });
        let ed = typ(ed, "z");
        assert!(matches!(ed.mode(), Mode::QueryReplace(_)));
        assert_eq!(ed.message(), "Answer y, n, !, or q");
        assert_eq!(text(&ed), "foo");
    }

    #[test]
    fn query_replace_non_character_key_hints_and_keeps_session() {
        // I-46: a key without a character (an arrow) is answered too, and does not end the session.
        let ed = typ(editor(), "foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "foo", "qux", MatchKind::Literal), fx)
        });
        let ed = crate::test_support::press(ed, Key::plain(Code::Up));
        assert!(matches!(ed.mode(), Mode::QueryReplace(_)));
        assert_eq!(ed.message(), "Answer y, n, !, or q");
        assert_eq!(text(&ed), "foo");
    }

    #[test]
    fn isearch_esc_cancels_like_ctrl_g() {
        let ed = typ(editor(), "abc abd");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "b");
        assert_eq!(point(&ed), 2);
        let ed = keys(ed, "ESC");
        assert_eq!(point(&ed), 0);
        assert_eq!(ed.mode(), &Mode::normal());
        assert_eq!(ed.message(), "Quit");
    }

    #[test]
    fn isearch_failure_follows_the_match() {
        let ed = typ(editor(), "abc");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "b");
        assert!(matches!(ed.mode(), Mode::Isearch(s) if s.found == Some((1, 2)) && !s.failed()));
        let ed = typ(ed, "z");
        assert!(matches!(ed.mode(), Mode::Isearch(s) if s.found.is_none() && s.failed()));
        assert!(ed.message().starts_with("Failing I-search"));
        let ed = keys(ed, "DEL");
        assert!(matches!(ed.mode(), Mode::Isearch(s) if s.found.is_some() && !s.failed()));
        let ed = keys(ed, "DEL");
        assert!(matches!(ed.mode(), Mode::Isearch(s) if s.found.is_none() && !s.failed()));
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
    fn isearch_wrap_off_fails_at_the_end_instead_of_wrapping() {
        let ed = Editor::new(crate::config::Config {
            toggles: Toggles::DEFAULT.difference(Toggles::ISEARCH_WRAP),
            ..crate::config::Config::default()
        });
        let ed = typ(ed, "abc abd");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "c");
        assert_eq!(point(&ed), 3);
        let ed = keys(ed, "C-s");
        assert_eq!(point(&ed), 3);
        assert!(!ed.message().contains("Wrapped"));
        assert!(ed.message().starts_with("Failing I-search"));
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
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "foo", "qux", MatchKind::Literal), fx)
        });
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
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "foo", "qux", MatchKind::Literal), fx)
        });
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
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "x*", "-", MatchKind::Regex), fx)
        });
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "-a-b-");
    }

    #[test]
    fn bulk_query_replace_keeps_small_inverses_undoable() {
        use crate::config::Config;
        use crate::weave_tree::{Doc, MAX_SLICE_BYTES};

        const MATCHES: usize = 257;
        let mut bytes = vec![b'a'; MATCHES * MAX_SLICE_BYTES];
        for chunk in bytes.as_chunks_mut::<MAX_SLICE_BYTES>().0 {
            chunk[MAX_SLICE_BYTES / 2] = b'X';
        }
        let original = Doc::from_bytes(&bytes);
        let ed = editor()
            .set_config(Config {
                toggles: Toggles::empty(),
                ..Config::default()
            })
            .with_current_buffer(|b| b.with_text(original.clone()));
        let ed = keys(start_query(ed, "X", "Y", MatchKind::Literal), "!");
        assert_eq!(ed.message(), "Replaced 257 occurrence(s)");
        assert_eq!(ed.buffer().text().byte_at(MAX_SLICE_BYTES / 2), Some(b'Y'));
        let ed = keys(ed, "C-/");
        assert_eq!(ed.buffer().text(), &original);
    }

    #[test]
    fn query_replace_continues_after_joining_utf8_bytes() {
        let ed = editor()
            .with_current_buffer(|b| {
                b.with_text(crate::weave_tree::Doc::from_bytes(&[
                    0xC3, b'X', 0xA9, b'X',
                ]))
            })
            .move_point(1);
        let ed = start_query(ed, "X*", "", MatchKind::Regex);
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "é");
        assert_eq!(ed.message(), "Replaced 3 occurrence(s)");
    }

    #[test]
    fn query_replace_regexp_bang_and_ampersand() {
        let ed = typ(editor(), "a1 a2");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "a[0-9]", "<&>", MatchKind::Regex), fx)
        });
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "<a1> <a2>");

        let lit = typ(editor(), "a1 a2");
        let lit = keys(lit, "C-a");
        let lit = run_command(lit, |e, fx| {
            (start_query(e, "a1", "<&>", MatchKind::Literal), fx)
        });
        let lit = keys(lit, "!");
        assert_eq!(text(&lit), "<&> a2");

        let esc = typ(editor(), "a1 a2");
        let esc = keys(esc, "C-a");
        let esc = run_command(esc, |e, fx| {
            (start_query(e, "a[0-9]", "\\&", MatchKind::Regex), fx)
        });
        let esc = keys(esc, "!");
        assert_eq!(text(&esc), "& &");
    }

    #[test]
    fn query_replace_refuses_view_only() {
        let ed = typ(editor(), "foo");
        let ed = ed.with_current_buffer(|b| b.set_view_only(true));
        let (ed, result) = start_query_replace(ed, "foo", "bar", MatchKind::Literal);
        assert_eq!(result, Err("Buffer is read-only".to_owned()));
        assert_eq!(text(&ed), "foo");
        assert_eq!(
            ed.mode(),
            &Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn query_replace_ctrl_g_keeps_one_group() {
        let ed = typ(editor(), "foo foo foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| {
            (start_query(e, "foo", "qux", MatchKind::Literal), fx)
        });
        let ed = keys(ed, "y y C-g");
        assert_eq!(text(&ed), "qux qux foo");
        // Replacements made before C-g stay, and one undo restores them all.
        let ed = keys(ed, "C-/");
        assert_eq!(text(&ed), "foo foo foo");
    }
}
