//! CLI argument parsing, `init.lua` loading, and startup file/WAL recovery (§12, I-23).

use crate::core::buffer::Buffer;
use crate::core::commands::{request_open, switch_to};
use crate::core::editor::Editor;
use crate::core::file::DiskState;
use crate::shell::{lua, perform_all, run_source, wal};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// One file argument with the line from a preceding `+n`, if any.
#[derive(Debug, PartialEq, Eq)]
pub struct FileArg {
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// Files and `+n` (§12). A `+n` applies to the file right after it (A6).
pub fn parse_args(args: &[String]) -> Result<Vec<FileArg>, String> {
    let mut files = Vec::new();
    let mut line = None;
    for arg in args {
        if let Some(n) = arg.strip_prefix('+') {
            let n = n
                .parse::<usize>()
                .ok()
                .filter(|&n| n >= 1)
                .ok_or_else(|| format!("invalid line number: {arg}"))?;
            line = Some(n);
        } else {
            files.push(FileArg {
                path: PathBuf::from(arg),
                line: line.take(),
            });
        }
    }
    if line.is_some() {
        return Err("+n must be followed by a file".to_owned());
    }
    Ok(files)
}

/// Queries the current disk state of the buffer backing the given path.
pub fn buffer_disk_state(ed: &Editor, path: &Path) -> DiskState {
    ed.buffers()
        .find_by_path(path)
        .map_or(DiskState::Unknown, |b| b.backing().disk())
}

/// Opens a single file and restores unsaved WAL content if available (I-23).
/// Returns the updated editor and the file name if restored from WAL.
pub fn open_and_recover_single(
    ed: Editor,
    lua: Option<&mlua::Lua>,
    arg: &FileArg,
) -> (Editor, Option<String>) {
    let has_wal = wal::has_wal(&arg.path);
    let fx = request_open(&ed, arg.path.clone(), arg.line, false, Vec::new());
    let mut ed = perform_all(ed, lua, fx);

    // A refused open (over `max-file-size`) leaves no buffer, so there is nothing to recover into.
    if has_wal && ed.buffers().find_by_path(&arg.path).is_some() {
        let cur_disk = buffer_disk_state(&ed, &arg.path);
        if let Ok(recovered) = wal::read_wal(&arg.path, cur_disk) {
            ed = ed.restore_buffer_content(&arg.path, &recovered.bytes, recovered.eol, arg.line);
            let name = arg
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            return (ed, Some(name));
        }
    }

    (ed, None)
}

/// Opens every file in the background. The first one that opens is shown. A `+n` sets its starting line.
/// Restores content from WAL via pure core transition if an unsaved WAL session is detected (I-23).
pub fn open_files(mut ed: Editor, lua: Option<&mlua::Lua>, files: &[FileArg]) -> Editor {
    let first_new = ed.buffers().len();
    let mut recovered = Vec::new();

    for f in files {
        let (next_ed, rec_name) = open_and_recover_single(ed, lua, f);
        ed = next_ed;
        if let Some(name) = rec_name {
            recovered.push(name);
        }
    }

    if !recovered.is_empty() {
        ed = ed.set_message(format!("[Recovered {} from WAL]", recovered.join(", ")));
    }

    match ed.buffers().get(first_new).map(Buffer::id) {
        Some(id) => switch_to(ed, id),
        None => ed,
    }
}

/// Runs `init.lua` from the config directory, if there is one (U2).
pub fn load_init_from(
    ed: Editor,
    lua: &mlua::Lua,
    xdg: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Editor {
    let Some(path) = lua::config_path(xdg, home) else {
        return ed;
    };
    let Ok(source) = fs::read_to_string(&path) else {
        return ed;
    };
    let (ed, result) = run_source(ed, lua, &source);
    if let Err(e) = result {
        let msg = format!(
            "init.lua: {}",
            e.to_string().lines().next().unwrap_or_default()
        );
        return ed.set_message(msg);
    }
    ed
}

pub fn load_init(ed: Editor, lua: &mlua::Lua) -> Editor {
    let xdg = env::var_os("XDG_CONFIG_HOME");
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    load_init_from(ed, lua, xdg.as_deref(), home.as_deref().map(Path::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use crate::core::eol::Eol;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn args() {
        let files = parse_args(&strings(&["a.txt", "+3", "b.txt", "c.txt"])).unwrap();
        assert_eq!(
            files,
            vec![
                FileArg {
                    path: PathBuf::from("a.txt"),
                    line: None
                },
                FileArg {
                    path: PathBuf::from("b.txt"),
                    line: Some(3)
                },
                FileArg {
                    path: PathBuf::from("c.txt"),
                    line: None
                },
            ]
        );
    }

    #[test]
    fn first_file_is_shown_and_line_applies_to_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"a\n").unwrap();
        fs::write(&b, b"1\n2\n3\n4\n").unwrap();
        let args = [
            a.display().to_string(),
            "+3".to_owned(),
            b.display().to_string(),
        ];
        let files = parse_args(&args).unwrap();

        let ed = open_files(Editor::new(Config::default()), None, &files);
        assert_eq!(ed.buffer().name(), "a.txt");
        let other = ed.buffers().iter().find(|x| x.name() == "b.txt").unwrap();
        assert_eq!(other.saved_point(), 4);
    }

    #[test]
    fn args_adversarial_rejections_and_boundaries() {
        assert_eq!(parse_args(&[]).unwrap(), vec![]);
        assert_eq!(
            parse_args(&strings(&["+1", "file.txt"])).unwrap(),
            vec![FileArg {
                path: PathBuf::from("file.txt"),
                line: Some(1)
            }]
        );
        // Rejections:
        assert!(parse_args(&strings(&["+0", "a"])).is_err());
        assert!(parse_args(&strings(&["+-1", "a"])).is_err());
        assert!(parse_args(&strings(&["+abc", "a"])).is_err());
        assert!(parse_args(&strings(&["+", "a"])).is_err());
        assert!(parse_args(&strings(&["+  1", "a"])).is_err());
        assert!(parse_args(&strings(&["+18446744073709551616", "a"])).is_err());
        assert!(parse_args(&strings(&["+3"])).is_err());
        assert!(parse_args(&strings(&["a", "+3"])).is_err());
    }

    #[test]
    fn open_files_empty_arguments() {
        let ed = open_files(Editor::new(Config::default()), None, &[]);
        assert_eq!(ed.buffers().len(), 1);
        assert_eq!(ed.buffer().name(), "*scratch*");
        assert!(ed.message().is_empty());
    }

    #[test]
    fn open_files_wal_recovery_multiple_and_message() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        fs::write(&path_a, "disk a").unwrap();
        fs::write(&path_b, "disk b").unwrap();

        let disk_a = crate::shell::file::disk_state(&path_a);
        let disk_b = crate::shell::file::disk_state(&path_b);

        wal::write_wal(&path_a, disk_a, Eol::Lf, b"recovered a").unwrap();
        wal::write_wal(&path_b, disk_b, Eol::Lf, b"recovered b").unwrap();

        let files = vec![
            FileArg {
                path: path_a.clone(),
                line: None,
            },
            FileArg {
                path: path_b.clone(),
                line: None,
            },
        ];

        let ed = open_files(Editor::new(Config::default()), None, &files);
        assert_eq!(ed.buffer().name(), "a.txt");
        assert_eq!(ed.buffer().text().to_bytes(), b"recovered a");
        assert!(ed.buffer().modified());

        let buf_b = ed.buffers().iter().find(|b| b.name() == "b.txt").unwrap();
        assert_eq!(buf_b.text().to_bytes(), b"recovered b");
        assert!(buf_b.modified());

        assert_eq!(ed.message(), "[Recovered a.txt, b.txt from WAL]");
    }

    #[test]
    fn open_files_wal_corruption_and_external_modification_resilience() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.txt");
        fs::write(&path, "original content").unwrap();

        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Lf, b"wal draft").unwrap();

