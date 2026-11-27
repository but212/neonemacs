//! Reads `init.lua` from the config directory and builds the configured editor (U2).
//!
//! The file is classified once, in `read_init` (I-55). `configure` then applies it whole or not at
//! all (I-54): a Lua state cannot be rolled back, so a failed run is thrown away and the bundled
//! state is built again.

use crate::core::Buffer;
use crate::core::Config;
use crate::core::Editor;
use crate::core::effect::InitLoad;
use crate::core::record_init_error;
use crate::shell::{lua, run_source};
use mlua::Lua;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// What was found at the `init.lua` path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitSource {
    /// No config directory is known, or the file does not exist.
    Absent,
    /// The file exists but cannot be used: an I/O error, or bytes that are not UTF-8.
    Unreadable {
        reason: String,
    },
    Loaded(String),
}

/// Reads `init.lua` from the path that `xdg` and `home` give. Only a missing file is `Absent`.
fn read_init(xdg: Option<&OsStr>, home: Option<&Path>) -> InitSource {
    let Some(path) = crate::shell::xdg_path(xdg, home, &[".config"], "init.lua") else {
        return InitSource::Absent;
    };
    match fs::read(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => InitSource::Absent,
        Err(e) => InitSource::Unreadable {
            reason: format!("cannot read {}: {e}", path.display()),
        },
        Ok(bytes) => String::from_utf8(bytes).map_or_else(
            |_| InitSource::Unreadable {
                reason: format!("{} is not valid UTF-8", path.display()),
            },
            InitSource::Loaded,
        ),
    }
}

/// Reads `init.lua` from the path that the environment gives.
#[must_use]
pub fn init_source() -> InitSource {
    let xdg = env::var_os("XDG_CONFIG_HOME");
    let home = crate::shell::home_dir();
    read_init(xdg.as_deref(), home.as_deref())
}

/// The editor and Lua state with only the bundled scripts applied.
fn bundled(cwd: PathBuf) -> mlua::Result<(Editor, Lua)> {
    let cwd = cwd
        .into_os_string()
        .into_string()
        .map_err(|_| mlua::Error::external("working directory path is not valid UTF-8"))?;
    let lua = lua::new_state()?;
    let ed = lua::bootstrap(Editor::new(Config::default()).with_cwd(cwd), &lua);
    Ok((ed, lua))
}

/// The `init.lua:<line>:` that `line` holds, if any.
fn position(line: &str) -> Option<&str> {
    const CHUNK: &str = "init.lua:";
    let start = line.find(CHUNK)?;
    let rest = &line[start + CHUNK.len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && rest[digits..].starts_with(':'))
        .then(|| &line[start..=start + CHUNK.len() + digits])
}

/// The whole text of `e`, with a first line that names `init.lua:<line>:` (I-55). An error that
/// an API function returns holds its line only in the traceback, so the innermost `init.lua`
/// frame is put in front.
fn located(e: &mlua::Error) -> String {
    let text = e.to_string();
    let mut lines = text.lines();
    if lines.next().is_some_and(|first| position(first).is_some()) {
        return text;
    }
    match lines.find_map(position) {
        Some(at) => format!("{at} {text}"),
        None => text,
    }
}

/// Gives the scratch buffer the configured `default-eol`. It exists before `init.lua` runs.
/// A scratch buffer that `init.lua` edited keeps the line ending the edits left.
fn scratch_takes_default_eol(ed: Editor) -> Editor {
    let eol = ed.config().default_eol;
    let scratch = ed
        .buffers()
        .iter()
        .find(|b| b.backing().is_scratch())
        .map(Buffer::id);
    match scratch {
        Some(id) => ed.with_buffer(id, |b| if b.modified() { b } else { b.set_eol(eol) }),
        None => ed,
    }
}

/// Builds the editor for `source`, with the Lua state that holds its commands.
///
/// The settings are the bundled scripts plus all of `init.lua`, or the bundled scripts alone: a
/// source that fails leaves no trace in the result (I-54).
///
/// # Errors
/// Returns `mlua::Error` if a Lua state cannot be created.
pub fn configure(cwd: PathBuf, source: InitSource) -> mlua::Result<(Editor, Lua, InitLoad)> {
    let (ed, lua) = bundled(cwd.clone())?;
    let text = match source {
        InitSource::Absent => return Ok((ed, lua, InitLoad::Absent)),
        InitSource::Unreadable { reason } => return Ok((ed, lua, InitLoad::Rejected(reason))),
        InitSource::Loaded(text) => text,
    };
    let (ed, result) = run_source(ed, &lua, "=init.lua", &text);
    match result {
        Ok(()) => Ok((scratch_takes_default_eol(ed), lua, InitLoad::Applied)),
        Err(e) => {
            let (ed, lua) = bundled(cwd)?;
            Ok((ed, lua, InitLoad::Rejected(located(&e))))
        }
    }
}

