//! 1-pass lightweight syntax tokenizer for buffer display (§5, I-22).
//! Pure: no regex, no heap bloat, no external crate dependencies.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenKind {
    #[default]
    Normal,
    Keyword,
    Type,
    String,
    Comment,
    Number,
}

/// Line-boundary lexer state for multi-line constructs (I-51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LexState {
    #[default]
    Normal,
    BlockComment,
    RawString {
        hashes: u8,
    },
}

/// Definition of syntax highlighting rules for a language (I-27, I-49).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntaxDef {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub keywords: Vec<String>,
    pub types: Vec<String>,
    pub case_insensitive: bool,
    /// A line starting with this prefix is a heading, drawn whole as a keyword. A definition with
    /// a heading prefix does no other highlighting (I-49).
    pub heading_prefix: Option<String>,
    /// When true, `'` only highlights single-character literals (`'a'`, `'\n'`) as strings so
    /// lifetimes and labels (`'a`, `'static`) are not tokenized as strings.
    pub single_quote_char_only: bool,
    /// When true, `r#"..."#` raw strings are tokenized across lines.
    pub raw_strings: bool,
}

impl SyntaxDef {
    /// Creates a validated and normalized `SyntaxDef` (I-49, I-50).
    ///
    /// # Panics
    /// Panics if `name` is empty or if both `extensions` and `filenames` are empty.
    pub fn new(
        name: impl Into<String>,
        extensions: Vec<String>,
        filenames: Vec<String>,
        line_comment: Option<String>,
        mut keywords: Vec<String>,
        mut types: Vec<String>,
        case_insensitive: bool,
    ) -> Self {
        let name = name.into();
        assert!(!name.trim().is_empty(), "syntax name must not be empty");
        assert!(
            !extensions.is_empty() || !filenames.is_empty(),
            "syntax spec must have at least one extension or filename (I-49)"
        );

        if case_insensitive {
            for k in &mut keywords {
                k.make_ascii_lowercase();
            }
            for t in &mut types {
                t.make_ascii_lowercase();
            }
        }
        keywords.sort_unstable();
        keywords.dedup();
        types.sort_unstable();
        types.dedup();

        Self {
            name,
            extensions,
            filenames,
            line_comment,
            block_comment: None,
            keywords,
            types,
            case_insensitive,
            heading_prefix: None,
            single_quote_char_only: false,
            raw_strings: false,
        }
    }

    #[must_use]
    pub fn with_block_comment(mut self, open: impl Into<String>, close: impl Into<String>) -> Self {
        self.block_comment = Some((open.into(), close.into()));
        self
    }

    #[must_use]
    pub fn with_raw_strings(mut self, enabled: bool) -> Self {
        self.raw_strings = enabled;
        self
    }

    #[must_use]
    pub fn with_heading_prefix(mut self, prefix: Option<String>) -> Self {
        self.heading_prefix = prefix;
        self
    }

    #[must_use]
    pub fn with_single_quote_char_only(mut self, enabled: bool) -> Self {
        self.single_quote_char_only = enabled;
        self
    }

    fn contains_word(&self, list: &[String], word: &str) -> bool {
        if self.case_insensitive {
            list.binary_search_by(|k| cmp_ignore_ascii_case(k.as_str(), word))
                .is_ok()
        } else {
            list.binary_search_by(|k| k.as_str().cmp(word)).is_ok()
        }
    }

    /// Checks if a word is a keyword using binary search in $O(\log K)$ (I-50).
    #[must_use]
    pub fn is_keyword(&self, word: &str) -> bool {
        self.contains_word(&self.keywords, word)
    }

    /// Checks if a word is a type using binary search in $O(\log T)$ (I-50).
    #[must_use]
    pub fn is_type(&self, word: &str) -> bool {
        self.contains_word(&self.types, word)
    }
}

fn cmp_ignore_ascii_case(a: &str, b: &str) -> core::cmp::Ordering {
    let a_bytes = a.bytes().map(|b| b.to_ascii_lowercase());
    let b_bytes = b.bytes().map(|b| b.to_ascii_lowercase());
    a_bytes.cmp(b_bytes)
}

