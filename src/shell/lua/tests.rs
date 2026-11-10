use super::*;
use crate::core::Config;
use crate::core::effect::Event;
use crate::shell::{handle, run_source};
use mlua::Table;

/// Runs `src` against `ed`. Returns the editor back with the result.
fn script(lua: &Lua, ed: Editor, src: &str) -> (Editor, mlua::Result<()>) {
    run_source(ed, lua, "=test", src)
}

#[test]
fn mutate_editor_preserves_cell_on_error() {
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default());
    let (ed, res) = script(&lua, ed, r#"bind("C-", "invalid-key")"#);
    assert!(res.is_err());
    assert_eq!(ed.config().tab_width, 8);
}

#[test]
fn config_path_follows_xdg_then_home() {
    assert_eq!(
        config_path(Some(OsStr::new("/x")), Some(Path::new("/h"))),
        Some(PathBuf::from("/x/neonemacs/init.lua"))
    );
    assert_eq!(
        config_path(Some(OsStr::new("")), Some(Path::new("/h"))),
        Some(Path::new("/h/.config/neonemacs/init.lua").to_path_buf())
    );
    assert_eq!(config_path(None, None), None);
}

#[test]
fn only_the_allowed_libraries_are_reachable() {
    // I-57
    let lua = new_state().unwrap();
    let g = lua.globals();
    for name in [
        "io",
        "os",
        "package",
        "coroutine",
        "debug",
        "dofile",
        "loadfile",
        "require",
    ] {
        assert!(
            g.get::<mlua::Value>(name).unwrap().is_nil(),
            "{name} must not be reachable"
        );
    }
    for name in ["string", "table", "math", "utf8"] {
        assert!(g.get::<Table>(name).is_ok(), "{name} must be available");
    }
    let (_, res) = run_source(
        Editor::new(Config::default()),
        &lua,
        "=t",
        r#"assert(("x"):upper() == "X")"#,
    );
    assert!(res.is_ok());
}

#[test]
fn an_error_names_its_chunk_and_line() {
    // I-55
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default());
    let (_, res) = run_source(ed, &lua, "=init.lua", "local x = 1\nerror('boom')");
    let text = res.unwrap_err().to_string();
    assert!(text.contains("init.lua:2:"), "{text}");
    let (_, res) = run_source(Editor::new(Config::default()), &lua, "=init.lua", "x = = 1");
    let text = res.unwrap_err().to_string();
    assert!(text.contains("init.lua:1:"), "{text}");
}

// -----------------------------------------------------------------------------
// Bundled text.lua command integration tests (§6)
// -----------------------------------------------------------------------------

#[test]
fn word_and_comment_commands_preserve_raw_non_utf8_bytes() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let ed = ed.with_current_buffer(|b| {
        b.set_name("test.rs")
            .with_text(crate::core::Text::from_bytes(b"hello\nworld \xff tail\n"))
    });

    // M-f from pos 0
    let ed = ed.with_window(|w| crate::core::Window { point: 0, ..w });
    let ed = handle(ed, Some(&lua), Event::Run("forward-word".to_owned()));
    assert!(ed.message().is_empty(), "M-f failed: {}", ed.message());
    assert_eq!(ed.window().point, 5);

    // M-f again: moves past 'world'
    let ed = handle(ed, Some(&lua), Event::Run("forward-word".to_owned()));
    assert!(ed.message().is_empty(), "M-f 2 failed: {}", ed.message());
    assert_eq!(ed.window().point, 11);

    // M-f past \xff to 'tail'
    let ed = handle(ed, Some(&lua), Event::Run("forward-word".to_owned()));
    assert!(ed.message().is_empty(), "M-f 3 failed: {}", ed.message());

    // comment-dwim on the line containing \xff
    let ed = ed.with_window(|w| crate::core::Window { point: 13, ..w });
    let ed = handle(ed, Some(&lua), Event::Run("comment-dwim".to_owned()));
    assert!(
        ed.message().is_empty(),
        "comment-dwim failed: {}",
        ed.message()
    );
    assert!(
        ed.buffer().text().to_bytes().contains(&0xff),
        "Raw byte 0xff was lost or corrupted!"
    );
}

#[test]
fn text_fill_paragraph_in_lua() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test",
        crate::core::Text::from_bytes(b"  hello world foo bar"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 0,
        ..w
    });
    let mut cfg = ed.config().clone();
    cfg.fill_column = 12;
    let ed = ed.set_config(cfg);

    let (ed, res) = script(&lua, ed, r#"run("fill-paragraph")"#);
    res.unwrap();
    assert_eq!(
        ed.buffer().text().to_bytes(),
        b"  hello\n  world foo\n  bar"
    );
}

