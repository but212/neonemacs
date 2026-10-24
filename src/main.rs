//! Entry point: arguments, terminal setup and restore, and the event loop (§5, §12, §13).

use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use neonemacs::core::commands::{request_open, switch_to};
use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::shell::lua;
use neonemacs::shell::session::ShellSession;
use neonemacs::shell::wal;
use neonemacs::shell::{perform_all, run_source};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One file argument with the line from a preceding `+n`, if any.
#[derive(Debug, PartialEq, Eq)]
struct FileArg {
    path: PathBuf,
    line: Option<usize>,
}

/// Files and `+n` (§12). A `+n` applies to the file right after it (A6).
fn parse_args(args: &[String]) -> Result<Vec<FileArg>, String> {
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

/// Opens every file in the background. The first one that opens is shown. A `+n` sets its starting line.
/// Restores content from WAL via pure core transition if an unsaved WAL session is detected (I-23).
fn open_files(mut ed: Editor, lua: Option<&mlua::Lua>, files: &[FileArg]) -> Editor {
    let first_new = ed.buffers.len();
    let mut recovered_any = Vec::new();
    for f in files {
        let has_wal = wal::has_wal(&f.path);
        let fx = request_open(&ed, f.path.clone(), f.line, false, Vec::new());
        ed = perform_all(ed, lua, fx);
        let cur_disk = ed
            .buffers
            .iter()
            .find(|b| match &b.backing {
                neonemacs::core::file::Backing::File { path, .. } => path == &f.path,
                neonemacs::core::file::Backing::None => false,
            })
            .and_then(|b| match b.backing {
                neonemacs::core::file::Backing::File { disk, .. } => Some(disk),
                neonemacs::core::file::Backing::None => None,
            })
            .unwrap_or(neonemacs::core::file::DiskState::Unknown);
        if has_wal && let Ok(wal_bytes) = wal::read_wal(&f.path, cur_disk) {
            ed = ed.restore_buffer_content(&f.path, &wal_bytes);
            let name = f
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            recovered_any.push(name);
        }
    }
    if !recovered_any.is_empty() {
        ed.message = format!("[Recovered {} from WAL]", recovered_any.join(", "));
    }
    match ed.buffers.get(first_new).map(|b| b.id) {
        Some(id) => switch_to(ed, id),
        None => ed,
    }
}

/// Runs `init.lua` from the config directory, if there is one (U2).
fn load_init(ed: Editor, lua: &mlua::Lua) -> Editor {
    let xdg = env::var_os("XDG_CONFIG_HOME");
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    let Some(path) = lua::config_path(xdg.as_deref(), home.as_deref().map(Path::new)) else {
        return ed;
    };
    let Ok(source) = fs::read_to_string(&path) else {
        return ed;
    };
    let (mut ed, result) = run_source(ed, lua, &source);
    if let Err(e) = result {
        ed.message = format!(
            "init.lua: {}",
            e.to_string().lines().next().unwrap_or_default()
        );
    }
    ed
}

/// Puts the terminal back as it was. Safe to call more than once.
fn restore_terminal() {
    let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Restores the terminal when dropped, including on early return.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Restores the terminal before a panic message is printed (§13).
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default(info);
    }));
}

fn run(files: &[FileArg]) -> io::Result<()> {
    let ed = Editor::new(Config::default());
    let lua = lua::new_state().map_err(|e| io::Error::other(e.to_string()))?;
    let ed = load_init(ed, &lua);
    let ed = open_files(ed, Some(&lua), files);
    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    ShellSession::new(ed, lua).run(&mut io::stdout())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let files = match parse_args(&args) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("neonemacs: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&files) {
        eprintln!("neonemacs: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(ed.buffer().name, "a.txt");
        let other = ed.buffers.iter().find(|x| x.name == "b.txt").unwrap();
        assert_eq!(other.saved_point, 4);
    }
}
