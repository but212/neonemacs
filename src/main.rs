//! Entry point: terminal setup and restore, and the event loop (§5, §12, §13).

use neonemacs::shell::session::ShellSession;
use neonemacs::shell::startup::{
    FileArg, Invocation, USAGE, init_source, open_files, parse_args, restore_scratch, start,
};
use neonemacs::shell::terminal::TerminalGuard;
use neonemacs::shell::wal;
use std::env;
use std::io::{self, Write};

fn run(files: &[FileArg]) -> io::Result<()> {
    let (ed, lua) =
        start(env::current_dir()?, init_source()).map_err(|e| io::Error::other(e.to_string()))?;
    let ed = restore_scratch(ed.with_scratch_slot(wal::scratch_path()));
    let ed = open_files(ed, Some(&lua), files);
    let _guard = TerminalGuard::enter()?;
    ShellSession::new(ed, lua).run(&mut io::stdout())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Invocation::Run(files)) => {
            if let Err(e) = run(&files) {
                eprintln!("neonemacs: {e}");
                std::process::exit(1);
            }
        }
        Ok(Invocation::Version) => {
            let _ = writeln!(io::stdout(), "neonemacs {}", env!("CARGO_PKG_VERSION"));
        }
        Ok(Invocation::Help) => {
            let _ = write!(io::stdout(), "{USAGE}");
        }
        Err(e) => {
            eprintln!("neonemacs: {e}");
            std::process::exit(2);
        }
    }
}