/// Registry storing syntax definitions for languages (I-27).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntaxRegistry {
    languages: Vec<SyntaxDef>,
}

impl SyntaxRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            languages: Vec::new(),
        }
    }

    #[must_use]
    pub fn register(mut self, def: SyntaxDef) -> Self {
        if let Some(pos) = self.languages.iter().position(|l| l.name == def.name) {
            self.languages[pos] = def;
        } else {
            self.languages.push(def);
        }
        self
    }

    /// Finds the syntax definition matching the file or buffer name.
    /// Priority (I-49): Exact basename match against `filenames` > extension match against `extensions`.
    #[must_use]
    pub fn find_by_filename<'a>(&'a self, name: &str) -> Option<&'a SyntaxDef> {
        let basename = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if basename.is_empty() {
            return None;
        }

        // 1. Exact basename match against filenames
        if let Some(def) = self
            .languages
            .iter()
            .find(|l| l.filenames.iter().any(|f| f == basename))
        {
            return Some(def);
        }

        // 2. Extension match
        let ext = basename.rsplit('.').next().unwrap_or("");
        if ext.is_empty() || ext == basename {
            return None;
        }
        self.languages
            .iter()
            .find(|l| l.extensions.iter().any(|e| e == ext))
    }
}

use core::ops::{ControlFlow, Range};

use crate::Doc;

/// Returns one token per byte of `range` after clamping to the document length.
/// Reads complete units at the range edges without returning their extra tokens.
/// The lexer borrows the input bytes when they reside within a single `ByteSlice`.
#[must_use]
pub fn highlight_line(
    syntax: Option<&SyntaxDef>,
    doc: &Doc,
    range: Range<usize>,
) -> Vec<TokenKind> {
    highlight_line_with_state(syntax, doc, range, LexState::Normal).0
}

/// Pure state-transition line highlighter: tokenizes `range` starting from `start_state`
/// and returns `(tokens, end_state)` (I-51).
#[must_use]
pub fn highlight_line_with_state(
    syntax: Option<&SyntaxDef>,
    doc: &Doc,
    range: Range<usize>,
    start_state: LexState,
) -> (Vec<TokenKind>, LexState) {
    let total = doc.total_bytes();
    let start = range.start.min(total);
    let end = range.end.min(total);
    if start >= end {
        return (Vec::new(), start_state);
    }
    let len = end - start;
    let Some(syntax) = syntax else {
        return (vec![TokenKind::Normal; len], LexState::Normal);
    };

    let aligned_start = doc.floor_boundary(start);
    let aligned_end = doc.ceil_boundary(end);
    let (mut tokens, end_state) =
        highlight_aligned(syntax, doc, aligned_start..aligned_end, start_state);
    if start != aligned_start {
        let skip = start - aligned_start;
        tokens.copy_within(skip..skip + len, 0);
    }
    tokens.truncate(len);
    (tokens, end_state)
}

fn highlight_aligned(
    syntax: &SyntaxDef,
    doc: &Doc,
    range: Range<usize>,
    start_state: LexState,
) -> (Vec<TokenKind>, LexState) {
    if let Some(prefix) = &syntax.heading_prefix {
        return (
            highlight_heading(doc, range, prefix.as_bytes()),
            LexState::Normal,
        );
    }
    let len = range.len();

    let mut single_slice_tokens: Option<(Vec<TokenKind>, LexState)> = None;
    let mut multi_buf: Vec<u8> = Vec::new();
    let _ = doc.try_for_each_slice(range, |chunk| {
        if multi_buf.is_empty() && chunk.len() == len {
            single_slice_tokens = Some(LineLexer::new(syntax, chunk, start_state).tokenize());
            return ControlFlow::Break(());
        }
        if multi_buf.is_empty() {
            multi_buf.reserve(len);
        }
        multi_buf.extend_from_slice(chunk);
        ControlFlow::Continue(())
    });

    single_slice_tokens
        .unwrap_or_else(|| LineLexer::new(syntax, &multi_buf, start_state).tokenize())
}

