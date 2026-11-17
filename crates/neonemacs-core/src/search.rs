//! Literal and mini-regex search over a `Text` (§9). A match never crosses a
//! newline (A11). Literal search is a pattern made only of literal atoms.

use crate::text::{Text, Unit};

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

    /// End of the match that starts at `pos`, if any.
    #[must_use]
    pub fn match_at(&self, text: &Text, pos: usize, fold: bool) -> Option<usize> {
        if pos > text.len() {
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
fn run(nodes: &[Node], text: &Text, pos: usize, fold: bool) -> Option<usize> {
    let Some((node, rest)) = nodes.split_first() else {
        return Some(pos);
    };
    match node {
        Node::Bol => {
            let at_bol = pos == 0 || text.byte_at(pos - 1) == b'\n';
            at_bol.then(|| run(rest, text, pos, fold)).flatten()
        }
        Node::Eol => {
            let at_eol = pos == text.len() || text.byte_at(pos) == b'\n';
            at_eol.then(|| run(rest, text, pos, fold)).flatten()
        }
        Node::Atom { class, star: false } => {
            let (unit, n) = text.unit_at(pos)?;
            class
                .matches(unit_char(unit), fold)
                .then(|| run(rest, text, pos + n, fold))
                .flatten()
        }
        Node::Atom { class, star: true } => {
            // Greedy: remember every end position, then try the longest first.
            let mut ends = vec![pos];
            let mut p = pos;
            while let Some((unit, n)) = text.unit_at(p) {
                if !class.matches(unit_char(unit), fold) {
                    break;
                }
                p += n;
                ends.push(p);
            }
            ends.iter().rev().find_map(|&q| run(rest, text, q, fold))
        }
    }
}

/// Returns exact UTF-8 needle bytes when `pat` is a non-empty literal pattern that can be
/// matched directly over canonical UTF-8 (Latin-1 atoms use the unit matcher).
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

/// Text also decodes overlong sequences, which have no canonical byte match.
/// A range or gap splitting a UTF-8 unit conservatively falls back to unit matching.
fn is_canonical_utf8(text: &Text, range: std::ops::Range<usize>) -> bool {
    let (left, right) = text.slices_in(range);
    std::str::from_utf8(left).is_ok() && std::str::from_utf8(right).is_ok()
}

fn gap_match_at(left: &[u8], right: &[u8], pos: usize, needle: &[u8]) -> bool {
    let left_len = left.len();
    if pos + needle.len() > left_len + right.len() {
        return false;
    }
    let from_left = left_len.saturating_sub(pos);
    left[pos..] == needle[..from_left] && right[..needle.len() - from_left] == needle[from_left..]
}

fn find_literal_forward(text: &Text, needle: &[u8], from: usize) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let (left, right) = text.slices();
    let left_len = left.len();
    let finder = memchr::memmem::Finder::new(needle);

    if from < left_len {
        if let Some(off) = finder.find(&left[from..]) {
            let start = from + off;
            return Some((start, start + needle.len()));
        }
        let cross_start = left_len.saturating_sub(needle.len() - 1).max(from);
        for p in cross_start..left_len {
            if gap_match_at(left, right, p, needle) {
                return Some((p, p + needle.len()));
            }
        }
        if let Some(off) = finder.find(right) {
            let start = left_len + off;
            return Some((start, start + needle.len()));
        }
        None
    } else {
        let r_from = from - left_len;
        finder.find(&right[r_from..]).map(|off| {
            let start = from + off;
            (start, start + needle.len())
        })
    }
}

fn find_literal_backward(text: &Text, needle: &[u8], from: usize) -> Option<(usize, usize)> {
    if needle.is_empty() || from == 0 {
        return None;
    }
    let (left, right) = text.slices();
    let left_len = left.len();
    let finder = memchr::memmem::FinderRev::new(needle);

    if from > left_len {
        let r_limit = ((from - left_len - 1) + needle.len()).min(right.len());
        if let Some(off) = finder.rfind(&right[..r_limit]) {
            let start = left_len + off;
            if start < from {
                return Some((start, start + needle.len()));
            }
        }
        let cross_start = left_len.saturating_sub(needle.len() - 1);
        for p in (cross_start..left_len).rev() {
            if gap_match_at(left, right, p, needle) {
                return Some((p, p + needle.len()));
            }
        }
        finder
            .rfind(left)
            .map(|start| (start, start + needle.len()))
    } else {
        let cross_start = left_len.saturating_sub(needle.len() - 1);
        for p in (cross_start..from).rev() {
            if gap_match_at(left, right, p, needle) {
                return Some((p, p + needle.len()));
            }
        }
        let l_limit = ((from - 1) + needle.len()).min(left_len);
        finder
            .rfind(&left[..l_limit])
            .filter(|&start| start < from)
            .map(|start| (start, start + needle.len()))
    }
}

fn find_bol_forward(text: &Text, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
    if from == 0
        && let Some(end) = pat.match_at(text, 0, fold)
    {
        return Some((0, end));
    }
    let (left, right) = text.slices();
    let left_len = left.len();
    let nl_from = from.saturating_sub(1);

    if nl_from < left_len {
        for off in memchr::memchr_iter(b'\n', &left[nl_from..]) {
            let cand = nl_from + off + 1;
            if cand >= from
                && let Some(end) = pat.match_at(text, cand, fold)
            {
                return Some((cand, end));
            }
        }
        for off in memchr::memchr_iter(b'\n', right) {
            let cand = left_len + off + 1;
            if let Some(end) = pat.match_at(text, cand, fold) {
                return Some((cand, end));
            }
        }
    } else {
        let r_from = nl_from - left_len;
        for off in memchr::memchr_iter(b'\n', &right[r_from..]) {
            let cand = nl_from + off + 1;
            if cand >= from
                && let Some(end) = pat.match_at(text, cand, fold)
            {
                return Some((cand, end));
            }
        }
    }
    None
}

fn find_bol_backward(
    text: &Text,
    pat: &Pattern,
    from: usize,
    fold: bool,
) -> Option<(usize, usize)> {
    let mut p = from;
    while p > 0 {
        let cand = text.line_start(p - 1);
        if cand < from
            && let Some(end) = pat.match_at(text, cand, fold)
        {
            return Some((cand, end));
        }
        p = cand;
    }
    None
}

/// First match starting at or after `from`. Returns `(start, end)`.
#[must_use]
pub fn find_forward(text: &Text, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
    if from > text.len() {
        return None;
    }
    if !fold && let Some(needle) = exact_byte_needle(pat) {
        let found = find_literal_forward(text, &needle, from);
        let end = found.map_or(text.len(), |(_, end)| end);
        // Validate only the searched span, not the whole buffer on every replacement.
        if is_canonical_utf8(text, from..end) {
            return found;
        }
    }
    if matches!(pat.nodes.first(), Some(Node::Bol)) {
        return find_bol_forward(text, pat, from, fold);
    }
    let mut p = from;
    loop {
        if let Some(end) = pat.match_at(text, p, fold) {
            return Some((p, end));
        }
        if p >= text.len() {
            return None;
        }
        p = text.next_boundary(p);
    }
}

/// Last match starting before `from`. Returns `(start, end)`.
#[must_use]
pub fn find_backward(
    text: &Text,
    pat: &Pattern,
    from: usize,
    fold: bool,
) -> Option<(usize, usize)> {
    let mut p = from.min(text.len());
    if !fold && let Some(needle) = exact_byte_needle(pat) {
        let found = find_literal_backward(text, &needle, p);
        let start = found.map_or(0, |(start, _)| start);
        let end = found.map_or(p, |(_, end)| end.max(p));
        if is_canonical_utf8(text, start..end) {
            return found;
        }
    }
    if matches!(pat.nodes.first(), Some(Node::Bol)) {
        return find_bol_backward(text, pat, p, fold);
    }
    while p > 0 {
        p = text.prev_boundary(p);
        if let Some(end) = pat.match_at(text, p, fold) {
            return Some((p, end));
        }
    }
    None
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

    fn text(s: &str) -> Text {
        Text::from_bytes(s.as_bytes())
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
            (&[0xC0, 0xAF], "/"),
            (&[0xE0, 0x80, 0xAF], "/"),
            (&[0xF0, 0x80, 0x80, 0xAF], "/"),
            ("Ā".as_bytes(), "Ā"),
            (&[0xF0, 0x80, 0x84, 0x80], "Ā"),
            (&[0xC0, 0xAF, 0xF0, 0x80, 0x84, 0x80], "/Ā"),
        ];
        for &(bytes, needle) in cases {
            for gap in 0..=bytes.len() {
                let t = Text::from_bytes(bytes)
                    .insert(gap, b"X")
                    .delete(gap..gap + 1);
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
                let t = Text::from_bytes(&bytes)
                    .insert(gap, b"X")
                    .delete(gap..gap + 1);
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
    fn literal_and_bol_search_across_gap_and_edge_cases() {
        // Insert in the middle so the gap splits "needle" right across left/right slices.
        let t = Text::from_bytes(b"aaaneedlebbb\n^line2\nneedleccc")
            .insert(6, b"") // move gap to 6 ("aaanee" | "dlebbb...")
            ;
        let t = t.insert(3, b"X").delete(3..4); // gap at 3 ("aaa" | "needle...")
        let t = t.insert(6, b"Y").delete(6..7); // gap at 6 ("aaanee" | "dle...")
        let pat = Pattern::literal("needle");
        assert_eq!(find_forward(&t, &pat, 0, false), Some((3, 9)));
        assert_eq!(find_forward(&t, &pat, 4, false), Some((20, 26)));
        assert_eq!(find_backward(&t, &pat, t.len(), false), Some((20, 26)));
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
        assert_eq!(find_backward(&t, &bol_pat, t.len(), false), Some((20, 26)));
        assert_eq!(find_backward(&t, &bol_pat, 21, false), Some((20, 26)));
        assert_eq!(find_backward(&t, &bol_pat, 20, false), None);
    }
}