/// Builds the editor a session starts with. A rejected `init.lua` is logged and echoed (I-55).
///
/// # Errors
/// Returns `mlua::Error` if a Lua state cannot be created.
pub fn start(cwd: PathBuf, source: InitSource) -> mlua::Result<(Editor, Lua)> {
    let (ed, lua, load) = configure(cwd, source)?;
    let ed = match load {
        InitLoad::Rejected(text) => record_init_error(ed, &text),
        InitLoad::Absent | InitLoad::Applied | InitLoad::Ignored => ed,
    };
    Ok((ed, lua))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Eol;
    use crate::core::parse_key_seq;

    fn loaded(src: &str) -> (Editor, Lua, InitLoad) {
        configure(PathBuf::new(), InitSource::Loaded(src.to_owned())).unwrap()
    }

    fn bound(ed: &Editor, keys: &str) -> Option<String> {
        ed.keymap()
            .lookup(&parse_key_seq(keys).unwrap())
            .map(str::to_owned)
    }

    #[test]
    fn only_a_missing_file_is_absent() {
        // I-55
        assert_eq!(read_init(None, None), InitSource::Absent);

        let dir = tempfile::tempdir().unwrap();
        let xdg = Some(dir.path().as_os_str());
        assert_eq!(read_init(xdg, None), InitSource::Absent);

        let config_dir = dir.path().join("neonemacs");
        fs::create_dir_all(&config_dir).unwrap();
        let init_file = config_dir.join("init.lua");
        assert_eq!(read_init(xdg, None), InitSource::Absent);

        fs::write(&init_file, "set(\"fill-column\", 85)").unwrap();
        assert_eq!(
            read_init(xdg, None),
            InitSource::Loaded("set(\"fill-column\", 85)".to_owned())
        );

        // CP949 bytes of a Korean comment.
        fs::write(&init_file, b"-- \xc7\xd1\xb1\xdb\n").unwrap();
        let InitSource::Unreadable { reason } = read_init(xdg, None) else {
            panic!("non-UTF-8 bytes must be unreadable");
        };
        assert!(reason.ends_with("is not valid UTF-8"), "{reason}");

        fs::remove_file(&init_file).unwrap();
        fs::create_dir(&init_file).unwrap();
        let InitSource::Unreadable { reason } = read_init(xdg, None) else {
            panic!("a directory must be unreadable");
        };
        assert!(reason.starts_with("cannot read "), "{reason}");
    }

    #[test]
    fn a_source_applies_whole() {
        let (ed, _, load) = loaded("set(\"fill-column\", 85)\nbind(\"C-x 9\", \"forward-char\")");
        assert_eq!(load, InitLoad::Applied);
        assert_eq!(ed.config().fill_column, 85);
        assert_eq!(bound(&ed, "C-x 9").as_deref(), Some("forward-char"));
        assert_eq!(ed.message(), "");
    }

    fn first_line_of_rejection(src: &str) -> String {
        let (_, _, load) = loaded(src);
        let InitLoad::Rejected(text) = load else {
            panic!("{src} must be rejected");
        };
        text.lines().next().unwrap_or_default().to_owned()
    }

    #[test]
    fn every_lua_error_names_its_line_on_the_first_line() {
        // I-55: an error that an API function returns carries its line only in the traceback.
        let first = first_line_of_rejection("local x = 1\nset(\"nope\", 1)");
        assert!(first.starts_with("init.lua:2: "), "{first}");
        assert!(first.contains("unknown setting: nope"), "{first}");

        // The innermost `init.lua` frame is the one named.
        let first =
            first_line_of_rejection("local function f()\n  bind(\"bogus\", \"x\")\nend\nf()");
        assert!(first.starts_with("init.lua:2: "), "{first}");

        // An error that already names its line is left as it is.
        let first = first_line_of_rejection("local x = 1\nerror(\"boom\")");
        assert_eq!(first.matches("init.lua:2:").count(), 1, "{first}");
    }

    #[test]
    fn a_failing_source_applies_nothing() {
        // I-54: the statements before the error are not kept.
        let (ed, lua, load) = loaded(
            "set(\"fill-column\", 85)\nbind(\"C-x 9\", \"forward-char\")\n\
             command(\"mine\", function() end)\nerror(\"boom\")",
        );
        let InitLoad::Rejected(text) = load else {
            panic!("a runtime error must be rejected");
        };
        assert!(text.contains("init.lua:4:"), "{text}");
        assert!(text.contains("boom"), "{text}");
        assert_eq!(ed.config().fill_column, 70);
        assert_eq!(bound(&ed, "C-x 9"), None);
        assert!(lua::call_lua(&lua, "mine").is_err());
        // The bundled scripts are still there.
        assert_eq!(bound(&ed, "C-x 2").as_deref(), Some("split-window-below"));

        let (ed, _, load) = loaded("set(\"fill-column\", 85)\nfunction bad_syntax(");
        assert!(matches!(load, InitLoad::Rejected(text) if text.contains("init.lua:2:")));
        assert_eq!(ed.config().fill_column, 70);
    }

    #[test]
    fn an_absent_or_unreadable_source_gives_the_bundled_settings() {
        let (ed, _, load) = configure(PathBuf::new(), InitSource::Absent).unwrap();
        assert_eq!(load, InitLoad::Absent);
        assert_eq!(bound(&ed, "C-x 2").as_deref(), Some("split-window-below"));

        let source = InitSource::Unreadable {
            reason: "cannot read x".to_owned(),
        };
        let (ed, _, load) = configure(PathBuf::new(), source).unwrap();
        assert_eq!(load, InitLoad::Rejected("cannot read x".to_owned()));
        assert_eq!(bound(&ed, "C-x 2").as_deref(), Some("split-window-below"));
    }

    #[test]
    fn default_eol_reaches_the_scratch_buffer() {
        let (ed, _, _) = loaded("set(\"default-eol\", \"crlf\")");
        assert_eq!(ed.buffer().name(), "*scratch*");
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert!(!ed.buffer().modified());
    }

    #[test]
    fn default_eol_does_not_undo_an_eol_that_init_set_on_scratch() {
        let (ed, _, load) = loaded("run(\"toggle-eol\")");
        assert_eq!(load, InitLoad::Applied);
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
    }

    #[test]
    fn file_and_process_libraries_are_not_reachable_from_init() {
        // I-57
        for src in ["io.open(\"x\")", "os.execute(\"x\")", "dofile(\"x\")"] {
            let (_, _, load) = loaded(src);
            assert!(matches!(load, InitLoad::Rejected(_)), "{src}");
        }
    }

    #[test]
    fn start_logs_a_rejected_init_and_stays_silent_otherwise() {
        // I-55
        let rejected = InitSource::Loaded("local x = 1\nerror(\"boom\")".to_owned());
        let (ed, _) = start(PathBuf::new(), rejected).unwrap();
        assert!(ed.message().starts_with("init.lua: "), "{}", ed.message());
        assert!(ed.message().contains("init.lua:2:"), "{}", ed.message());
        assert!(!ed.message().contains('\n'));
        let log = ed.buffers().find_by_name("*init-log*").unwrap();
        let log = String::from_utf8(crate::test_support::doc_bytes(log.text())).unwrap();
        assert!(log.contains("init.lua:2:") && log.contains("boom"), "{log}");
        assert_eq!(ed.buffer().name(), "*scratch*");

        let unreadable = InitSource::Unreadable {
            reason: "cannot read x".to_owned(),
        };
        let (ed, _) = start(PathBuf::new(), unreadable).unwrap();
        assert_eq!(ed.message(), "init.lua: cannot read x");

        for quiet in [InitSource::Absent, InitSource::Loaded(String::new())] {
            let (ed, _) = start(PathBuf::new(), quiet).unwrap();
            assert_eq!(ed.message(), "");
            assert!(ed.buffers().find_by_name("*init-log*").is_none());
        }
    }

    #[test]
    fn position_requires_digits_followed_by_colon() {
        assert_eq!(position("init.lua:12: boom"), Some("init.lua:12:"));
        assert_eq!(position("init.lua:: boom"), None);
        assert_eq!(position("init.lua:12x boom"), None);
    }
}