/// Pure state-transition single-line lexer (I-51).
struct LineLexer<'a> {
    bytes: &'a [u8],
    syntax: &'a SyntaxDef,
    out: Vec<TokenKind>,
    pos: usize,
    state: LexState,
    comment_bytes: Option<&'a [u8]>,
}

impl<'a> LineLexer<'a> {
    fn new(syntax: &'a SyntaxDef, bytes: &'a [u8], state: LexState) -> Self {
        let n = bytes.len();
        let comment_bytes = syntax
            .line_comment
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(str::as_bytes);
        Self {
            bytes,
            syntax,
            out: vec![TokenKind::Normal; n],
            pos: 0,
            state,
            comment_bytes,
        }
    }

    fn tokenize(mut self) -> (Vec<TokenKind>, LexState) {
        let n = self.bytes.len();
        while self.pos < n {
            match self.state {
                LexState::BlockComment => {
                    self = self.continue_block_comment();
                    continue;
                }
                LexState::RawString { hashes } => {
                    self = self.continue_raw_string(hashes);
                    continue;
                }
                LexState::Normal => {}
            }
            let (next, entered_block) = self.start_block_comment();
            self = next;
            if entered_block {
                continue;
            }
            let (next, done) = self.consume_comment();
            self = next;
            if done {
                break;
            }
            let (next, started_raw) = self.start_raw_string();
            self = next;
            if started_raw {
                continue;
            }
            let (next, matched_str) = self.consume_string();
            self = next;
            if matched_str {
                continue;
            }
            let (next, matched_num) = self.consume_number();
            self = next;
            if matched_num {
                continue;
            }
            let (next, matched_ident) = self.consume_ident();
            self = next;
            if matched_ident {
                continue;
            }
            self.pos += 1;
        }
        (self.out, self.state)
    }

    fn start_block_comment(mut self) -> (Self, bool) {
        let Some((open, _)) = &self.syntax.block_comment else {
            return (self, false);
        };
        if !open.is_empty() && self.bytes[self.pos..].starts_with(open.as_bytes()) {
            let end = self.pos + open.len();
            self.out[self.pos..end].fill(TokenKind::Comment);
            self.pos = end;
            self.state = LexState::BlockComment;
            (self, true)
        } else {
            (self, false)
        }
    }

    fn continue_block_comment(mut self) -> Self {
        let close = self
            .syntax
            .block_comment
            .as_ref()
            .map(|(_, c)| c.as_bytes())
            .unwrap_or(b"*/");
        if close.is_empty() {
            self.out[self.pos..].fill(TokenKind::Comment);
            self.pos = self.bytes.len();
            self.state = LexState::Normal;
            return self;
        }
        if let Some(rel) = self.bytes[self.pos..]
            .windows(close.len())
            .position(|w| w == close)
        {
            let end = self.pos + rel + close.len();
            self.out[self.pos..end].fill(TokenKind::Comment);
            self.pos = end;
            self.state = LexState::Normal;
        } else {
            self.out[self.pos..].fill(TokenKind::Comment);
            self.pos = self.bytes.len();
        }
        self
    }

    fn start_raw_string(mut self) -> (Self, bool) {
        if !self.syntax.raw_strings || self.bytes[self.pos] != b'r' {
            return (self, false);
        }
        let mut j = self.pos + 1;
        let n = self.bytes.len();
        let mut hashes: usize = 0;
        while j < n && self.bytes[j] == b'#' {
            hashes += 1;
            j += 1;
        }
        if j < n && self.bytes[j] == b'"' && hashes <= usize::from(u8::MAX) {
            let end = j + 1;
            self.out[self.pos..end].fill(TokenKind::String);
            self.pos = end;
            self.state = LexState::RawString {
                hashes: hashes as u8,
            };
            (self, true)
        } else {
            (self, false)
        }
    }

