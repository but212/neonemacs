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
    /// Creates a new builder for constructing a validated `SyntaxDef`.
    pub fn builder(name: impl Into<String>) -> SyntaxDefBuilder {
        SyntaxDefBuilder::new(name.into())
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
    pub fn is_keyword(&self, word: &str) -> bool {
        self.contains_word(&self.keywords, word)
    }

    /// Checks if a word is a type using binary search in $O(\log T)$ (I-29).
    pub fn is_type(&self, word: &str) -> bool {
        self.contains_word(&self.types, word)
    }
}

/// Builder for constructing validated `SyntaxDef` instances (I-27, I-28).
#[derive(Debug, Clone, Default)]
pub struct SyntaxDefBuilder {
    name: String,
    extensions: Vec<String>,
    filenames: Vec<String>,
    line_comment: Option<String>,
    keywords: Vec<String>,
    types: Vec<String>,
    case_insensitive: bool,
}

impl SyntaxDefBuilder {
    pub fn new(name: String) -> Self {
        Self {
            name,
            extensions: Vec::new(),
            filenames: Vec::new(),
            line_comment: None,
            keywords: Vec::new(),
            types: Vec::new(),
            case_insensitive: false,
        }
    }

    pub fn extensions(mut self, extensions: Vec<String>) -> Self {
        self.extensions = extensions;
        self
    }

    pub fn filenames(mut self, filenames: Vec<String>) -> Self {
        self.filenames = filenames;
        self
    }

    pub fn line_comment(mut self, line_comment: Option<String>) -> Self {
        self.line_comment = line_comment;
        self
    }

    pub fn keywords(mut self, keywords: Vec<String>) -> Self {
        self.keywords = keywords;
        self
    }

    pub fn types(mut self, types: Vec<String>) -> Self {
        self.types = types;
        self
    }

    pub fn case_insensitive(mut self, case_insensitive: bool) -> Self {
        self.case_insensitive = case_insensitive;
        self
    }

    /// Builds a normalized and validated `SyntaxDef`.
    /// Panics if name is empty or both extensions and filenames are empty (I-28).
    pub fn build(self) -> SyntaxDef {
        assert!(
            !self.name.trim().is_empty(),
            "syntax name must not be empty"
        );
        assert!(
            !self.extensions.is_empty() || !self.filenames.is_empty(),
            "syntax spec must have at least one extension or filename (I-28)"
        );

        let mut keywords = self.keywords;
        let mut types = self.types;
        if self.case_insensitive {
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

        SyntaxDef {
            name: self.name,
            extensions: self.extensions,
            filenames: self.filenames,
            line_comment: self.line_comment,
            keywords,
            types,
            case_insensitive: self.case_insensitive,
        }
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
    pub const fn new() -> Self {
        Self {
            languages: Vec::new(),
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.languages.is_empty()
    }

    pub fn register(mut self, def: SyntaxDef) -> Self {
        if let Some(pos) = self.languages.iter().position(|l| l.name == def.name) {
            self.languages[pos] = def;
        } else {
            self.languages.push(def);
        }
        self
    }

    /// Detects language from buffer name or file path (I-28).
    pub fn detect_lang<'a>(&'a self, name: &str) -> Option<&'a str> {
        self.find_by_filename(name).map(|d| d.name.as_str())
    }

    /// Finds the syntax definition matching the file or buffer name.
    /// Priority (I-28): Exact basename match against `filenames` > extension match against `extensions`.
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

    pub fn find<'a>(&'a self, lang: &str) -> Option<&'a SyntaxDef> {
        self.languages.iter().find(|l| l.name == lang)
    }

    pub fn is_keyword(&self, lang: &str, word: &str) -> bool {
        self.find(lang).is_some_and(|d| d.is_keyword(word))
    }

    pub fn is_type(&self, lang: &str, word: &str) -> bool {
        self.find(lang).is_some_and(|d| d.is_type(word))
    }

    pub fn line_comment<'a>(&'a self, lang: &str) -> Option<&'a str> {
        self.find(lang).and_then(|d| d.line_comment.as_deref())
    }

    pub fn line_comment_for_filename<'a>(&'a self, filename: &str) -> Option<&'a str> {
        self.detect_lang(filename)
            .and_then(|lang| self.line_comment(lang))
    }
}

