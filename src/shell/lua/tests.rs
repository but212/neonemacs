use super::*;
use crate::core::commands;
use crate::core::commands::request_open;
use crate::core::config::Config;
use crate::core::effect::Event;
use crate::core::keymap::{Code, Key, parse_keys};
use crate::core::syntax::SyntaxRegistry;
use crate::shell::{handle, perform_all, run_source};
use mlua::{Function, Table};
use std::fs;

/// Runs `src` against `ed`. Returns the editor back with the result.
fn script(lua: &Lua, ed: Editor, src: &str) -> (Editor, mlua::Result<()>) {
    run_source(ed, lua, src)
}

/// Syntax lookups by file name, as the editor resolves a buffer's language (I-28).
fn lang_of<'a>(reg: &'a SyntaxRegistry, file: &str) -> Option<&'a str> {
    reg.find_by_filename(file).map(|d| d.name.as_str())
}

fn keyword_in(reg: &SyntaxRegistry, file: &str, word: &str) -> bool {
    reg.find_by_filename(file)
        .is_some_and(|d| d.is_keyword(word))
}

fn type_in(reg: &SyntaxRegistry, file: &str, word: &str) -> bool {
    reg.find_by_filename(file).is_some_and(|d| d.is_type(word))
}

fn comment_in<'a>(reg: &'a SyntaxRegistry, file: &str) -> Option<&'a str> {
    reg.line_comment_for_filename(file)
}