    fn continue_raw_string(mut self, hashes: u8) -> Self {
        let n = self.bytes.len();
        let needed = usize::from(hashes);
        let mut j = self.pos;
        while j < n {
            let Some(rel) = memchr::memchr(b'"', &self.bytes[j..]) else {
                break;
            };
            let quote_at = j + rel;
            let after_quote = quote_at + 1;
            if after_quote + needed <= n
                && self.bytes[after_quote..after_quote + needed]
                    .iter()
                    .all(|&b| b == b'#')
            {
                let end = after_quote + needed;
                self.out[self.pos..end].fill(TokenKind::String);
                self.pos = end;
                self.state = LexState::Normal;
                return self;
            }
            j = after_quote;
        }
        self.out[self.pos..].fill(TokenKind::String);
        self.pos = n;
        self
    }

    fn consume_comment(mut self) -> (Self, bool) {
        if let Some(prefix) = self.comment_bytes
            && self.bytes[self.pos..].starts_with(prefix)
        {
            self.out[self.pos..].fill(TokenKind::Comment);
            (self, true)
        } else {
            (self, false)
        }
    }

    fn consume_string(mut self) -> (Self, bool) {
        let b = self.bytes[self.pos];
        if b != b'"' && b != b'\'' && b != b'`' {
            return (self, false);
        }
        if b == b'\'' && self.syntax.single_quote_char_only {
            return self.consume_single_quote_char_or_lifetime();
        }
        let quote = b;
        let mut j = self.pos + 1;
        let n = self.bytes.len();
        while j < n {
            let Some(offset) = memchr::memchr2(quote, b'\\', &self.bytes[j..]) else {
                j = n;
                break;
            };
            j += offset;
            if self.bytes[j] == b'\\' {
                j = (j + 2).min(n);
            } else {
                j += 1;
                break;
            }
        }
        self.out[self.pos..j].fill(TokenKind::String);
        self.pos = j;
        (self, true)
    }

    fn consume_single_quote_char_or_lifetime(mut self) -> (Self, bool) {
        if let Some(end) = self.char_literal_end() {
            self.out[self.pos..end].fill(TokenKind::String);
            self.pos = end;
            return (self, true);
        }
        // Not a char literal: consume `'` and any attached lifetime/label identifier as Normal
        // so `'static` is not highlighted as a keyword.
        self.pos += 1;
        let n = self.bytes.len();
        while self.pos < n && is_ident_byte(self.bytes[self.pos]) {
            self.pos += 1;
        }
        (self, true)
    }

    fn char_literal_end(&self) -> Option<usize> {
        let rest = &self.bytes[self.pos + 1..];
        let &first = rest.first()?;
        if first == b'\\' {
            let &esc = rest.get(1)?;
            return match esc {
                b'x' => {
                    let hex_ok =
                        rest.get(2)?.is_ascii_hexdigit() && rest.get(3)?.is_ascii_hexdigit();
                    (hex_ok && rest.get(4) == Some(&b'\'')).then_some(self.pos + 6)
                }
                b'u' if rest.get(2) == Some(&b'{') => {
                    let mut k = 3;
                    while k < rest.len() && (rest[k].is_ascii_hexdigit() || rest[k] == b'_') {
                        k += 1;
                    }
                    (rest.get(k) == Some(&b'}') && rest.get(k + 1) == Some(&b'\''))
                        .then_some(self.pos + 1 + k + 2)
                }
                _ => (rest.get(2) == Some(&b'\'')).then_some(self.pos + 4),
            };
        }
        if first == b'\'' {
            return None;
        }
        let prefix = &rest[..rest.len().min(4)];
        let valid = match core::str::from_utf8(prefix) {
            Ok(s) => s,
            Err(e) => core::str::from_utf8(&prefix[..e.valid_up_to()]).ok()?,
        };
        let ch = valid.chars().next()?;
        let ch_len = ch.len_utf8();
        (rest.get(ch_len) == Some(&b'\'')).then_some(self.pos + 1 + ch_len + 1)
    }

