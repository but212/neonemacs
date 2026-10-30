//! C-07, C-08, I-22: Syntax highlighting and styled render without character width or cursor distortion.

use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use neonemacs::core::config::Config;
use neonemacs::core::editor::{Editor, Window};
use neonemacs::core::file::Backing;
use neonemacs::core::render;
use neonemacs::core::syntax::{TokenKind, highlight_line};
use neonemacs::core::text::Text;
use neonemacs::shell::lua;
use neonemacs::shell::session::ShellSession;

#[test]
fn syntax_lexer_detects_and_tokenizes_multiple_languages() {
    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua);
    let reg = ed.syntax_registry();

    // Rust
    assert_eq!(
        reg.find_by_filename("lib.rs").map(|d| d.name.as_str()),
        Some("rust")
    );
    let rust_tokens = highlight_line(reg.find_by_filename("lib.rs"), "let x = 10; // assign");
    assert_eq!(rust_tokens[0], TokenKind::Keyword); // "let"
    assert_eq!(rust_tokens[8], TokenKind::Number); // "10"
    assert_eq!(rust_tokens[13], TokenKind::Comment); // "// assign"

    // Python
    assert_eq!(
        reg.find_by_filename("script.py").map(|d| d.name.as_str()),
        Some("python")
    );
    let py_tokens = highlight_line(reg.find_by_filename("script.py"), "def foo(): # comment");
    assert_eq!(py_tokens[0], TokenKind::Keyword); // "def"
    assert_eq!(py_tokens[11], TokenKind::Comment); // "# comment"

    // C
    assert_eq!(
        reg.find_by_filename("main.c").map(|d| d.name.as_str()),
        Some("c")
    );
    let c_tokens = highlight_line(reg.find_by_filename("main.c"), "int a = 0; return a;");
    assert_eq!(c_tokens[0], TokenKind::Type); // "int"
    assert_eq!(c_tokens[8], TokenKind::Number); // "0"
    let ret_idx = "int a = 0; return a;".find("return").unwrap();
    assert_eq!(c_tokens[ret_idx], TokenKind::Keyword); // "return"

    // Lua
    assert_eq!(
        reg.find_by_filename("init.lua").map(|d| d.name.as_str()),
        Some("lua")
    );
    let lua_tokens = highlight_line(
        reg.find_by_filename("init.lua"),
        "local v = 'str' -- comment",
    );
    assert_eq!(lua_tokens[0], TokenKind::Keyword); // "local"
    assert_eq!(lua_tokens[10], TokenKind::String); // 'str'
    assert_eq!(lua_tokens[16], TokenKind::Comment); // -- comment
}

#[test]
fn styled_render_preserves_column_and_cursor_positions() {
    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua).with_current_buffer(|b| {
        b.set_name("test.rs")
            .with_text(Text::from_bytes(b"fn main() {\n    let x = 42;\n}\n"))
            .set_backing(Backing::None)
    });

    let (cols, rows) = (40, 10);
    let ed = render::scroll(ed, cols, rows);
    let frame = render::frame(&ed, cols, rows);

    // Rows must be exactly formatted text without ANSI codes embedded
    assert_eq!(frame.rows.len(), rows);
    let row_texts = frame.row_strings();
    assert!(row_texts[0].starts_with("fn main() {"));
    assert!(row_texts[1].starts_with("    let x = 42;"));

    // Style in Cell grid must be populated with 1:1 cell alignment
    assert_eq!(frame.rows[0][0].style, TokenKind::Keyword); // "f"
    assert_eq!(frame.rows[0][1].style, TokenKind::Keyword); // "n"
    assert_eq!(frame.rows[0][2].style, TokenKind::Normal); // " "
    assert_eq!(frame.rows[0][3].style, TokenKind::Normal); // "m"

    // "    let"
    assert_eq!(frame.rows[1][4].style, TokenKind::Keyword); // "l"
    assert_eq!(frame.rows[1][5].style, TokenKind::Keyword); // "e"
    assert_eq!(frame.rows[1][6].style, TokenKind::Keyword); // "t"
    let pos_42 = row_texts[1].find("42").unwrap();
    assert_eq!(frame.rows[1][pos_42].style, TokenKind::Number);
    assert_eq!(frame.rows[1][pos_42 + 1].style, TokenKind::Number);

    // Cursor position remains unaffected (I-22)
    assert_eq!(frame.cursor, (0, 0));
}

#[test]
fn shell_session_render_sized_wide_hangul_and_scroll_writeback() {
    let src = "let s = \"가나다\"; let n = 7; // 주석\nline2\nline3\nline4\n";
    let ed = Editor::new(Config::default())
        .with_current_buffer(|b| {
            b.set_name("hangul.rs")
                .with_text(Text::from_bytes(src.as_bytes()))
                .set_backing(Backing::None)
        })
        // Place point on line 4 so `render::scroll` must scroll `window().top`
        .with_window(|w| Window {
            point: src.len() - 2,
            ..w
        });

    let lua = lua::new_state().unwrap();
    let mut session = ShellSession::new(lua::bootstrap(ed, &lua), lua);
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

#[test]
fn raw_non_utf8_bytes_do_not_shift_syntax_highlighting() {
    let lua = lua::new_state().unwrap();
    // Raw invalid UTF-8 bytes: \xe4\xb8 inside string literal on the same line
    let src = b"let a = \"\xe4\xb8\"; let x = 7;\n";
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua).with_current_buffer(|b| {
        b.set_name("test.rs")
            .with_text(Text::from_bytes(src))
            .set_backing(Backing::None)
    });

    let (cols, rows) = (40, 5);
    let frame = render::frame(&ed, cols, rows);
    let first_row = &frame.rows[0];

    // Find "let x = 7;" in first_row
    let _row_str: String = first_row.iter().map(|c| c.glyph.as_str()).collect();
    // 'l', 'e', 't' of second let are at cells 14, 15, 16
    assert_eq!(first_row[14].glyph, "l");
    assert_eq!(
        first_row[14].style,
        TokenKind::Keyword,
        "second 'let' keyword style was shifted or corrupted by raw bytes!"
    );
    assert_eq!(first_row[15].glyph, "e");
    assert_eq!(first_row[15].style, TokenKind::Keyword);
    assert_eq!(first_row[16].glyph, "t");
    assert_eq!(first_row[16].style, TokenKind::Keyword);

    // '7' is at cell 22
    assert_eq!(first_row[22].glyph, "7");
    assert_eq!(first_row[22].style, TokenKind::Number);
}