        // 1. External change after WAL: modified on disk
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "external change from git").unwrap();

        let files = vec![FileArg {
            path: path.clone(),
            line: None,
        }];
        let ed = open_files(Editor::new(Config::default()), None, &files);
        // Stale WAL must not be recovered; disk content loaded
        assert_eq!(ed.buffer().text().to_bytes(), b"external change from git");
        assert!(!ed.buffer().modified());
        assert!(ed.message().is_empty());

        // 2. Corrupted WAL file (invalid bytes in WAL path)
        let corrupt_path = dir.path().join("bad_wal.txt");
        fs::write(&corrupt_path, "good disk").unwrap();
        let cur_disk = crate::shell::file::disk_state(&corrupt_path);
        wal::write_wal(&corrupt_path, cur_disk, Eol::Lf, b"wal draft").unwrap();
        let wal_file = wal::wal_path(&corrupt_path);
        // Overwrite WAL with garbage
        fs::write(&wal_file, b"GARBAGE_NOT_A_VALID_WAL_FORMAT").unwrap();

        let files2 = vec![FileArg {
            path: corrupt_path.clone(),
            line: None,
        }];
        let ed2 = open_files(Editor::new(Config::default()), None, &files2);
        assert_eq!(ed2.buffer().text().to_bytes(), b"good disk");
        assert!(!ed2.buffer().modified());
        assert!(ed2.message().is_empty());
    }

    #[test]
    fn load_init_missing_and_adversarial_lua_handling() {
        let lua = lua::new_state().unwrap();
        let ed = Editor::new(Config::default());

        // 1. Missing / None paths
        let ed = load_init_from(ed, &lua, None, None);
        assert!(ed.message().is_empty());

        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("neonemacs");
        fs::create_dir_all(&config_dir).unwrap();
        let init_file = config_dir.join("init.lua");

        // 2. Valid Lua script modifying config
        fs::write(&init_file, r#"set("fill-column", 85)"#).unwrap();
        let ed = load_init_from(ed, &lua, Some(dir.path().as_os_str()), None);
        assert_eq!(ed.config().fill_column, 85);
        assert!(ed.message().is_empty());

        // 3. Syntax error in init.lua
        fs::write(&init_file, "function bad_syntax(").unwrap();
        let ed = load_init_from(ed, &lua, Some(dir.path().as_os_str()), None);
        assert!(ed.message().starts_with("init.lua: "));

        // 4. Runtime error in init.lua
        fs::write(&init_file, r#"error("adversarial error")"#).unwrap();
        let ed = load_init_from(ed, &lua, Some(dir.path().as_os_str()), None);
        assert!(ed.message().starts_with("init.lua: "));
        assert!(ed.message().contains("adversarial error"));
    }

    #[test]
    fn buffer_disk_state_matches_known_and_unknown() {
        let ed = Editor::new(Config::default());
        let unknown = buffer_disk_state(&ed, Path::new("nonexistent.txt"));
        assert_eq!(unknown, DiskState::Unknown);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracked.txt");
        fs::write(&path, "tracked content").unwrap();

        let fx = request_open(&ed, path.clone(), None, false, Vec::new());
        let ed = perform_all(ed, None, fx);
        let disk = buffer_disk_state(&ed, &path);
        assert!(matches!(disk, DiskState::Known { .. }));
    }

    #[test]
    fn open_and_recover_single_isolated_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.txt");
        fs::write(&path, "initial").unwrap();

        let ed = Editor::new(Config::default());
        let arg = FileArg {
            path: path.clone(),
            line: None,
        };

        // Without WAL: returns (ed, None) and opens in background
        let (ed, rec) = open_and_recover_single(ed, None, &arg);
        assert_eq!(rec, None);
        let buf = ed
            .buffers()
            .iter()
            .find(|b| b.name() == "single.txt")
            .unwrap();
        assert_eq!(buf.text().to_bytes(), b"initial");
        assert!(!buf.modified());

        // With WAL: returns (ed, Some("single.txt")) and recovers text
        let cur_disk = buffer_disk_state(&ed, &path);
        wal::write_wal(&path, cur_disk, Eol::Lf, b"wal draft").unwrap();
        let (ed2, rec2) = open_and_recover_single(Editor::new(Config::default()), None, &arg);
        assert_eq!(rec2, Some("single.txt".to_string()));
        let buf2 = ed2
            .buffers()
            .iter()
            .find(|b| b.name() == "single.txt")
            .unwrap();
        assert_eq!(buf2.text().to_bytes(), b"wal draft");
        assert!(buf2.modified());
    }

    #[test]
    fn oversized_file_is_not_recovered_from_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        fs::write(&path, "0123456789").unwrap();
        wal::write_wal(
            &path,
            crate::shell::file::disk_state(&path),
            Eol::Lf,
            b"wal draft",
        )
        .unwrap();

        let ed = Editor::new(Config {
            max_file_size: 4,
            ..Config::default()
        });
        let arg = FileArg {
            path: path.clone(),
            line: None,
        };
        let (ed, rec) = open_and_recover_single(ed, None, &arg);

        // The open is refused, so there is no buffer to recover into and no `[Recovered]` notice.
        assert_eq!(rec, None);
        assert!(ed.buffers().find_by_path(&path).is_none());
    }

    #[test]
    fn wal_recovery_restores_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eol.txt");
        fs::write(&path, "a\nb\n").unwrap();
        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Crlf, b"a\nb\nc\n").unwrap();

        let arg = FileArg {
            path: path.clone(),
            line: None,
        };
        let (ed, rec) = open_and_recover_single(Editor::new(Config::default()), None, &arg);

        assert!(rec.is_some());
        let buf = ed.buffers().find_by_path(&path).unwrap();
        assert_eq!(buf.eol(), Eol::Crlf);
        assert_eq!(buf.text().to_bytes(), b"a\nb\nc\n");
    }
}