    fn consume_number(mut self) -> (Self, bool) {
        let b = self.bytes[self.pos];
        if !b.is_ascii_digit() || (self.pos > 0 && is_ident_byte(self.bytes[self.pos - 1])) {
            return (self, false);
        }
        let mut j = self.pos;
        let n = self.bytes.len();
        while j < n
            && (self.bytes[j].is_ascii_alphanumeric()
                || self.bytes[j] == b'_'
                || self.bytes[j] == b'.')
        {
            self.out[j] = TokenKind::Number;
            j += 1;
        }
        self.pos = j;
        (self, true)
    }

    fn consume_ident(mut self) -> (Self, bool) {
        let b = self.bytes[self.pos];
        if !is_ident_start_byte(b) {
            return (self, false);
        }
        let start = self.pos;
        let n = self.bytes.len();
        while self.pos < n && is_ident_byte(self.bytes[self.pos]) {
            self.pos += 1;
        }
        let word_bytes = &self.bytes[start..self.pos];
        let word = core::str::from_utf8(word_bytes).unwrap_or("");
        let target_kind = if self.syntax.is_keyword(word) {
            Some(TokenKind::Keyword)
        } else if self.syntax.is_type(word) {
            Some(TokenKind::Type)
        } else {
            None
        };
        if let Some(kind) = target_kind {
            self.out[start..self.pos].fill(kind);
        }
        (self, true)
    }
}