#[test]
fn word_and_comment_commands_preserve_raw_non_utf8_bytes() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let ed = ed.with_current_buffer(|b| {
        b.set_name("test.rs")
            .with_text(crate::core::text::Text::from_bytes(
                b"hello\nworld \xff tail\n",
            ))
    });

    // M-f from pos 0
    let ed = ed.with_window(|w| crate::core::editor::Window { point: 0, ..w });
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
    let ed = ed.with_window(|w| crate::core::editor::Window { point: 13, ..w });
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
fn set_mark_snaps_to_unit_boundary() {
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default())
        .with_current_buffer(|b| b.with_text(crate::core::text::Text::from_bytes("가".as_bytes())));
    let (ed, res) = script(&lua, ed, "buffer.set_mark(1)");
    res.unwrap();
    assert_eq!(ed.buffer().mark(), Some(0));

    let (ed, res) = script(&lua, ed, "buffer.set_mark(999)");
    res.unwrap();
    assert_eq!(ed.buffer().mark(), Some(3));
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
fn api_bind_and_bad_keys() {
    let lua = new_state().unwrap();
    let (ed, result) = script(
        &lua,
        Editor::new(Config::default()),
        r#"bind("C-s", "isearch-forward-regexp")"#,
    );
    result.unwrap();
    assert_eq!(
        ed.keymap().lookup(&parse_keys("C-s").unwrap()),
        Some("isearch-forward-regexp")
    );
    let (_, result) = script(&lua, ed, r#"bind("C-", "undo")"#);
    assert!(result.is_err());
}

#[test]
fn api_command_and_run() {
    let lua = new_state().unwrap();
    let (ed, result) = script(
        &lua,
        Editor::new(Config::default()),
        r#"command("wide", function() set("fill-column", 33); run("undo") end)"#,
    );
    result.unwrap();
    let ed = handle(ed, Some(&lua), Event::Run("wide".to_owned()));
    assert_eq!(ed.config().fill_column, 33);
    let (ed, result) = script(&lua, ed, r#"command("undo", function() end)"#);
    assert!(result.is_err());
    let (_, result) = script(&lua, ed, r#"run("nope")"#);
    assert!(result.is_err());
}

#[test]
fn run_from_lua_can_use_get_and_set() {
    let lua = new_state().unwrap();
    let (ed, result) = script(
        &lua,
        Editor::new(Config::default()),
        r#"command("inner", function() set("fill-column", 40) end)
           command("outer", function() run("inner"); assert(get("fill-column") == 40) end)"#,
    );
    result.unwrap();
    let ed = handle(ed, Some(&lua), Event::Run("outer".to_owned()));
    assert!(ed.message().is_empty());
    assert_eq!(ed.config().fill_column, 40);
}

#[test]
fn run_sequence_saves_before_quit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &path, None, true, Vec::new());
    let ed = perform_all(ed, None, fx);
    let ed = handle(ed, None, Event::Key(Key::plain(Code::Char('x'))));

    let (ed, result) = script(
        &lua,
        ed,
        r#"command("finish", function() run("save-buffer"); run("save-buffers-kill-emacs") end)"#,
    );
    result.unwrap();
    let ed = handle(ed, Some(&lua), Event::Run("finish".to_owned()));
    assert!(ed.quit());
    assert_eq!(fs::read(&path).unwrap(), b"xabc");
}

#[test]
fn reentrant_run_does_not_panic() {
    let lua = new_state().unwrap();
    let (ed, result) = script(
        &lua,
        Editor::new(Config::default()),
        r#"command("again", function() run("again") end)"#,
    );
    result.unwrap();
    let ed = handle(ed, Some(&lua), Event::Run("again".to_owned()));
    assert!(!ed.message().is_empty());
    assert!(!ed.quit());
}

#[test]
fn api_get_and_set_validate_input() {
    let lua = new_state().unwrap();
    let (ed, result) = script(
        &lua,
        Editor::new(Config::default()),
        r#"assert(get("fill-column") == 70)"#,
    );
    result.unwrap();
    let (ed, result) = script(&lua, ed, r#"set("case-fold-search", false)"#);
    result.unwrap();
    assert!(!ed.config().case_fold_search);
    let (ed, result) = script(&lua, ed, r#"set("tab-width", 0)"#);
    assert!(result.is_err());
    let (ed, result) = script(&lua, ed, r#"set("nope", 1)"#);
    assert!(result.is_err());
    let (_, result) = script(&lua, ed, r#"set("fill-column", "x")"#);
    assert!(result.is_err());
}
#[test]
fn api_syntax_register_and_validation() {
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default());
    let (ed, result) = script(
        &lua,
        ed,
        r##"syntax.register("mylang", {
            extensions = { "xyz" },
            line_comment = "#",
            keywords = { "fn", "var" }
        })"##,
    );
    result.unwrap();
    assert_eq!(lang_of(ed.syntax_registry(), "file.xyz"), Some("mylang"));
    assert!(keyword_in(ed.syntax_registry(), "file.xyz", "fn"));
    assert_eq!(comment_in(ed.syntax_registry(), "file.xyz"), Some("#"));

    // Validation failures
    let (_, result) = script(&lua, ed.clone(), r#"syntax.register("", {})"#);
    assert!(result.is_err());
    let (_, result) = script(
        &lua,
        ed.clone(),
        r#"syntax.register("bad", { extensions = 123 })"#,
    );
    assert!(result.is_err());
    // Missing both extensions and filenames
    let (_, result) = script(
        &lua,
        ed.clone(),
        r#"syntax.register("bad", { keywords = { "foo" } })"#,
    );
    assert!(result.is_err());

    // Extended fields: filenames, types, case_insensitive
    let (ed, result) = script(
        &lua,
        ed,
        r##"syntax.register("sql", {
            extensions = { "sql" },
            filenames = { "query.sql.txt" },
            case_insensitive = true,
            keywords = { "select" },
            types = { "varchar" }
        })"##,
    );
    result.unwrap();
    assert_eq!(lang_of(ed.syntax_registry(), "query.sql.txt"), Some("sql"));
    assert!(keyword_in(ed.syntax_registry(), "query.sql.txt", "SELECT"));
    assert!(type_in(ed.syntax_registry(), "query.sql.txt", "VARCHAR"));
}

#[test]
fn bundled_syntax_loaded_by_default() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let reg = ed.syntax_registry();
    assert_eq!(lang_of(reg, "main.rs"), Some("rust"));
    assert_eq!(lang_of(reg, "script.py"), Some("python"));
    assert_eq!(lang_of(reg, "index.js"), Some("javascript"));
    assert_eq!(lang_of(reg, "config.lua"), Some("lua"));
    assert_eq!(lang_of(reg, "app.go"), Some("go"));
    assert_eq!(lang_of(reg, "build.sh"), Some("shell"));
    assert_eq!(lang_of(reg, "README.md"), Some("markdown"));
    assert_eq!(lang_of(reg, "main.c"), Some("c"));
    assert_eq!(lang_of(reg, "Makefile"), Some("makefile"));
    assert_eq!(lang_of(reg, "Cargo.toml"), Some("toml"));
    assert_eq!(lang_of(reg, "query.sql"), Some("sql"));
    assert!(type_in(reg, "main.rs", "i32"));
    assert!(keyword_in(reg, "query.sql", "SELECT"));
    assert!(type_in(reg, "query.sql", "VARCHAR"));
    assert!(keyword_in(reg, "build.sh", "if"));
}

