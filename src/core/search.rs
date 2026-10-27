//! Literal and mini-regex search over a `Text` (§9). A match never crosses a
//! newline (A11). Literal search is a pattern made only of literal atoms.

use crate::core::text::{Text, Unit};

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
            SetItem::Char(p) => p == c,
            SetItem::Range(a, b) => (a..=b).contains(&c),
        }
    }
}

impl Class {
    fn matches(&self, c: char, fold: bool) -> bool {
        if c == '\n' {
            return false;
        }
        match self {
            Class::Lit(p) => *p == c || (fold && lower(*p) == lower(c)),
            Class::Any => true,
            Class::Set { negate, items } => {
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
    pub fn literal(s: &str) -> Pattern {
        Pattern {
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
    pub fn regex(s: &str) -> Result<Pattern, String> {
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
        Ok(Pattern { nodes })
    }

    /// End of the match that starts at `pos`, if any.
    #[must_use]
    pub fn match_at(&self, text: &Text, pos: usize, fold: bool) -> Option<usize> {
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
        if chars.peek() == Some(&'-') {
            let mut ahead = chars.clone();
            ahead.next();
            if let Some(end) = ahead.next().filter(|&e| e != ']') {
                chars.next();
                chars.next();
                items.push(SetItem::Range(c, end));
                continue;
            }
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

/// First match starting at or after `from`. Returns `(start, end)`.
#[must_use]
pub fn find_forward(text: &Text, pat: &Pattern, from: usize, fold: bool) -> Option<(usize, usize)> {
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
    let mut p = from;
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
}