fn highlight_heading(doc: &Doc, range: Range<usize>, prefix: &[u8]) -> Vec<TokenKind> {
    let len = range.end - range.start;
    let mut matched = 0usize;
    let mut is_heading = prefix.is_empty();
    if !is_heading && len >= prefix.len() {
        let _ = doc.try_for_each_slice(range, |chunk| {
            let need = prefix.len() - matched;
            let take = chunk.len().min(need);
            if chunk[..take] != prefix[matched..matched + take] {
                return ControlFlow::Break(());
            }
            matched += take;
            if matched == prefix.len() {
                is_heading = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
    }
    let kind = if is_heading {
        TokenKind::Keyword
    } else {
        TokenKind::Normal
    };
    vec![kind; len]
}

const fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

pub(crate) const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use alloc::borrow::ToOwned;

    /// The language name that `name` (a file or buffer name) resolves to (I-49).
    fn lang<'a>(reg: &'a SyntaxRegistry, name: &str) -> Option<&'a str> {
        reg.find_by_filename(name).map(|d| d.name.as_str())
    }

    #[test]
    fn registry_detect_lang() {
        let reg = SyntaxRegistry::new().register(SyntaxDef::new(
            "rust",
            vec!["rs".to_string()],
            Vec::new(),
            Some("//".to_string()),
            vec!["fn".to_string(), "let".to_string()],
            Vec::new(),
            false,
        ));
        assert_eq!(lang(&reg, "main.rs"), Some("rust"));
        assert_eq!(lang(&reg, "src/lib.rs"), Some("rust"));
        assert_eq!(lang(&reg, "test.py"), None);
    }

    #[test]
    fn detect_lang_by_exact_filename() {
        let reg = SyntaxRegistry::new().register(SyntaxDef::new(
            "makefile",
            Vec::new(),
            vec!["Makefile".to_string(), "GNUmakefile".to_string()],
            Some("#".to_string()),
            Vec::new(),
            Vec::new(),
            false,
        ));
        assert_eq!(lang(&reg, "Makefile"), Some("makefile"));
        assert_eq!(lang(&reg, "src/Makefile"), Some("makefile"));
        assert_eq!(lang(&reg, "nested/dir/GNUmakefile"), Some("makefile"));
        assert_eq!(lang(&reg, "Makefile.bak"), None);
    }

    #[test]
    fn filename_precedence_over_extension() {
        let def_special = SyntaxDef::new(
            "special",
            Vec::new(),
            vec!["config.c".to_string()],
            Some("//".to_string()),
            Vec::new(),
            Vec::new(),
            false,
        );
        let def_c = SyntaxDef::new(
            "c",
            vec!["c".to_string()],
            Vec::new(),
            Some("//".to_string()),
            Vec::new(),
            Vec::new(),
            false,
        );
        let reg = SyntaxRegistry::new().register(def_c).register(def_special);
        assert_eq!(lang(&reg, "config.c"), Some("special"));
        assert_eq!(lang(&reg, "src/config.c"), Some("special"));
        assert_eq!(lang(&reg, "other.c"), Some("c"));
    }

    #[test]
    fn tokenize_keywords_and_types() {
        let def = SyntaxDef::new(
            "rust",
            vec!["rs".to_string()],
            Vec::new(),
            Some("//".to_string()),
            vec!["let".to_string(), "mut".to_string()],
            vec!["i32".to_string(), "String".to_string()],
            false,
        );
        let line = "let mut x: i32 = 42;";
        let tokens = highlight_line(Some(&def), &Doc::from_bytes(line.as_bytes()), 0..line.len());
        // "let" -> Keyword
        assert_eq!(tokens[0], TokenKind::Keyword);
        assert_eq!(tokens[2], TokenKind::Keyword);
        // "mut" -> Keyword
        assert_eq!(tokens[4], TokenKind::Keyword);
        // "i32" -> Type
        let i32_idx = line.find("i32").unwrap();
        assert_eq!(tokens[i32_idx], TokenKind::Type);
        assert_eq!(tokens[i32_idx + 2], TokenKind::Type);
        // 42 -> Number
        let num_idx = line.find("42").unwrap();
        assert_eq!(tokens[num_idx], TokenKind::Number);
    }

    #[test]
    fn tokenize_case_insensitive() {
        let def = SyntaxDef::new(
            "sql",
            vec!["sql".to_string()],
            Vec::new(),
            Some("--".to_string()),
            vec!["select".to_string(), "where".to_string()],
            vec!["int".to_string(), "varchar".to_string()],
            true,
        );
        let line = "SELECT col FROM t WHERE id: INT;";
        let tokens = highlight_line(Some(&def), &Doc::from_bytes(line.as_bytes()), 0..line.len());
        // "SELECT" -> Keyword
        assert_eq!(tokens[0], TokenKind::Keyword);
        assert_eq!(tokens[5], TokenKind::Keyword);
        // "WHERE" -> Keyword
        let where_idx = line.find("WHERE").unwrap();
        assert_eq!(tokens[where_idx], TokenKind::Keyword);
        // "INT" -> Type
        let int_idx = line.find("INT").unwrap();
        assert_eq!(tokens[int_idx], TokenKind::Type);
    }

    #[test]
    fn registry_highlight_custom_lang() {
        let def = SyntaxDef::new(
            "custom",
            vec!["xyz".to_string()],
            Vec::new(),
            Some("#".to_string()),
            vec!["hello".to_string(), "world".to_string()],
            Vec::new(),
            false,
        );
        let reg = SyntaxRegistry::new().register(def);
        let syntax = reg.find_by_filename("x.xyz");
        let line = "hello 123; # comment";
        let tokens = highlight_line(syntax, &Doc::from_bytes(line.as_bytes()), 0..line.len());
        assert_eq!(tokens[0], TokenKind::Keyword); // "hello"
        assert_eq!(tokens[6], TokenKind::Number); // "123"
        assert_eq!(tokens[11], TokenKind::Comment); // "# comment"
    }

    #[test]
    fn syntax_def_keyword_binary_search_and_dedup() {
        let def = SyntaxDef::new(
            "test",
            vec!["t".to_string()],
            Vec::new(),
            None,
            vec![
                "zebra".to_string(),
                "apple".to_string(),
                "apple".to_string(),
                "middle".to_string(),
            ],
            Vec::new(),
            false,
        );
        assert_eq!(def.keywords, vec!["apple", "middle", "zebra"]);
        assert!(def.is_keyword("apple"));
        assert!(def.is_keyword("middle"));
        assert!(def.is_keyword("zebra"));
        assert!(!def.is_keyword("banana"));
    }

    #[test]
    fn tokenize_string_and_number() {
        let def = SyntaxDef::new(
            "rust",
            vec!["rs".to_string()],
            Vec::new(),
            Some("//".to_string()),
            Vec::new(),
            Vec::new(),
            false,
        );
        let line = "let s = \"hello 123\"; let n = 42;";
        let tokens = highlight_line(Some(&def), &Doc::from_bytes(line.as_bytes()), 0..line.len());
        let str_start = line.find('"').unwrap();
        let str_end = line.rfind('"').unwrap();
        assert_eq!(tokens[str_start], TokenKind::String);
        assert_eq!(tokens[str_end], TokenKind::String);
        // 42 -> Number
        let num_start = line.find("42").unwrap();
        assert_eq!(tokens[num_start], TokenKind::Number);
        assert_eq!(tokens[num_start + 1], TokenKind::Number);
    }

    #[test]
    fn highlight_returns_only_requested_bytes_inside_utf8_units() {
        let doc = Doc::from_bytes("é".as_bytes());
        assert_eq!(highlight_line(None, &doc, 1..2), [TokenKind::Normal]);

        let def = heading_def("notes", None);
        let doc = Doc::from_bytes("é\"x\"".as_bytes());
        let all = highlight_line(Some(&def), &doc, 0..doc.total_bytes());
        for range in [0..1, 1..4, 1..9] {
            assert_eq!(
                highlight_line(Some(&def), &doc, range.clone()),
                all[range.start..range.end.min(doc.total_bytes())]
            );
        }
    }

    fn heading_def(name: &str, prefix: Option<&str>) -> SyntaxDef {
        SyntaxDef::new(
            name,
            vec!["md".to_string()],
            Vec::new(),
            None,
            vec!["let".to_string()],
            Vec::new(),
            false,
        )
        .with_heading_prefix(prefix.map(str::to_owned))
    }

    #[test]
    fn heading_prefix_colors_the_whole_line_and_nothing_else() {
        let def = heading_def("notes", Some("#"));
        let heading = highlight_line(Some(&def), &Doc::from_bytes(b"# let"), 0..5);
        assert!(heading.iter().all(|t| *t == TokenKind::Keyword));
        let other = highlight_line(Some(&def), &Doc::from_bytes(b"x # let"), 0..7);
        assert!(other.iter().all(|t| *t == TokenKind::Normal));
    }

    #[test]
    fn a_syntax_named_markdown_has_no_special_case_without_heading_prefix() {
        let def = heading_def("markdown", None);
        let heading = highlight_line(Some(&def), &Doc::from_bytes(b"# x"), 0..3);
        assert!(heading.iter().all(|t| *t == TokenKind::Normal));
        let kw = highlight_line(Some(&def), &Doc::from_bytes(b"let"), 0..3);
        assert!(kw.iter().all(|t| *t == TokenKind::Keyword));
    }

    #[test]
    #[should_panic(expected = "at least one extension or filename")]
    fn new_rejects_empty_extension_and_filename() {
        SyntaxDef::new(
            "invalid",
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            false,
        );
    }

    #[test]
    fn empty_or_inverted_range_inside_multibyte_char_returns_empty() {
        let def = heading_def("notes", Some("#"));
        let doc = Doc::from_bytes("가".as_bytes());
        assert!(highlight_line(None, &doc, 1..1).is_empty());
        assert!(highlight_line(Some(&def), &doc, 1..1).is_empty());
        assert!(highlight_line(None, &doc, Range { start: 2, end: 1 }).is_empty());
        assert!(highlight_line(Some(&def), &doc, Range { start: 2, end: 1 }).is_empty());
    }

    #[test]
    #[should_panic(expected = "syntax name must not be empty")]
    fn new_rejects_empty_name() {
        SyntaxDef::new(
            "   ",
            vec!["txt".to_string()],
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            false,
        );
    }

    #[test]
    fn single_quote_char_only_preserves_lifetimes_and_char_literals() {
        let def = SyntaxDef::new(
            "rust",
            vec!["rs".to_string()],
            Vec::new(),
            Some("//".to_string()),
            vec!["fn".to_string(), "static".to_string(), "loop".to_string()],
            vec!["Vec".to_string()],
            false,
        )
        .with_single_quote_char_only(true);

        let line = "fn f<'a, 'static>(x: &'a Vec<u8>, c: char) { 'outer: loop {} }";
        let tokens = highlight_line(Some(&def), &Doc::from_bytes(line.as_bytes()), 0..line.len());

        let vec_pos = line.find("Vec").unwrap();
        assert_eq!(&tokens[vec_pos..vec_pos + 3], &[TokenKind::Type; 3]);

        let static_pos = line.find("'static").unwrap();
        assert_eq!(&tokens[static_pos..static_pos + 7], &[TokenKind::Normal; 7]);

        let loop_pos = line.find("loop").unwrap();
        assert_eq!(&tokens[loop_pos..loop_pos + 4], &[TokenKind::Keyword; 4]);

        let chars = "'a' '가' '\\n' '\\'' '\\x7f' '\\u{10FFFF}'";
        let char_tokens = highlight_line(
            Some(&def),
            &Doc::from_bytes(chars.as_bytes()),
            0..chars.len(),
        );
        for (i, &b) in chars.as_bytes().iter().enumerate() {
            let expected = if b == b' ' {
                TokenKind::Normal
            } else {
                TokenKind::String
            };
            assert_eq!(char_tokens[i], expected, "mismatch at byte {i} ({b:?})");
        }

        let with_invalid_tail = b"'a' \xff";
        let invalid_tail_tokens = highlight_line(
            Some(&def),
            &Doc::from_bytes(with_invalid_tail),
            0..with_invalid_tail.len(),
        );
        assert_eq!(
            invalid_tail_tokens,
            [
                TokenKind::String,
                TokenKind::String,
                TokenKind::String,
                TokenKind::Normal,
                TokenKind::Normal,
            ]
        );
    }

    #[test]
    fn multiline_block_comment_and_raw_string_transitions() {
        let def = SyntaxDef::new(
            "rust",
            vec!["rs".to_string()],
            Vec::new(),
            Some("//".to_string()),
            vec!["let".to_string()],
            Vec::new(),
            false,
        )
        .with_block_comment("/*", "*/")
        .with_raw_strings(true);

        let l1 = "let /* open";
        let (t1, s1) = highlight_line_with_state(
            Some(&def),
            &Doc::from_bytes(l1.as_bytes()),
            0..l1.len(),
            LexState::Normal,
        );
        assert_eq!(t1[0], TokenKind::Keyword);
        assert_eq!(t1[4], TokenKind::Comment);
        assert_eq!(s1, LexState::BlockComment);

        let l2 = "still comment */ let x";
        let (t2, s2) =
            highlight_line_with_state(Some(&def), &Doc::from_bytes(l2.as_bytes()), 0..l2.len(), s1);
        assert_eq!(t2[0], TokenKind::Comment);
        assert_eq!(t2[15], TokenKind::Comment); // '/' of '*/'
        assert_eq!(t2[17], TokenKind::Keyword); // 'let'
        assert_eq!(s2, LexState::Normal);

        let r1 = "let s = r#\"multi";
        let (tr1, sr1) = highlight_line_with_state(
            Some(&def),
            &Doc::from_bytes(r1.as_bytes()),
            0..r1.len(),
            LexState::Normal,
        );
        assert_eq!(tr1[8], TokenKind::String);
        assert_eq!(sr1, LexState::RawString { hashes: 1 });

        let r2 = "line \"#; let y";
        let (tr2, sr2) = highlight_line_with_state(
            Some(&def),
            &Doc::from_bytes(r2.as_bytes()),
            0..r2.len(),
            sr1,
        );
        assert_eq!(tr2[0], TokenKind::String);
        assert_eq!(tr2[6], TokenKind::String); // '#' of '"#'
        assert_eq!(tr2[9], TokenKind::Keyword); // 'let'
        assert_eq!(sr2, LexState::Normal);
    }
}
