//! 1-pass lightweight syntax tokenizer for buffer display (§5, I-22).
//! Pure: no regex, no heap bloat, no external crate dependencies.

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

/// Definition of syntax highlighting rules for a language (I-27, I-28).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntaxDef {
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub line_comment: Option<String>,
    pub keywords: Vec<String>,
    pub types: Vec<String>,
    pub case_insensitive: bool,
}

impl SyntaxDef {
    /// Creates a validated and normalized `SyntaxDef` (I-28, I-29).
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
            "syntax spec must have at least one extension or filename (I-28)"
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
            keywords,
            types,
            case_insensitive,
        }
    }

    fn contains_word(&self, list: &[String], word: &str) -> bool {
        if self.case_insensitive {
            list.binary_search_by(|k| cmp_ignore_ascii_case(k.as_str(), word))
                .is_ok()
        } else {
            list.binary_search_by(|k| k.as_str().cmp(word)).is_ok()
        }
    }

    /// Checks if a word is a keyword using binary search in $O(\log K)$ (I-29).
    #[must_use]
    pub fn is_keyword(&self, word: &str) -> bool {
        self.contains_word(&self.keywords, word)
    }

    /// Checks if a word is a type using binary search in $O(\log T)$ (I-29).
    #[must_use]
    pub fn is_type(&self, word: &str) -> bool {
        self.contains_word(&self.types, word)
    }
}

fn cmp_ignore_ascii_case(a: &str, b: &str) -> std::cmp::Ordering {
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
    /// Priority (I-28): Exact basename match against `filenames` > extension match against `extensions`.
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

    #[must_use]
    pub fn line_comment_for_filename<'a>(&'a self, filename: &str) -> Option<&'a str> {
        self.find_by_filename(filename)
            .and_then(|d| d.line_comment.as_deref())
    }
}

/// Tokenizes a single line of text into per-character `TokenKind` array.
/// Returns a `Vec<TokenKind>` whose length matches `line.chars().count()`.
pub fn highlight_line(syntax: Option<&SyntaxDef>, line: impl AsRef<[u8]>) -> Vec<TokenKind> {
    let bytes = line.as_ref();
    if bytes.is_empty() {
        return Vec::new();
    }
    let Some(syntax) = syntax else {
        return vec![TokenKind::Normal; bytes.len()];
    };

    if syntax.name == "markdown" {
        return highlight_markdown(bytes);
    }

    LineLexer::new(syntax, bytes).tokenize()
}

/// Single-line stateless lexer encapsulating tokenization rules (I-30).
struct LineLexer<'a> {
    bytes: &'a [u8],
    syntax: &'a SyntaxDef,
    out: Vec<TokenKind>,
    pos: usize,
    comment_bytes: Option<&'a [u8]>,
}

impl<'a> LineLexer<'a> {
    fn new(syntax: &'a SyntaxDef, bytes: &'a [u8]) -> Self {
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
            comment_bytes,
        }
    }

    fn tokenize(mut self) -> Vec<TokenKind> {
        let n = self.bytes.len();
        while self.pos < n {
            let (next, done) = self.consume_comment();
            self = next;
            if done {
                break;
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
        self.out
    }

    fn consume_comment(mut self) -> (Self, bool) {
        if let Some(prefix) = self.comment_bytes
            && self.bytes[self.pos..].starts_with(prefix)
        {
            let pos = self.pos;
            let slots = &mut self.out[pos..];
            for slot in slots {
                *slot = TokenKind::Comment;
            }
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
        let quote = b;
        self.out[self.pos] = TokenKind::String;
        let mut j = self.pos + 1;
        let mut escaped = false;
        let n = self.bytes.len();
        while j < n {
            self.out[j] = TokenKind::String;
            if escaped {
                escaped = false;
            } else if self.bytes[j] == b'\\' {
                escaped = true;
            } else if self.bytes[j] == quote {
                j += 1;
                break;
            }
            j += 1;
        }
        self.pos = j;
        (self, true)
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
        let word = std::str::from_utf8(word_bytes).unwrap_or("");
        let target_kind = if self.syntax.is_keyword(word) {
            Some(TokenKind::Keyword)
        } else if self.syntax.is_type(word) {
            Some(TokenKind::Type)
        } else {
            None
        };
        if let Some(kind) = target_kind {
            let end = self.pos;
            let slots = &mut self.out[start..end];
            for slot in slots {
                *slot = kind;
            }
        }
        (self, true)
    }
}

fn highlight_markdown(bytes: &[u8]) -> Vec<TokenKind> {
    let mut out = vec![TokenKind::Normal; bytes.len()];
    if bytes.is_empty() {
        return out;
    }
    // Headings starting with #
    if bytes[0] == b'#' {
        out.fill(TokenKind::Keyword);
    }
    out
}

const fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The language name that `name` (a file or buffer name) resolves to (I-28).
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
        assert!(reg.find_by_filename("main.rs").is_some());
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
        let tokens = highlight_line(Some(&def), line);
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
        let tokens = highlight_line(Some(&def), line);
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
        let tokens = highlight_line(syntax, line);
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
        let tokens = highlight_line(Some(&def), line);
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
}