#[test]
fn api_unbind_removes_binding_and_rejects_invalid_keys() {
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default());
    let (ed, result) = script(&lua, ed, r#"bind("C-x 2", "split-window-below")"#);
    result.unwrap();
    let key = crate::core::keymap::parse_key_seq("C-x 2").unwrap();
    assert_eq!(ed.keymap().lookup(&key), Some("split-window-below"));

    let (ed, result) = script(&lua, ed, r#"unbind("C-x 2")"#);
    result.unwrap();
    assert_eq!(ed.keymap().lookup(&key), None);

    // Invalid key syntax produces error
    let (_, result) = script(&lua, ed, r#"unbind("C-left")"#);
    assert!(result.is_err());
}

#[test]
fn api_buffer_inspection_and_mutation() {
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default());
    let (ed, id) = ed.add_buffer(
        "test",
        crate::core::text::Text::from_bytes(b"hello world\nline two"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
        buffer: id,
        point: 0,
        ..w
    });

    let (ed, result) = script(
        &lua,
        ed,
        r#"
        assert(buffer.len() == 20)
        assert(buffer.point() == 0)
        assert(buffer.get_text() == "hello world\nline two")
        assert(buffer.get_text(0, 5) == "hello")
        assert(buffer.line_start(3) == 0)
        assert(buffer.line_end(3) == 11)
        assert(buffer.line_start(15) == 12)
        assert(buffer.width("가나") == 4)

        buffer.set_point(5)
        assert(buffer.point() == 5)

        buffer.insert(5, " beautiful")
        assert(buffer.get_text(0, 21) == "hello beautiful world")

        buffer.delete(5, 15)
        assert(buffer.get_text(0, 11) == "hello world")

        buffer.replace(0, 5, "hi")
        assert(buffer.get_text(0, 8) == "hi world")

        buffer.kill(0, 3)
        assert(buffer.get_text(0, 5) == "world")
    "#,
    );
    result.unwrap();
    assert_eq!(ed.kill_ring().entries(), &[b"hi ".to_vec()]);
}

#[test]
fn bundled_keymap_loaded_by_bootstrap() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let keymap = ed.keymap();

    // Check extended bindings defined in keymap.lua
    let k_c_k = crate::core::keymap::parse_key_seq("C-k").unwrap();
    assert_eq!(keymap.lookup(&k_c_k), Some("kill-line"));

    let k_split = crate::core::keymap::parse_key_seq("C-x 2").unwrap();
    assert_eq!(keymap.lookup(&k_split), Some("split-window-below"));

    let k_other = crate::core::keymap::parse_key_seq("C-x o").unwrap();
    assert_eq!(keymap.lookup(&k_other), Some("other-window"));

    let k_switch = crate::core::keymap::parse_key_seq("C-x b").unwrap();
    assert_eq!(keymap.lookup(&k_switch), Some("switch-to-buffer"));

    let k_kill = crate::core::keymap::parse_key_seq("C-x k").unwrap();
    assert_eq!(keymap.lookup(&k_kill), Some("kill-buffer"));

    let k_fill = crate::core::keymap::parse_key_seq("M-q").unwrap();
    assert_eq!(keymap.lookup(&k_fill), Some("fill-paragraph"));

    let k_goto = crate::core::keymap::parse_key_seq("M-g").unwrap();
    assert_eq!(keymap.lookup(&k_goto), Some("goto-line"));
}

#[test]
fn bundled_keymap_commands_all_exist() {
    let lua = new_state().unwrap();
    let _ed = bootstrap(Editor::new(Config::default()), &lua);
    let commands_table: Table = lua.globals().get(COMMANDS).unwrap();

    for line in BUNDLED_KEYMAP.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("bind(") {
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 4 {
                let cmd_name = parts[3];
                let in_builtins = commands::find(cmd_name).is_some();
                let in_lua = commands_table
                    .get::<Option<Function>>(cmd_name)
                    .unwrap()
                    .is_some();
                assert!(
                    in_builtins || in_lua,
                    "bundled keymap.lua references non-existent command: '{cmd_name}'"
                );
            }
        }
    }
}

