//! Literal and mini-regex search over a `Doc` (§9). A match never crosses a
//! newline (A11). Literal search is a pattern made only of literal atoms.

use std::ops::{ControlFlow, Range};

use crate::weave_tree::{CHUNK_CAP, Doc, Unit, decode_in_slice, decode_unit_raw, prev_in_slice};

// Edited documents can have slices as small as a byte. Bound the overlap copied at
// each seam by the edit chunk size, not by the maximum size of a loaded slice.
const MAX_LITERAL_OVERLAP_NEEDLE: usize = CHUNK_CAP;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Class {
    Lit(char),
    Any,
    Set { negate: bool, items: Vec<SetItem> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SetItem {
    Char(char),
    Range(char, char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Atom { class: Class, star: bool },
    Bol,
    Eol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    nodes: Vec<Node>,
}

fn unit_char(unit: Unit) -> char {
    match unit {
        Unit::Char(c) => c,
        Unit::Raw(b) => char::from(b),
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn upper(c: char) -> char {
    c.to_uppercase().next().unwrap_or(c)
}

impl SetItem {
    fn contains(&self, c: char) -> bool {
        match *self {
            Self::Char(p) => p == c,
            Self::Range(a, b) => (a..=b).contains(&c),
        }
    }
}

impl Class {
    fn matches(&self, c: char, fold: bool) -> bool {
        if c == '\n' {
            return false;
        }
        match self {
            Self::Lit(p) => *p == c || (fold && lower(*p) == lower(c)),
            Self::Any => true,
            Self::Set { negate, items } => {
                let hit = |x| items.iter().any(|it| it.contains(x));
                let found = hit(c) || (fold && (hit(lower(c)) || hit(upper(c))));
                found != *negate
            }
        }
    }
}

impl Pattern {
    /// Every character is literal.
    #[must_use]
    pub fn literal(s: &str) -> Self {
        Self {
            nodes: s
                .chars()
                .map(|c| Node::Atom {
                    class: Class::Lit(c),
                    star: false,
                })
                .collect(),
        }
    }

    /// Mini regex: `.`, `[...]`, `[^...]`, `*` after an atom, `^`, `$`.
    ///
    /// # Errors
    /// Returns an error message if `s` contains an unclosed character class or invalid syntax.
    pub fn regex(s: &str) -> Result<Self, String> {
        let mut nodes: Vec<Node> = Vec::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '.' => nodes.push(Node::Atom {
                    class: Class::Any,
                    star: false,
                }),
                '^' => nodes.push(Node::Bol),
                '$' => nodes.push(Node::Eol),
                '*' => match nodes.last_mut() {
                    Some(Node::Atom { star, .. }) if !*star => *star = true,
                    _ => return Err("* needs a character before it".to_owned()),
                },
                '[' => {
                    let (class, rest) = parse_set(chars)?;
                    chars = rest;
                    nodes.push(Node::Atom { class, star: false });
                }
                _ => nodes.push(Node::Atom {
                    class: Class::Lit(c),
                    star: false,
                }),
            }
        }
        Ok(Self { nodes })
    }

    /// End of the match that starts at the unit boundary `pos`, if any.
    #[must_use]
    pub fn match_at(&self, text: &Doc, pos: usize, fold: bool) -> Option<usize> {
        if !text.is_boundary(pos) {
            return None;
        }
        run(&self.nodes, text, pos, fold)
    }
}

fn parse_set(
    mut chars: std::iter::Peekable<std::str::Chars>,
) -> Result<(Class, std::iter::Peekable<std::str::Chars>), String> {
    let negate = chars.peek() == Some(&'^');
    if negate {
        chars.next();
    }
    let mut items = Vec::new();
    loop {
        let c = chars.next().ok_or("unclosed [")?;
        if c == ']' && !items.is_empty() {
            break;
        }
        if chars.peek() == Some(&'-')
            && let Some(end) = chars.clone().nth(1).filter(|&e| e != ']')
        {
            chars.next();
            chars.next();
            items.push(SetItem::Range(c, end));
            continue;
        }
        items.push(SetItem::Char(c));
    }
    Ok((Class::Set { negate, items }, chars))
}

/// Backtracking match of `nodes` at `pos`. Returns the end of the match.
fn run(nodes: &[Node], text: &Doc, pos: usize, fold: bool) -> Option<usize> {
    let Some((node, rest)) = nodes.split_first() else {
        return Some(pos);
    };
    match node {
        Node::Bol => {
            let at_bol = pos == 0 || text.byte_at(pos - 1) == Some(b'\n');
            at_bol.then(|| run(rest, text, pos, fold)).flatten()
        }
        Node::Eol => {
            let at_eol = pos == text.total_bytes() || text.byte_at(pos) == Some(b'\n');
            at_eol.then(|| run(rest, text, pos, fold)).flatten()
        }
        Node::Atom { class, star: false } => {
            let (unit, n) = text.decode_at(pos)?;
            class
                .matches(unit_char(unit), fold)
                .then(|| run(rest, text, pos + n, fold))
                .flatten()
        }
        Node::Atom { class, star: true } => {
            // Scan once, then walk unit boundaries back only as far as needed.
            let mut end = pos;
            let _ = text
                .try_for_each_slice(pos..text.total_bytes(), |chunk| {
                    let mut i = 0;
                    while let Some((unit, n)) = decode_in_slice(chunk, i) {
                        if !class.matches(unit_char(unit), fold) {
                            return ControlFlow::Break(());
                        }
                        i += n;
                        end += n;
                    }
                    ControlFlow::Continue(())
                })
                .expect("regex starts at a unit boundary");
            loop {
                if let Some(hit) = run(rest, text, end, fold) {
                    return Some(hit);
                }
                if end == pos {
                    return None;
                }
                end = text.prev_boundary(end);
            }
        }
    }
}

/// Returns exact UTF-8 needle bytes when `pat` is a non-empty literal pattern that can be
/// matched directly over `Doc` bytes (Latin-1 non-ASCII atoms use the unit matcher).
/// Returns `Some(Vec::new())` if the pattern contains `\n` (which can never match).
fn exact_byte_needle(pat: &Pattern) -> Option<Vec<u8>> {
    if pat.nodes.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(pat.nodes.len());
    for node in &pat.nodes {
        let Node::Atom {
            class: Class::Lit(c),
            star: false,
        } = *node
        else {
            return None;
        };
        if c == '\n' {
            return Some(Vec::new());
        }
        if !c.is_ascii() && c <= '\u{FF}' {
            return None;
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}

/// Knuth–Morris–Pratt for long needles: retain the matched prefix length across slices,
/// rather than copying and rescanning a needle-sized overlap at each seam.
fn find_long_literal(
    text: &Doc,
    needle: &[u8],
    range: Range<usize>,
    from: usize,
    reverse: bool,
) -> Option<(usize, usize)> {
    let len = needle.len();
    let byte = |i| needle[if reverse { len - 1 - i } else { i }];
    let mut prefix = vec![0; len];
    let mut matched = 0;
    for i in 1..len {
        while matched > 0 && byte(i) != byte(matched) {
            matched = prefix[matched - 1];
        }
        matched += usize::from(byte(i) == byte(matched));
        prefix[i] = matched;
    }

    let mut matched = 0;
    let mut cursor = if reverse { range.end } else { range.start };
    let scan = |chunk: &[u8]| {
        let start = if reverse {
            cursor - chunk.len()
        } else {
            cursor
        };
        cursor = if reverse { start } else { start + chunk.len() };
        let skip = if matched != 0 {
            0
        } else if reverse {
            memchr::memrchr(byte(0), chunk).map_or(chunk.len(), |i| chunk.len() - 1 - i)
        } else {
            memchr::memchr(byte(0), chunk).unwrap_or(chunk.len())
        };
        for i in skip..chunk.len() {
            let offset = if reverse { chunk.len() - 1 - i } else { i };
            let b = chunk[offset];
            while matched > 0 && b != byte(matched) {
                matched = prefix[matched - 1];
            }
            matched += usize::from(b == byte(matched));
            if matched == len {
                let pos = start + offset;
                let pos = if reverse { pos } else { pos + 1 - len };
                if (reverse && pos < from) || (!reverse && pos >= from) {
                    return ControlFlow::Break((pos, pos + len));
                }
                matched = prefix[len - 1];
            }
        }
        ControlFlow::Continue(())
    };
    let flow = if reverse {
        text.try_for_each_slice_rev(range, scan)
    } else {
        text.try_for_each_slice(range, scan)
    };
    match flow.ok()? {
        ControlFlow::Break(hit) => Some(hit),
        ControlFlow::Continue(()) => None,
    }
}

fn find_literal_forward(text: &Doc, needle: &[u8], from: usize) -> Option<(usize, usize)> {
    let total = text.total_bytes();
    if needle.is_empty() || from >= total {
        return None;
    }
    let start = text.ceil_boundary(from);
    if needle.len() > total - start {
        return None;
    }
    if needle.len() > MAX_LITERAL_OVERLAP_NEEDLE {
        return find_long_literal(text, needle, start..total, from, false);
    }
    let finder = memchr::memmem::Finder::new(needle);
    let tail_cap = needle.len() - 1;
    let mut chunk_start = start;
    let mut overlap: Vec<u8> = Vec::with_capacity(tail_cap * 2);

    let flow = text
        .try_for_each_slice(start..total, |chunk| {
            if tail_cap > 0 && !overlap.is_empty() {
                let keep = overlap.len();
                let take = chunk.len().min(tail_cap);
                overlap.extend_from_slice(&chunk[..take]);
                if let Some(off) = finder.find(&overlap) {
                    let pos = chunk_start - keep + off;
                    return ControlFlow::Break((pos, pos + needle.len()));
                }
                overlap.truncate(keep);
            }

            if let Some(off) = finder.find(chunk) {
                let pos = chunk_start + off;
                return ControlFlow::Break((pos, pos + needle.len()));
            }

            if tail_cap > 0 {
                if chunk.len() >= tail_cap {
                    overlap.clear();
                    overlap.extend_from_slice(&chunk[chunk.len() - tail_cap..]);
                } else {
                    let combined = overlap.len() + chunk.len();
                    if combined > tail_cap {
                        overlap.drain(..combined - tail_cap);
                    }
                    overlap.extend_from_slice(chunk);
                }
            }
            chunk_start += chunk.len();
            ControlFlow::Continue(())
        })
        .ok()?;

    match flow {
        ControlFlow::Break(hit) => Some(hit),
        ControlFlow::Continue(()) => None,
    }
}

fn find_literal_backward(text: &Doc, needle: &[u8], from: usize) -> Option<(usize, usize)> {
    let total = text.total_bytes();
    let from = from.min(total);
    if needle.is_empty() || needle.len() > total || from == 0 {
        return None;
    }
    let tail_cap = needle.len() - 1;
    let scan_end = text.ceil_boundary(from.saturating_add(tail_cap).min(total));
    if needle.len() > MAX_LITERAL_OVERLAP_NEEDLE {
        return find_long_literal(text, needle, 0..scan_end, from, true);
    }
    let finder = memchr::memmem::FinderRev::new(needle);
    let mut chunk_end = scan_end;
    let mut right_overlap = Vec::with_capacity(tail_cap);
    let mut window = Vec::with_capacity(tail_cap * 2);

    let flow = text
        .try_for_each_slice_rev(0..scan_end, |chunk| {
            let chunk_start = chunk_end - chunk.len();

            if tail_cap > 0 && !right_overlap.is_empty() && chunk_start < from {
                let take_left = chunk.len().min(tail_cap);
                let win_start = chunk_end - take_left;
                window.clear();
                window.extend_from_slice(&chunk[chunk.len() - take_left..]);
                window.extend_from_slice(&right_overlap);
                if win_start < from {
                    let limit = ((from - win_start - 1) + needle.len()).min(window.len());
                    if let Some(off) = finder.rfind(&window[..limit]) {
                        let pos = win_start + off;
                        if pos < from {
                            return ControlFlow::Break((pos, pos + needle.len()));
                        }
                    }
                }
            }

            if chunk_start < from {
                let limit = ((from - chunk_start - 1) + needle.len()).min(chunk.len());
                if let Some(off) = finder.rfind(&chunk[..limit]) {
                    let pos = chunk_start + off;
                    if pos < from {
                        return ControlFlow::Break((pos, pos + needle.len()));
                    }
                }
            }

            if tail_cap > 0 {
                if chunk.len() >= tail_cap {
                    right_overlap.clear();
                    right_overlap.extend_from_slice(&chunk[..tail_cap]);
                } else {
                    let take = right_overlap.len().min(tail_cap - chunk.len());
                    right_overlap.resize(chunk.len() + take, 0);
                    right_overlap.copy_within(..take, chunk.len());
                    right_overlap[..chunk.len()].copy_from_slice(chunk);
                }
            }
            chunk_end = chunk_start;
            ControlFlow::Continue(())
        })
        .ok()?;

    match flow {
        ControlFlow::Break(hit) => Some(hit),
        ControlFlow::Continue(()) => None,
    }
}

fn find_bol_forward(text: &Doc, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
    if from == 0
        && let Some(end) = pat.match_at(text, 0, fold)
    {
        return Some((0, end));
    }
    let total = text.total_bytes();
    let nl_from = text.floor_boundary(from.saturating_sub(1));
    let mut chunk_start = nl_from;
    let flow = text
        .try_for_each_slice(nl_from..total, |chunk| {
            for off in memchr::memchr_iter(b'\n', chunk) {
                let cand = chunk_start + off + 1;
                if cand >= from
                    && let Some(end) = pat.match_at(text, cand, fold)
                {
                    return ControlFlow::Break((cand, end));
                }
            }
            chunk_start += chunk.len();
            ControlFlow::Continue(())
        })
        .ok()?;
    match flow {
        ControlFlow::Break(hit) => Some(hit),
        ControlFlow::Continue(()) => None,
    }
}

fn find_bol_backward(text: &Doc, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
    if from == 0 {
        return None;
    }
    // A newline at `from - 1` would start a match at `from`, so exclude it.
    let scan_end = text.floor_boundary(from - 1);
    let mut chunk_start = scan_end;
    let flow = text
        .try_for_each_slice_rev(0..scan_end, |chunk| {
            chunk_start -= chunk.len();
            for off in memchr::memrchr_iter(b'\n', chunk) {
                let cand = chunk_start + off + 1;
                if let Some(end) = pat.match_at(text, cand, fold) {
                    return ControlFlow::Break((cand, end));
                }
            }
            ControlFlow::Continue(())
        })
        .expect("scan_end is a unit boundary");
    match flow {
        ControlFlow::Break(hit) => Some(hit),
        ControlFlow::Continue(()) => pat.match_at(text, 0, fold).map(|end| (0, end)),
    }
}

/// First match starting at or after `from`. Returns `(start, end)`.
#[must_use]
pub fn find_forward(text: &Doc, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
    if from > text.total_bytes() {
        return None;
    }
    if !fold && let Some(needle) = exact_byte_needle(pat) {
        return find_literal_forward(text, &needle, from);
    }
    if matches!(pat.nodes.first(), Some(Node::Bol)) {
        return find_bol_forward(text, pat, from, fold);
    }
    let total = text.total_bytes();
    find_by_unit(text, pat, text.ceil_boundary(from)..total, fold, false)
        .or_else(|| pat.match_at(text, total, fold).map(|end| (total, end)))
}

/// Last match starting before `from`. Returns `(start, end)`.
#[must_use]
pub fn find_backward(text: &Doc, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
    let from = from.min(text.total_bytes());
    if !fold && let Some(needle) = exact_byte_needle(pat) {
        return find_literal_backward(text, &needle, from);
    }
    if matches!(pat.nodes.first(), Some(Node::Bol)) {
        return find_bol_backward(text, pat, from, fold);
    }
    // If `from` is inside a unit, that unit's start is still a valid candidate.
    find_by_unit(text, pat, 0..text.ceil_boundary(from), fold, true)
}

fn find_by_unit(
    text: &Doc,
    pat: &Pattern,
    range: Range<usize>,
    fold: bool,
    reverse: bool,
) -> Option<(usize, usize)> {
    let mut cursor = if reverse { range.end } else { range.start };
    let scan = |chunk: &[u8]| {
        let base = if reverse {
            cursor - chunk.len()
        } else {
            cursor
        };
        cursor = if reverse { base } else { base + chunk.len() };
        let mut i = if reverse { chunk.len() } else { 0 };
        while if reverse { i > 0 } else { i < chunk.len() } {
            if reverse {
                i = prev_in_slice(chunk, i);
            }
            let (unit, len) = decode_unit_raw(chunk, i).expect("candidate is within slice");
            let pos = base + i;
            let end = match pat.nodes.split_first() {
                Some((Node::Atom { class, star: false }, rest)) => class
                    .matches(unit_char(unit), fold)
                    .then(|| run(rest, text, pos + len, fold))
                    .flatten(),
                _ => run(&pat.nodes, text, pos, fold),
            };
            if let Some(end) = end {
                return ControlFlow::Break((pos, end));
            }
            if !reverse {
                i += len;
            }
        }
        ControlFlow::Continue(())
    };
    let flow = if reverse {
        text.try_for_each_slice_rev(range, scan)
    } else {
        text.try_for_each_slice(range, scan)
    };
    match flow.expect("search range is aligned") {
        ControlFlow::Break(hit) => Some(hit),
        ControlFlow::Continue(()) => None,
    }
}

/// Replacement text for a regex match: `&` is the matched text, `\&` is a literal `&`.
#[must_use]
pub fn expand_replacement(replacement: &str, matched: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'&') => {
                chars.next();
                out.push(b'&');
            }
            '&' => out.extend_from_slice(matched),
            _ => out.extend_from_slice(c.to_string().as_bytes()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weave_tree::MAX_SLICE_BYTES;

    fn text(s: &str) -> Doc {
        Doc::from_bytes(s.as_bytes())
    }

    #[test]
    fn literal_fold() {
        let t = text("Hello hello");
        let pat = Pattern::literal("hello");
        assert_eq!(find_forward(&t, &pat, 0, true), Some((0, 5)));
        assert_eq!(find_forward(&t, &pat, 1, true), Some((6, 11)));
        assert_eq!(find_forward(&t, &pat, 0, false), Some((6, 11)));
        assert_eq!(find_backward(&t, &pat, 11, false), Some((6, 11)));
        assert_eq!(find_backward(&t, &pat, 6, true), Some((0, 5)));
        assert_eq!(find_forward(&t, &Pattern::literal("zzz"), 0, true), None);
    }

    #[test]
    fn streamed_unit_candidates_match_reference_across_slices() {
        use crate::weave_tree::DocBuilder;

        let doc = DocBuilder::new()
            .push_bytes(b"B\n\xC9")
            .push_bytes("É".as_bytes())
            .push_bytes(b"Ba\n")
            .finish();
        let total = doc.total_bytes();
        for query in ["", "éé", "éB", "[^a]a", "b*", "^b", "$", ".$"] {
            let pat = Pattern::regex(query).unwrap();
            for from in 0..=total {
                let hit = |pos| pat.match_at(&doc, pos, true).map(|end| (pos, end));
                assert_eq!(
                    find_forward(&doc, &pat, from, true),
                    (from..=total).find_map(hit),
                    "{query:?} from {from}"
                );
                assert_eq!(
                    find_backward(&doc, &pat, from, true),
                    (0..from).rev().find_map(hit),
                    "{query:?} from {from}"
                );
            }
        }
    }

    #[test]
    fn regex_dot_class_star_anchors() {
        let t = text("abc\nxyz");
        let m = |re: &str| {
            let pat = Pattern::regex(re).unwrap();
            find_forward(&t, &pat, 0, false)
        };
        assert_eq!(m("a.c"), Some((0, 3)));
        assert_eq!(m("[a-c]*"), Some((0, 3)));
        assert_eq!(m("[^a]"), Some((1, 2)));
        assert_eq!(m("^x"), Some((4, 5)));
        assert_eq!(m("c$"), Some((2, 3)));
        // `.` never crosses the newline.
        assert_eq!(m("c.x"), None);
        assert_eq!(m("ab*c"), Some((0, 3)));
    }

    #[test]
    fn regex_star_backtracks_greedily_across_slices_on_unit_boundaries() {
        let prefix = "a".repeat(MAX_SLICE_BYTES - 2);
        let line = format!("{prefix}éXéXéé\nX");
        let doc = text(&line);
        let pat = Pattern::regex("^.*X").unwrap();
        assert_eq!(pat.match_at(&doc, 0, false), Some(prefix.len() + 6));
        let pat = Pattern::regex("^.*aé").unwrap();
        assert_eq!(pat.match_at(&doc, 0, false), Some(prefix.len() + 2));
        let pat = Pattern::regex("^.*XX").unwrap();
        assert_eq!(pat.match_at(&doc, 0, false), None);
    }

    #[test]
    fn backward_bol_search_across_dense_newlines() {
        let padding = "\n".repeat(2 * MAX_SLICE_BYTES);
        let doc = text(&format!("é{padding}é{padding}"));
        let second = 2 + padding.len();
        let pat = Pattern::regex("^é").unwrap();
        assert_eq!(
            find_backward(&doc, &pat, doc.total_bytes(), false),
            Some((second, second + 2))
        );
        assert_eq!(
            find_backward(&doc, &pat, second + 1, false),
            Some((second, second + 2))
        );
        assert_eq!(find_backward(&doc, &pat, second, false), Some((0, 2)));
        assert_eq!(find_backward(&doc, &pat, 0, false), None);
        let absent = Pattern::regex("^x").unwrap();
        assert_eq!(find_backward(&doc, &absent, doc.total_bytes(), false), None);
    }

    #[test]
    fn empty_matches_require_a_unit_boundary() {
        let doc = text("é");
        let pat = Pattern::regex("x*").unwrap();
        assert_eq!(pat.match_at(&doc, 1, false), None);
        assert_eq!(find_forward(&doc, &pat, 1, false), Some((2, 2)));
    }

    #[test]
    fn regex_fold_applies_to_classes() {
        let t = text("ABC");
        let pat = Pattern::regex("[a-b]*").unwrap();
        assert_eq!(find_forward(&t, &pat, 0, true), Some((0, 2)));
        assert_eq!(find_forward(&t, &pat, 0, false), Some((0, 0)));
    }

    #[test]
    fn regex_errors() {
        assert!(Pattern::regex("[ab").is_err());
        assert!(Pattern::regex("*a").is_err());
    }

    #[test]
    fn replacement_expands_ampersand() {
        assert_eq!(expand_replacement("[&]", b"xy"), b"[xy]");
        assert_eq!(expand_replacement("\\&", b"x"), b"&");
    }

    #[test]
    fn literal_search_preserves_decoded_units_at_every_gap_position() {
        let cases: &[(&[u8], &str)] = &[
            (b"/", "/"),
            ("Ā".as_bytes(), "Ā"),
            ("/Ā".as_bytes(), "/Ā"),
            ("😀".as_bytes(), "😀"),
        ];
        for &(bytes, needle) in cases {
            for gap in 0..=bytes.len() {
                let doc = Doc::from_bytes(bytes);
                if !doc.is_boundary(gap) {
                    continue;
                }
                let t = doc
                    .replace(gap, 0, b"X")
                    .unwrap()
                    .0
                    .replace(gap, 1, b"")
                    .unwrap()
                    .0;
                for pat in [Pattern::literal(needle), Pattern::regex(needle).unwrap()] {
                    for fold in [false, true] {
                        assert_eq!(pat.match_at(&t, 0, fold), Some(bytes.len()));
                        for from in 0..=bytes.len() + 1 {
                            let forward = (from == 0).then_some((0, bytes.len()));
                            let backward = (from > 0).then_some((0, bytes.len()));
                            assert_eq!(
                                find_forward(&t, &pat, from, fold),
                                forward,
                                "forward: bytes={bytes:02x?}, gap={gap}, from={from}, fold={fold}"
                            );
                            assert_eq!(
                                find_backward(&t, &pat, from, fold),
                                backward,
                                "backward: bytes={bytes:02x?}, gap={gap}, from={from}, fold={fold}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn literal_search_matches_unit_search_in_mixed_encodings() {
        for (encoded, needle, unit_pattern) in [
            (&[0xC0, 0xAF][..], "/", "[/]"),
            (&[0xF0, 0x80, 0x84, 0x80][..], "Ā", "[Ā]"),
        ] {
            let bytes = [needle.as_bytes(), encoded, needle.as_bytes()].concat();
            let pat = Pattern::literal(needle);
            let unit_pat = Pattern::regex(unit_pattern).unwrap();
            for gap in 0..=bytes.len() {
                let doc = Doc::from_bytes(&bytes);
                if !doc.is_boundary(gap) {
                    continue;
                }
                let t = doc
                    .replace(gap, 0, b"X")
                    .unwrap()
                    .0
                    .replace(gap, 1, b"")
                    .unwrap()
                    .0;
                for from in 0..=bytes.len() + 1 {
                    assert_eq!(
                        find_forward(&t, &pat, from, false),
                        find_forward(&t, &unit_pat, from, false),
                        "forward: needle={needle}, gap={gap}, from={from}"
                    );
                    assert_eq!(
                        find_backward(&t, &pat, from, false),
                        find_backward(&t, &unit_pat, from, false),
                        "backward: needle={needle}, gap={gap}, from={from}"
                    );
                }
            }
        }
    }

    #[test]
    fn backward_literal_carries_overlap_across_tiny_slices() {
        use crate::weave_tree::DocBuilder;

        let bytes = "zabcdefabcdef!";
        let doc = [&b"za"[..], b"b", b"cde", b"fa", b"bc", b"d", b"ef!"]
            .into_iter()
            .fold(DocBuilder::new(), DocBuilder::push_bytes)
            .finish();
        let needle = "abcdef";
        let pattern = Pattern::literal(needle);
        for from in 0..=bytes.len() {
            let expected = bytes
                .match_indices(needle)
                .filter(|&(start, _)| start < from)
                .last()
                .map(|(start, hit)| (start, start + hit.len()));
            assert_eq!(find_backward(&doc, &pattern, from, false), expected);
        }
    }

    #[test]
    fn medium_literal_search_on_fragmented_text() {
        use crate::weave_tree::DocBuilder;

        let block = [b'a'; 64];
        let builder = (0..1024).fold(DocBuilder::new(), |b, _| b.push_bytes(&block));
        let doc = builder.push_bytes(b"b").finish();
        let total = doc.total_bytes();
        let run = "a".repeat(MAX_LITERAL_OVERLAP_NEEDLE);
        let absent = Pattern::literal(&format!("{run}c"));
        assert_eq!(find_forward(&doc, &absent, 0, false), None);
        assert_eq!(find_backward(&doc, &absent, total, false), None);

        let present = Pattern::literal(&format!("{run}b"));
        let start = total - run.len() - 1;
        assert_eq!(find_forward(&doc, &present, 0, false), Some((start, total)));
        assert_eq!(
            find_backward(&doc, &present, start + 1, false),
            Some((start, total))
        );
        assert_eq!(find_backward(&doc, &present, start, false), None);
    }

    #[test]
    fn long_literal_crosses_slices_in_both_directions() {
        let run = "a".repeat(2 * MAX_SLICE_BYTES);
        let needle = format!("b{run}b");
        let doc = text(&format!("aa{needle}{run}b"));
        let first = 2;
        let second = first + needle.len() - 1;
        let pat = Pattern::literal(&needle);
        assert_eq!(
            find_forward(&doc, &pat, 0, false),
            Some((first, second + 1))
        );
        assert_eq!(
            find_forward(&doc, &pat, first + 1, false),
            Some((second, doc.total_bytes()))
        );
        assert_eq!(
            find_backward(&doc, &pat, second + 1, false),
            Some((second, doc.total_bytes()))
        );
        assert_eq!(
            find_backward(&doc, &pat, second, false),
            Some((first, second + 1))
        );
        assert_eq!(find_backward(&doc, &pat, first, false), None);
    }

    #[test]
    fn literal_and_bol_search_across_gap_and_edge_cases() {
        // Insert in the middle so a tree seam splits "needle" right across slices.
        let t = Doc::from_bytes(b"aaaneedlebbb\n^line2\nneedleccc");
        let t = t
            .replace(3, 0, b"X")
            .unwrap()
            .0
            .replace(3, 1, b"")
            .unwrap()
            .0;
        let t = t
            .replace(6, 0, b"Y")
            .unwrap()
            .0
            .replace(6, 1, b"")
            .unwrap()
            .0;
        let pat = Pattern::literal("needle");
        assert_eq!(find_forward(&t, &pat, 0, false), Some((3, 9)));
        assert_eq!(find_forward(&t, &pat, 4, false), Some((20, 26)));
        assert_eq!(
            find_backward(&t, &pat, t.total_bytes(), false),
            Some((20, 26))
        );
        // `from` inside the gap-crossing match still finds it backward.
        assert_eq!(find_backward(&t, &pat, 7, false), Some((3, 9)));
        assert_eq!(find_backward(&t, &pat, 4, false), Some((3, 9)));
        assert_eq!(find_backward(&t, &pat, 3, false), None);

        // Newline inside literal never matches.
        assert_eq!(
            find_forward(&t, &Pattern::literal("bbb\n^line2"), 0, false),
            None
        );

        // `^`-anchored search across gap.
        let bol_pat = Pattern::regex("^needle").unwrap();
        assert_eq!(find_forward(&t, &bol_pat, 0, false), Some((20, 26)));
        assert_eq!(
            find_backward(&t, &bol_pat, t.total_bytes(), false),
            Some((20, 26))
        );
        assert_eq!(find_backward(&t, &bol_pat, 21, false), Some((20, 26)));
        assert_eq!(find_backward(&t, &bol_pat, 20, false), None);
    }

    #[test]
    fn literal_forward_from_mid_char_finds_overlapping_seam_match() {
        let pad = "x".repeat(2048);
        let left = format!("{pad}가가");
        let right = format!("가가{pad}");
        let seam = left.len();
        let doc = Doc::from_bytes(left.as_bytes())
            .replace(seam, 0, right.as_bytes())
            .unwrap()
            .0;
        let mut slice_ends = Vec::new();
        let mut acc = 0;
        let _ = doc.try_for_each_slice(0..doc.total_bytes(), |s| {
            acc += s.len();
            slice_ends.push(acc);
            ControlFlow::<()>::Continue(())
        });
        assert!(slice_ends.contains(&seam), "expected slice seam at {seam}");

        let base = pad.len();
        let pat = Pattern::literal("가가가");
        // Mid-char `from = base + 1` must skip the match at `base` and still find `base + 3`.
        assert_eq!(
            find_forward(&doc, &pat, base, false),
            Some((base, base + 9))
        );
        assert_eq!(
            find_forward(&doc, &pat, base + 1, false),
            Some((base + 3, base + 12))
        );
        assert_eq!(find_forward(&doc, &pat, base + 4, false), None);
        assert_eq!(
            find_backward(&doc, &pat, base + 4, false),
            Some((base + 3, base + 12))
        );
        assert_eq!(
            find_backward(&doc, &pat, base + 3, false),
            Some((base, base + 9))
        );
    }

    #[test]
    fn literal_search_with_raw_bytes_matches_unit_search() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xFF, b'a', 0x80, 0xC0, 0xAF, b'/']);
        bytes.extend_from_slice("Ā".as_bytes());
        bytes.extend_from_slice(&[0xFE, b'a', 0xC4, b'x']);
        let doc = Doc::from_bytes(&bytes);
        for (needle, unit_re) in [("a", "[a]"), ("/", "[/]"), ("Ā", "[Ā]")] {
            let pat = Pattern::literal(needle);
            let unit_pat = Pattern::regex(unit_re).unwrap();
            for from in 0..=bytes.len() + 1 {
                assert_eq!(
                    find_forward(&doc, &pat, from, false),
                    find_forward(&doc, &unit_pat, from, false),
                    "forward needle={needle}, from={from}"
                );
                assert_eq!(
                    find_backward(&doc, &pat, from, false),
                    find_backward(&doc, &unit_pat, from, false),
                    "backward needle={needle}, from={from}"
                );
            }
        }
        assert_eq!(find_forward(&doc, &Pattern::literal("a\n"), 0, false), None);
        assert_eq!(
            find_backward(&doc, &Pattern::literal("a\n"), bytes.len(), false),
            None
        );
    }
}
