//! 1-pass lightweight syntax tokenizer for buffer display (§5, I-22).
//! Pure: no regex, no heap bloat, no external crate dependencies.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenKind {
    #[default]
    Normal,
    Keyword,
    String,
    Comment,
    Number,
}

/// Definition of syntax highlighting rules for a language.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntaxDef {
    pub name: String,
    pub extensions: Vec<String>,
    pub line_comment: Option<String>,
    pub keywords: Vec<String>,
}

impl SyntaxDef {
    pub fn new(
        name: String,
        extensions: Vec<String>,
        line_comment: Option<String>,
        mut keywords: Vec<String>,
    ) -> Self {
        keywords.sort_unstable();
        keywords.dedup();
        Self {
            name,
            extensions,
            line_comment,
            keywords,
        }
    }

    /// Checks if a word is a keyword using binary search in $O(\log K)$.
    pub fn is_keyword(&self, word: &str) -> bool {
        self.keywords
            .binary_search_by(|k| k.as_str().cmp(word))
            .is_ok()
    }
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

    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
    }

    pub fn register(mut self, mut def: SyntaxDef) -> Self {
        def.keywords.sort_unstable();
        def.keywords.dedup();
        if let Some(pos) = self.languages.iter().position(|l| l.name == def.name) {
            self.languages[pos] = def;
        } else {
            self.languages.push(def);
        }
        self
    }

    /// Detects language from buffer name or file path extension.
    pub fn detect_lang<'a>(&'a self, name: &str) -> Option<&'a str> {
        self.find_by_filename(name).map(|d| d.name.as_str())
    }

    /// Finds the syntax definition matching the file or buffer name.
    pub fn find_by_filename<'a>(&'a self, name: &str) -> Option<&'a SyntaxDef> {
        let ext = name.rsplit('.').next().unwrap_or("");
        if ext.is_empty() {
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

    pub fn line_comment<'a>(&'a self, lang: &str) -> Option<&'a str> {
        self.find(lang).and_then(|d| d.line_comment.as_deref())
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
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    let Some(syntax) = syntax else {
        return vec![TokenKind::Normal; n];
    };

    if syntax.name == "markdown" {
        return highlight_markdown(&chars);
    }

    let mut out = vec![TokenKind::Normal; n];
    let mut i = 0;

    let comment_prefix = syntax.line_comment.as_deref();
    let comment_chars: Option<Vec<char>> = comment_prefix.map(|p| p.chars().collect());

    while i < n {
        let c = chars[i];

        // Line comment check
        if let Some(ref prefix) = comment_chars {
            if !prefix.is_empty() && chars[i..].starts_with(prefix) {
                for slot in out.iter_mut().skip(i) {
                    *slot = TokenKind::Comment;
                }
                break;
            }
        }

        // String literals: "..." or '...' or `...`
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            out[i] = TokenKind::String;
            let mut j = i + 1;
            let mut escaped = false;
            while j < n {
                out[j] = TokenKind::String;
                if escaped {
                    escaped = false;
                } else if chars[j] == '\\' {
                    escaped = true;
                } else if chars[j] == quote {
                    j += 1;
                    break;
                }
                j += 1;
            }
            i = j;
            continue;
        }

        // Numbers: single digits or hex/decimal numbers starting with digit
        if c.is_ascii_digit() && (i == 0 || !is_ident_char(chars[i - 1])) {
            let mut j = i;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '.')
            {
                out[j] = TokenKind::Number;
                j += 1;
            }
            i = j;
            continue;
        }

        // Identifiers & Keywords
        if is_ident_start(c) {
            let start = i;
            while i < n && is_ident_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if syntax.is_keyword(&word) {
                for slot in &mut out[start..i] {
                    *slot = TokenKind::Keyword;
                }
            }
            continue;
        }

        i += 1;
    }

    out
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
        let reg = SyntaxRegistry::new().register(SyntaxDef::new(
            "rust".to_string(),
            vec!["rs".to_string()],
            Some("//".to_string()),
            vec!["fn".to_string(), "let".to_string()],
        ));
        assert_eq!(reg.detect_lang("main.rs"), Some("rust"));
        assert_eq!(reg.detect_lang("src/lib.rs"), Some("rust"));
        assert_eq!(reg.detect_lang("test.py"), None);
        assert!(reg.find_by_filename("main.rs").is_some());
    }

    #[test]
    fn registry_highlight_custom_lang() {
        let def = SyntaxDef::new(
            "custom".to_string(),
            vec!["xyz".to_string()],
            Some("#".to_string()),
            vec!["hello".to_string(), "world".to_string()],
        );
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
        let def = SyntaxDef::new(
            "test".to_string(),
            vec!["t".to_string()],
            None,
            vec![
                "zebra".to_string(),
                "apple".to_string(),
                "apple".to_string(),
                "middle".to_string(),
            ],
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
            "rust".to_string(),
            vec!["rs".to_string()],
            Some("//".to_string()),
            vec![],
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
}
