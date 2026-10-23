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

/// Detects language from buffer name or file path extension.
pub fn detect_lang(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Some("rust"),
        "c" | "h" | "cpp" | "hpp" | "cc" => Some("c"),
        "py" => Some("python"),
        "js" | "ts" | "jsx" | "tsx" => Some("javascript"),
        "lua" => Some("lua"),
        "go" => Some("go"),
        "sh" | "bash" => Some("shell"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "int", "long", "register", "return", "short",
    "signed", "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned", "void",
    "volatile", "while",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];

const JS_KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];

fn is_keyword(lang: &str, word: &str) -> bool {
    let list = match lang {
        "rust" => RUST_KEYWORDS,
        "c" => C_KEYWORDS,
        "python" => PYTHON_KEYWORDS,
        "javascript" => JS_KEYWORDS,
        "lua" => LUA_KEYWORDS,
        "go" => GO_KEYWORDS,
        _ => return false,
    };
    list.contains(&word)
}

/// Tokenizes a single line of text into per-character `TokenKind` array.
/// Returns a `Vec<TokenKind>` whose length matches `line.chars().count()`.
pub fn highlight_line(lang: Option<&str>, line: &str) -> Vec<TokenKind> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    let Some(lang) = lang else {
        return vec![TokenKind::Normal; n];
    };

    if lang == "markdown" {
        return highlight_markdown(&chars);
    }

    let mut out = vec![TokenKind::Normal; n];
    let mut i = 0;

    while i < n {
        let c = chars[i];

        // Line comment check
        if (lang == "rust" || lang == "c" || lang == "javascript" || lang == "go")
            && c == '/'
            && i + 1 < n
            && chars[i + 1] == '/'
        {
            for slot in out.iter_mut().skip(i) {
                *slot = TokenKind::Comment;
            }
            break;
        }

        if (lang == "python" || lang == "shell") && c == '#' {
            for slot in out.iter_mut().skip(i) {
                *slot = TokenKind::Comment;
            }
            break;
        }

        if lang == "lua" && c == '-' && i + 1 < n && chars[i + 1] == '-' {
            for slot in out.iter_mut().skip(i) {
                *slot = TokenKind::Comment;
            }
            break;
        }

        // String literals: "..." or '...' or `...`
        if c == '"' || c == '\'' || (c == '`' && (lang == "javascript" || lang == "go")) {
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
            if is_keyword(lang, &word) {
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
    fn detect_languages() {
        assert_eq!(detect_lang("main.rs"), Some("rust"));
        assert_eq!(detect_lang("test.py"), Some("python"));
        assert_eq!(detect_lang("script.lua"), Some("lua"));
        assert_eq!(detect_lang("file.txt"), None);
    }

    #[test]
    fn tokenize_rust_line() {
        let line = "pub fn add(x: i32) -> i32 { // comment";
        let tokens = highlight_line(Some("rust"), line);
        assert_eq!(tokens.len(), line.chars().count());
        // "pub" -> Keyword
        assert_eq!(tokens[0], TokenKind::Keyword);
        assert_eq!(tokens[2], TokenKind::Keyword);
        assert_eq!(tokens[3], TokenKind::Normal); // space
        // "fn" -> Keyword
        assert_eq!(tokens[4], TokenKind::Keyword);
        assert_eq!(tokens[5], TokenKind::Keyword);
        // comment at the end
        let comment_start = line.find("//").unwrap();
        assert_eq!(tokens[comment_start], TokenKind::Comment);
        assert_eq!(tokens[tokens.len() - 1], TokenKind::Comment);
    }

    #[test]
    fn tokenize_string_and_number() {
        let line = "let s = \"hello 123\"; let n = 42;";
        let tokens = highlight_line(Some("rust"), line);
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
