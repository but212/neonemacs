//! Entry point: terminal setup and restore, and the event loop (§5, §12, §13).

use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::shell::lua;
use neonemacs::shell::session::ShellSession;
use neonemacs::shell::startup::{FileArg, load_init, open_files, parse_args};
use std::env;
use std::io;
use std::sync::Once;

/// Puts the terminal back as it was. Safe to call more than once.
fn restore_terminal() {
    let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Restores the terminal when dropped, including on early return or panic (§13).
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        Self::install_panic_hook();
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }

    fn install_panic_hook() {
        static PANIC_HOOK: Once = Once::new();
        PANIC_HOOK.call_once(|| {
            let default = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                default(info);
            }));
        });
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn run(files: &[FileArg]) -> io::Result<()> {
    let lua = lua::new_state().map_err(|e| io::Error::other(e.to_string()))?;
    let ed = Editor::new(Config::default());
    let ed = lua::bootstrap(ed, &lua);
    let ed = load_init(ed, &lua);
    let ed = open_files(ed, Some(&lua), files);
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