/// Helper function to detect language using registry.
pub fn detect_lang<'a>(registry: &'a SyntaxRegistry, name: &str) -> Option<&'a str> {
    registry.detect_lang(name)
}

/// Tokenizes a single line of text into per-character `TokenKind` array.
/// Returns a `Vec<TokenKind>` whose length matches `line.chars().count()`.
pub fn highlight_line(syntax: Option<&SyntaxDef>, line: &str) -> Vec<TokenKind> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let Some(syntax) = syntax else {
        return vec![TokenKind::Normal; chars.len()];
    };

    if syntax.name == "markdown" {
        return highlight_markdown(&chars);
    }

    LineLexer::new(syntax, chars).tokenize()
}

/// Single-line stateless lexer encapsulating tokenization rules (I-30).
struct LineLexer<'a> {
    chars: Vec<char>,
    syntax: &'a SyntaxDef,
    out: Vec<TokenKind>,
    pos: usize,
    comment_chars: Option<Vec<char>>,
}

impl<'a> LineLexer<'a> {
    fn new(syntax: &'a SyntaxDef, chars: Vec<char>) -> Self {
        let n = chars.len();
        let comment_chars = syntax
            .line_comment
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| p.chars().collect());
        Self {
            chars,
            syntax,
            out: vec![TokenKind::Normal; n],
            pos: 0,
            comment_chars,
        }
    }

    fn tokenize(mut self) -> Vec<TokenKind> {
        let n = self.chars.len();
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
        if let Some(ref prefix) = self.comment_chars
            && self.chars[self.pos..].starts_with(prefix)
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
        let c = self.chars[self.pos];
        if c != '"' && c != '\'' && c != '`' {
            return (self, false);
        }
        let quote = c;
        self.out[self.pos] = TokenKind::String;
        let mut j = self.pos + 1;
        let mut escaped = false;
        let n = self.chars.len();
        while j < n {
            self.out[j] = TokenKind::String;
            if escaped {
                escaped = false;
            } else if self.chars[j] == '\\' {
                escaped = true;
            } else if self.chars[j] == quote {
                j += 1;
                break;
            }
            j += 1;
        }
        self.pos = j;
        (self, true)
    }

    fn consume_number(mut self) -> (Self, bool) {
        let c = self.chars[self.pos];
        if !c.is_ascii_digit() || (self.pos > 0 && is_ident_char(self.chars[self.pos - 1])) {
            return (self, false);
        }
        let mut j = self.pos;
        let n = self.chars.len();
        while j < n
            && (self.chars[j].is_ascii_alphanumeric()
                || self.chars[j] == '_'
                || self.chars[j] == '.')
        {
            self.out[j] = TokenKind::Number;
            j += 1;
        }
        self.pos = j;
        (self, true)
    }

    fn consume_ident(mut self) -> (Self, bool) {
        let c = self.chars[self.pos];
        if !is_ident_start(c) {
            return (self, false);
        }
        let start = self.pos;
        let n = self.chars.len();
        while self.pos < n && is_ident_char(self.chars[self.pos]) {
            self.pos += 1;
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        let target_kind = if self.syntax.is_keyword(&word) {
            Some(TokenKind::Keyword)
        } else if self.syntax.is_type(&word) {
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

fn highlight_markdown(chars: &[char]) -> Vec<TokenKind> {
    let mut out = vec![TokenKind::Normal; chars.len()];
    if chars.is_empty() {
        return out;
    }
    // Headings starting with #
    if chars[0] == '#' {
        out.fill(TokenKind::Keyword);
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_detect_lang() {
        let reg = SyntaxRegistry::new().register(
            SyntaxDef::builder("rust")
                .extensions(vec!["rs".to_string()])
                .line_comment(Some("//".to_string()))
                .keywords(vec!["fn".to_string(), "let".to_string()])
                .build(),
        );
        assert_eq!(reg.detect_lang("main.rs"), Some("rust"));
        assert_eq!(reg.detect_lang("src/lib.rs"), Some("rust"));
        assert_eq!(reg.detect_lang("test.py"), None);
        assert!(reg.find_by_filename("main.rs").is_some());
    }

    #[test]
    fn detect_lang_by_exact_filename() {
        let reg = SyntaxRegistry::new().register(
            SyntaxDef::builder("makefile")
                .filenames(vec!["Makefile".to_string(), "GNUmakefile".to_string()])
                .line_comment(Some("#".to_string()))
                .build(),
        );
        assert_eq!(reg.detect_lang("Makefile"), Some("makefile"));
        assert_eq!(reg.detect_lang("src/Makefile"), Some("makefile"));
        assert_eq!(reg.detect_lang("nested/dir/GNUmakefile"), Some("makefile"));
        assert_eq!(reg.detect_lang("Makefile.bak"), None);
    }

    #[test]
    fn filename_precedence_over_extension() {
        let def_special = SyntaxDef::builder("special")
            .filenames(vec!["config.c".to_string()])
            .line_comment(Some("//".to_string()))
            .build();
        let def_c = SyntaxDef::builder("c")
            .extensions(vec!["c".to_string()])
            .line_comment(Some("//".to_string()))
            .build();
        let reg = SyntaxRegistry::new().register(def_c).register(def_special);
        assert_eq!(reg.detect_lang("config.c"), Some("special"));
        assert_eq!(reg.detect_lang("src/config.c"), Some("special"));
        assert_eq!(reg.detect_lang("other.c"), Some("c"));
    }

    #[test]
    fn tokenize_keywords_and_types() {
        let def = SyntaxDef::builder("rust")
            .extensions(vec!["rs".to_string()])
            .line_comment(Some("//".to_string()))
            .keywords(vec!["let".to_string(), "mut".to_string()])
            .types(vec!["i32".to_string(), "String".to_string()])
            .build();
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
        let def = SyntaxDef::builder("sql")
            .extensions(vec!["sql".to_string()])
            .line_comment(Some("--".to_string()))
            .keywords(vec!["select".to_string(), "where".to_string()])
            .types(vec!["int".to_string(), "varchar".to_string()])
            .case_insensitive(true)
            .build();
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
        let def = SyntaxDef::builder("custom")
            .extensions(vec!["xyz".to_string()])
            .line_comment(Some("#".to_string()))
            .keywords(vec!["hello".to_string(), "world".to_string()])
            .build();
        let reg = SyntaxRegistry::new().register(def);
        let syntax = reg.find("custom");
        let line = "hello 123; # comment";
        let tokens = highlight_line(syntax, line);
        assert_eq!(tokens[0], TokenKind::Keyword); // "hello"
        assert_eq!(tokens[6], TokenKind::Number); // "123"
        assert_eq!(tokens[11], TokenKind::Comment); // "# comment"
    }

    #[test]
    fn syntax_def_keyword_binary_search_and_dedup() {
        let def = SyntaxDef::builder("test")
            .extensions(vec!["t".to_string()])
            .keywords(vec![
                "zebra".to_string(),
                "apple".to_string(),
                "apple".to_string(),
                "middle".to_string(),
            ])
            .build();
        assert_eq!(def.keywords, vec!["apple", "middle", "zebra"]);
        assert!(def.is_keyword("apple"));
        assert!(def.is_keyword("middle"));
        assert!(def.is_keyword("zebra"));
        assert!(!def.is_keyword("banana"));
    }

    #[test]
    fn tokenize_string_and_number() {
        let def = SyntaxDef::builder("rust")
            .extensions(vec!["rs".to_string()])
            .line_comment(Some("//".to_string()))
            .build();
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
    fn builder_rejects_empty_extension_and_filename() {
        SyntaxDef::builder("invalid").build();
    }

    #[test]
    #[should_panic(expected = "syntax name must not be empty")]
    fn builder_rejects_empty_name() {
        SyntaxDef::builder("   ")
            .extensions(vec!["txt".to_string()])
            .build();
    }
}