#[test]
fn text_word_and_buffer_motion() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test",
        crate::core::Text::from_bytes(b"hello world neon"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 0,
        ..w
    });

    // forward-word: 0 -> 5 ("hello"), then 5 -> 11 ("world")
    let (ed, res) = script(&lua, ed, r#"run("forward-word")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 5);

    let (ed, res) = script(&lua, ed, r#"run("forward-word")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 11);

    // backward-word: 11 -> 6 (start of "world")
    let (ed, res) = script(&lua, ed, r#"run("backward-word")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 6);

    // end-of-buffer & beginning-of-buffer
    let (ed, res) = script(&lua, ed, r#"run("end-of-buffer")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 16);

    let (ed, res) = script(&lua, ed, r#"run("beginning-of-buffer")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 0);
}

#[test]
fn text_kill_word_and_backward() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);

    let (ed, id) = ed.add_buffer(
        "test_kill",
        crate::core::Text::from_bytes(b"hello world and neon"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 0,
        ..w
    });

    // kill-word from point 0
    let (ed, res) = script(&lua, ed, r#"run("kill-word")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b" world and neon");
    assert_eq!(ed.window().point, 0);

    // move to end and backward-kill-word
    let len = ed.buffer().text().len();
    let ed = ed.move_point(len);
    let (ed, res) = script(&lua, ed, r#"run("backward-kill-word")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b" world and ");
    assert_eq!(ed.window().point, ed.buffer().text().len());
}

#[test]
fn text_word_backward_stops_at_line_start() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test_multiline",
        crate::core::Text::from_bytes(b"hello\nworld"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 11, // at end of "world"
        ..w
    });

    // backward-word: 11 -> 6 (start of "world", must not merge with "hello")
    let (ed, res) = script(&lua, ed, r#"run("backward-word")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 6);
}

#[test]
fn text_kill_word_backward_at_line_start() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test_kill_multiline",
        crate::core::Text::from_bytes(b"hello\nworld"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 11, // at end of "world"
        ..w
    });

    // backward-kill-word: should kill only "world", leaving "hello\n"
    let (ed, res) = script(&lua, ed, r#"run("backward-kill-word")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b"hello\n");
    assert_eq!(ed.window().point, 6);
}

#[test]
fn text_word_forward_across_empty_lines() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test_empty_lines",
        crate::core::Text::from_bytes(b"hello\n\n\nworld"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 5, // at end of "hello"
        ..w
    });

    // forward-word: 5 -> 13 (end of "world")
    let (ed, res) = script(&lua, ed, r#"run("forward-word")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 13);
}

#[test]
fn text_word_multibyte_backward_at_line_start() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let text_bytes = "안녕\n세상".as_bytes();
    let len = text_bytes.len();
    let (ed, id) = ed.add_buffer(
        "test_multibyte",
        crate::core::Text::from_bytes(text_bytes),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: len, // at end of "세상"
        ..w
    });

    // backward-word: should stop at start of "세상" (after "안녕\n")
    let (ed, res) = script(&lua, ed, r#"run("backward-word")"#);
    res.unwrap();
    let start_of_sesang = "안녕\n".len();
    assert_eq!(ed.window().point, start_of_sesang);
}

#[test]
fn test_comment_dwim_integration() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test.rs",
        crate::core::Text::from_bytes(b"    let x = 1;\n    let y = 2;"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 4,
        ..w
    });

    // Single line comment-dwim on indented line
    let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
    res.unwrap();
    assert_eq!(
        ed.buffer().text().to_bytes(),
        b"    // let x = 1;\n    let y = 2;"
    );

    // Uncommenting single line with comment-dwim
    let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
    res.unwrap();
    assert_eq!(
        ed.buffer().text().to_bytes(),
        b"    let x = 1;\n    let y = 2;"
    );

    // Region multi-line comment
    let ed = ed.with_current_buffer(|b| b.set_mark(Some(0)));
    let len = ed.buffer().text().len();
    let ed = ed.move_point(len);
    let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
    res.unwrap();
    assert_eq!(
        ed.buffer().text().to_bytes(),
        b"    // let x = 1;\n    // let y = 2;"
    );

    // Region multi-line uncomment
    let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
    res.unwrap();
    assert_eq!(
        ed.buffer().text().to_bytes(),
        b"    let x = 1;\n    let y = 2;"
    );

    // Missing syntax error message
    let (ed, id2) = ed.add_buffer(
        "notes.md",
        crate::core::Text::from_bytes(b"plain markdown text"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id2,
        point: 0,
        ..w
    });
    let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b"plain markdown text");
    assert_eq!(ed.message(), "No comment syntax defined for buffer");
}

#[test]
fn comment_dwim_preserves_remapped_mark_across_lines() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test.rs",
        crate::core::Text::from_bytes(b"aaa\nbbb\nccc\n"),
        crate::core::Backing::None,
    );
    let ed = crate::core::switch_to(ed, id);
    // Point at 0, mark at 8 (start of "ccc\n")
    let ed = ed
        .move_point(0)
        .with_current_buffer(|b| b.set_mark(Some(8)));
    // Comment region over lines 1 and 2
    let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
    res.unwrap();
    // In Rust mode, "// " is prepended to line 1 and line 2 (+3 bytes each = +6 bytes before mark).
    // Mark should be automatically remapped to 8 + 6 = 14.
    assert_eq!(ed.buffer().mark(), Some(14));
}

