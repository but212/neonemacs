//! Startup: command-line parsing, `init.lua` loading, and file/WAL recovery (§12, I-23).

mod args;
mod init;
mod open;

pub use args::{FileArg, Invocation, USAGE, parse_args};
pub use init::{InitSource, configure, init_source, start};
pub use open::{open_files, restore_scratch};

#[cfg(test)]
fn files_of(args: &[String]) -> Vec<FileArg> {
    match parse_args(args).unwrap() {
        Invocation::Run(files) => files,
        other => panic!("expected file arguments, got {other:?}"),
    }
}
