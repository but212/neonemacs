//! C-07, C-08, I-22: Syntax highlighting and styled render without character width or cursor distortion.

use neonemacs::core::config::Config;
use neonemacs::core::editor::{Buffer, Editor};
use neonemacs::core::file::Backing;
use neonemacs::core::render;
use neonemacs::core::syntax::{TokenKind, detect_lang, highlight_line};
use neonemacs::core::text::Text;

#[test]
fn syntax_lexer_detects_and_tokenizes_multiple_languages() {
    // Rust
    assert_eq!(detect_lang("lib.rs"), Some("rust"));
    let rust_tokens = highlight_line(Some("rust"), "let x = 10; // assign");
    assert_eq!(rust_tokens[0], TokenKind::Keyword); // "let"
    assert_eq!(rust_tokens[8], TokenKind::Number); // "10"
    assert_eq!(rust_tokens[13], TokenKind::Comment); // "// assign"

    // Python
    assert_eq!(detect_lang("script.py"), Some("python"));
    let py_tokens = highlight_line(Some("python"), "def foo(): # comment");
    assert_eq!(py_tokens[0], TokenKind::Keyword); // "def"
    assert_eq!(py_tokens[11], TokenKind::Comment); // "# comment"

    // C
    assert_eq!(detect_lang("main.c"), Some("c"));
    let c_tokens = highlight_line(Some("c"), "int a = 0;");
    assert_eq!(c_tokens[0], TokenKind::Keyword); // "int"
    assert_eq!(c_tokens[8], TokenKind::Number); // "0"

    // Lua
    assert_eq!(detect_lang("init.lua"), Some("lua"));
    let lua_tokens = highlight_line(Some("lua"), "local v = 'str' -- comment");
    assert_eq!(lua_tokens[0], TokenKind::Keyword); // "local"
    assert_eq!(lua_tokens[10], TokenKind::String); // 'str'
    assert_eq!(lua_tokens[16], TokenKind::Comment); // -- comment
}

#[test]
fn styled_render_preserves_column_and_cursor_positions() {
    let ed = Editor::new(Config::default()).with_current_buffer(|b| Buffer {
        name: "test.rs".to_owned(),
        text: Text::from_bytes(b"fn main() {\n    let x = 42;\n}\n"),
        backing: Backing::None,
        ..b
    });

    let (cols, rows) = (40, 10);
    let ed = render::scroll(ed, cols, rows);
    let frame = render::frame(&ed, cols, rows);

    // Rows must be exactly formatted text without ANSI codes embedded
    assert_eq!(frame.rows.len(), rows);
    assert!(frame.rows[0].starts_with("fn main() {"));
    assert!(frame.rows[1].starts_with("    let x = 42;"));

    // Style grid must be populated
    assert_eq!(frame.styles.len(), rows);
    let line0_styles = &frame.styles[0];
    assert_eq!(line0_styles[0], TokenKind::Keyword); // "f"
    assert_eq!(line0_styles[1], TokenKind::Keyword); // "n"
    assert_eq!(line0_styles[2], TokenKind::Normal); // " "
    assert_eq!(line0_styles[3], TokenKind::Normal); // "m"

    let line1_styles = &frame.styles[1];
    // "    let"
    assert_eq!(line1_styles[4], TokenKind::Keyword); // "l"
    assert_eq!(line1_styles[5], TokenKind::Keyword); // "e"
    assert_eq!(line1_styles[6], TokenKind::Keyword); // "t"
    // 42 -> Number
    let pos_42 = frame.rows[1].find("42").unwrap();
    assert_eq!(line1_styles[pos_42], TokenKind::Number);
    assert_eq!(line1_styles[pos_42 + 1], TokenKind::Number);

    // Cursor position remains unaffected (I-22)
    assert_eq!(frame.cursor, (0, 0));
}