#[test]
fn test_mark_whole_buffer() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);

    // Multi-line non-empty buffer
    let (ed, id) = ed.add_buffer(
        "test.txt",
        crate::core::Text::from_bytes(b"hello world\nline 2"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 5,
        ..w
    });

    let (ed, res) = script(&lua, ed, r#"run("mark-whole-buffer")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 0);
    assert_eq!(ed.buffer().mark(), Some(18));
    assert_eq!(ed.message(), "Mark set");

    // Empty buffer
    let (ed, empty_id) = ed.add_buffer(
        "empty.txt",
        crate::core::Text::from_bytes(b""),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: empty_id,
        point: 0,
        ..w
    });
    let (ed, res) = script(&lua, ed, r#"run("mark-whole-buffer")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 0);
    assert_eq!(ed.buffer().mark(), Some(0));
    assert_eq!(ed.message(), "Mark set");
}

#[test]
fn edit_after_yank_invalidates_yank_pop() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let ed = ed.with_kill_ring(|kr| kr.push(b"hello".to_vec(), 120));
    let ed = handle(ed, Some(&lua), Event::Run("yank".to_owned()));

    // An edit after the yank means the yank is no longer the last command (I-38).
    let (ed, result) = script(&lua, ed, r#"buffer.insert(0, "  ")"#);
    result.unwrap();
    let before = ed.buffer().text().to_bytes();

    let ed = handle(ed, Some(&lua), Event::Run("yank-pop".to_owned()));
    assert_eq!(ed.buffer().text().to_bytes(), before);
    assert_eq!(ed.message(), "Previous command was not a yank");
}

#[test]
fn test_kill_line_lua_command() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);

    // 1. Mid-line kill and consecutive kills merge (I-09)
    let (ed, id) = ed.add_buffer(
        "test.txt",
        crate::core::Text::from_bytes(b"one\ntwo"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 0,
        ..w
    });

    // First kill-line: kills "one" up to newline
    let (ed, res) = script(&lua, ed, r#"run("kill-line")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b"\ntwo");
    assert_eq!(ed.window().point, 0);

    // Second kill-line: kills "\n"
    let (ed, res) = script(&lua, ed, r#"run("kill-line")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b"two");
    assert_eq!(ed.window().point, 0);

    // Consecutive kills must append into a single kill ring entry (I-09)
    assert_eq!(ed.kill_ring().entries(), &[b"one\n".to_vec()]);

    // 2. View-only protection (I-06)
    let ed = ed.with_current_buffer(|b| b.set_view_only(true));
    let (ed, res) = script(&lua, ed, r#"run("kill-line")"#);
    res.unwrap();
    assert_eq!(ed.message(), "Buffer is read-only");
    assert_eq!(ed.buffer().text().to_bytes(), b"two");
    assert_eq!(ed.kill_ring().entries(), &[b"one\n".to_vec()]);

    // 3. Boundary & empty buffer
    let ed = ed.with_current_buffer(|b| b.set_view_only(false));
    let ed = ed.with_window(|w| crate::core::Window { point: 9999, ..w });
    let (ed, res) = script(&lua, ed, r#"run("kill-line")"#);
    res.unwrap();
    assert_eq!(ed.buffer().text().to_bytes(), b"two");
}

#[test]
fn test_exchange_point_and_mark_lua_command() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);

    // 1. Without mark set: reports message
    let (ed, id) = ed.add_buffer(
        "test.txt",
        crate::core::Text::from_bytes(b"hello world"),
        crate::core::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::Window {
        buffer: id,
        point: 0,
        ..w
    });
    let (ed, res) = script(&lua, ed, r#"run("exchange-point-and-mark")"#);
    res.unwrap();
    assert_eq!(ed.message(), "No mark set in this buffer");

    // 2. With active mark
    let (ed, res) = script(&lua, ed, "buffer.set_mark(5)");
    res.unwrap();
    assert!(ed.buffer().mark_active());
    let (ed, res) = script(&lua, ed, r#"run("exchange-point-and-mark")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 5);
    assert_eq!(ed.buffer().mark(), Some(0));
    assert!(ed.buffer().mark_active());

    // 3. Inactive mark retention and reactivation (I-47)
    let ed = ed.with_current_buffer(crate::core::Buffer::deactivate_mark);
    assert!(!ed.buffer().mark_active());
    assert_eq!(ed.buffer().mark(), Some(0));
    let (ed, res) = script(&lua, ed, r#"run("exchange-point-and-mark")"#);
    res.unwrap();
    assert_eq!(ed.window().point, 0);
    assert_eq!(ed.buffer().mark(), Some(5));
    assert!(ed.buffer().mark_active());
}
