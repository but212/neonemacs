//! Startup: command-line parsing, `init.lua` loading, and opening startup files (§12).

mod args;
mod init;
mod open;

pub use args::{FileArg, Invocation, USAGE, parse_args};
pub use init::{InitSource, configure, init_source, start};
pub use open::open_files;

#[cfg(test)]
fn files_of(args: &[String]) -> Vec<FileArg> {
    match parse_args(args).unwrap() {
        Invocation::Run(files) => files,
        other => panic!("expected file arguments, got {other:?}"),
    }
}
