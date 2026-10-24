//! C-07, C-08, I-22: Syntax highlighting and styled render without character width or cursor distortion.

use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use neonemacs::core::config::Config;
use neonemacs::core::editor::{Buffer, Editor, Window};
use neonemacs::core::file::Backing;
use neonemacs::core::render;
use neonemacs::core::syntax::{TokenKind, detect_lang, highlight_line};
use neonemacs::core::text::Text;
use neonemacs::shell::lua;
use neonemacs::shell::session::ShellSession;

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

#[test]
fn shell_session_render_sized_wide_hangul_and_scroll_writeback() {
    let src = "let s = \"가나다\"; let n = 7; // 주석\nline2\nline3\nline4\n";
    let ed = Editor::new(Config::default())
        .with_current_buffer(|b| Buffer {
            name: "hangul.rs".to_owned(),
            text: Text::from_bytes(src.as_bytes()),
            backing: Backing::None,
            ..b
        })
        // Place point on line 4 so `render::scroll` must scroll `window().top`
        .with_window(|w| Window {
            point: src.len() - 2,
            ..w
        });

    let lua = lua::new_state().unwrap();
    let mut session = ShellSession::new(ed, lua);
    let mut out = Vec::new();

    // Height 3 (1 body row + 1 status row + 1 echo row): forces vertical scroll to line 4
    session.render_sized(&mut out, 40, 3).unwrap();
    assert!(
        session.ed.window().top > 0,
        "render_sized must persist scrolled window.top back into session.ed"
    );

    // Move point back to 0 and render with full height so row 0 ("let s = \"가나다\"...") is drawn
    session.ed = session.ed.with_window(|w| Window { point: 0, ..w });
    let mut out_top = Vec::new();
    session.render_sized(&mut out_top, 40, 5).unwrap();

    // Verify that after the wide string `"가나다"`, `let` is still styled Cyan and `7` is styled Yellow
    let mut expected_fragment = Vec::new();
    queue!(
        &mut expected_fragment,
        SetForegroundColor(Color::Green),
        Print("\"가나다\""),
        ResetColor,
        Print("; "),
        SetForegroundColor(Color::Cyan),
        Print("let"),
        ResetColor,
        Print(" n = "),
        SetForegroundColor(Color::Yellow),
        Print("7"),
        ResetColor
    )
    .unwrap();

    assert!(
        out_top
            .windows(expected_fragment.len())
            .any(|w| w == expected_fragment.as_slice()),
        "ANSI colors after wide Hangul string must not shift"
    );
}