#[test]
fn text_fill_paragraph_in_lua() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test",
        crate::core::text::Text::from_bytes(b"  hello world foo bar"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
        buffer: id,
        point: 0,
        ..w
    });
    let mut cfg = *ed.config();
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
        crate::core::text::Text::from_bytes(b"hello world neon"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
fn buffer_mark_api() {
    let lua = new_state().unwrap();
    let ed = Editor::new(Config::default())
        .with_current_buffer(|b| b.with_text(crate::core::text::Text::from_bytes(b"hello world")));
    assert_eq!(ed.buffer().mark(), None);

    // set mark to 5
    let (ed, res) = script(&lua, ed, r#"buffer.set_mark(5)"#);
    res.unwrap();
    assert_eq!(ed.buffer().mark(), Some(5));

    // query mark from lua
    let (ed, res) = script(
        &lua,
        ed,
        r#"
        local m = buffer.mark()
        assert(m == 5, "mark should be 5")
        buffer.set_mark(nil)
    "#,
    );
    res.unwrap();
    assert_eq!(ed.buffer().mark(), None);
}

#[test]
fn text_kill_word_and_backward() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);

    let (ed, id) = ed.add_buffer(
        "test_kill",
        crate::core::text::Text::from_bytes(b"hello world and neon"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
        crate::core::text::Text::from_bytes(b"hello\nworld"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
        crate::core::text::Text::from_bytes(b"hello\nworld"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
        crate::core::text::Text::from_bytes(b"hello\n\n\nworld"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
        crate::core::text::Text::from_bytes(text_bytes),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
fn comment_prefix_message_and_command_sync() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "main.rs",
        crate::core::text::Text::default(),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window { buffer: id, ..w });

    // comment_prefix for rust buffer
    let (ed, res) = script(&lua, ed, r#"assert(buffer.comment_prefix() == "//")"#);
    res.unwrap();

    // message FFI
    let (ed, res) = script(&lua, ed, r#"message("hello from lua")"#);
    res.unwrap();
    assert_eq!(ed.message(), "hello from lua");

    // command sync into Editor::commands
    let (ed, res) = script(&lua, ed, r#"command("my-test-lua-cmd", function() end)"#);
    res.unwrap();
    assert!(ed.commands().contains("my-test-lua-cmd"));
}

#[test]
fn test_comment_dwim_integration() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, id) = ed.add_buffer(
        "test.rs",
        crate::core::text::Text::from_bytes(b"    let x = 1;\n    let y = 2;"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
        crate::core::text::Text::from_bytes(b"plain markdown text"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
fn test_mark_whole_buffer() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);

    // Multi-line non-empty buffer
    let (ed, id) = ed.add_buffer(
        "test.txt",
        crate::core::text::Text::from_bytes(b"hello world\nline 2"),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
        crate::core::text::Text::from_bytes(b""),
        crate::core::file::Backing::None,
    );
    let ed = ed.with_window(|w| crate::core::editor::Window {
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
fn test_pruned_commands_unregistered() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let pruned = [
        "what-cursor-position",
        "what-line",
        "count-words",
        "count-words-region",
        "transpose-chars",
        "transpose-words",
        "open-line",
        "join-line",
        "delete-indentation",
        "just-one-space",
        "delete-horizontal-space",
        "delete-blank-lines",
        "upcase-region",
        "downcase-region",
        "kill-whole-line",
        "upcase-word",
        "downcase-word",
        "capitalize-word",
        "forward-sentence",
        "backward-sentence",
        "kill-sentence",
        "forward-paragraph",
        "backward-paragraph",
        "mark-paragraph",
        "comment-line",
    ];
    for cmd in pruned {
        assert!(
            !ed.commands().contains(cmd),
            "Command '{cmd}' should be pruned from registered commands"
        );
    }
}

#[test]
fn test_count_lines_ffi_removed() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (_ed, res) = script(
        &lua,
        ed,
        r#"
        assert(buffer.count_lines == nil, "buffer.count_lines FFI should not exist")
        "#,
    );
    res.unwrap();
}

#[test]
fn undo_limit_applies_to_existing_buffers() {
    use crate::core::undo::Edit;

    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let (ed, result) = script(&lua, ed, r#"set("undo-limit", 0)"#);
    result.unwrap();

    // Two edits in two undo groups. With limit 0 the first group is dropped at once (I-35).
    let (ed, _) = ed.apply(&Edit::Insert {
        pos: 0,
        bytes: b"a".to_vec(),
    });
    let ed = ed.end_command();
    let (ed, _) = ed.apply(&Edit::Insert {
        pos: 1,
        bytes: b"b".to_vec(),
    });
    let ed = ed.end_command();

    // Each undo is its own command, as in the event loop.
    let (ed, _) = ed.begin_command().undo(Vec::new());
    let (ed, _) = ed.begin_command().undo(Vec::new());
    assert_eq!(ed.message(), "No further undo information");
}

#[test]
fn edit_after_yank_invalidates_yank_pop() {
    let lua = new_state().unwrap();
    let ed = bootstrap(Editor::new(Config::default()), &lua);
    let ed = ed.with_kill_ring(|kr| kr.push(b"hello".to_vec()));
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
