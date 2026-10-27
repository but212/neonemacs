//! Command-line parsing (§12). Pure: no I/O, so every rule is tested here.

use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: neonemacs [OPTION] [+LINE] [FILE]...

  -v, --version  print the version and exit
  -h, --help     print this help and exit
  --             treat the following arguments as files
  +LINE FILE     open FILE at LINE
";

/// One file argument with the line from a preceding `+n`, if any.
#[derive(Debug, PartialEq, Eq)]
pub struct FileArg {
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// What the command line asks for (§12). `Version` and `Help` never start the editor.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Run(Vec<FileArg>),
    Version,
    Help,
}

/// Arguments in order (§12). A `+n` applies to the file right after it (A6).
/// The first `-v` or `-h` returns at once, so an earlier argument error still wins.
pub fn parse_args(args: &[String]) -> Result<Invocation, String> {
    let mut files = Vec::new();
    let mut line = None;
    let mut options_open = true;
    for arg in args {
        if let Some(n) = arg.strip_prefix('+') {
            let n = n
                .parse::<usize>()
                .ok()
                .filter(|&n| n >= 1)
                .ok_or_else(|| format!("invalid line number: {arg}"))?;
            line = Some(n);
        } else if options_open && arg == "--" {
            options_open = false;
        } else if options_open && arg.starts_with('-') {
            return match arg.as_str() {
                "-v" | "--version" => Ok(Invocation::Version),
                "-h" | "--help" => Ok(Invocation::Help),
                _ => Err(format!("unknown option: {arg}")),
            };
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
    Ok(Invocation::Run(files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::startup::files_of;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn args() {
        let files = files_of(&strings(&["a.txt", "+3", "b.txt", "c.txt"]));
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
    fn args_adversarial_rejections_and_boundaries() {
        assert_eq!(files_of(&[]), vec![]);
        assert_eq!(
            files_of(&strings(&["+1", "file.txt"])),
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
    fn version_and_help_flags() {
        assert_eq!(parse_args(&strings(&["-v"])).unwrap(), Invocation::Version);
        assert_eq!(
            parse_args(&strings(&["--version"])).unwrap(),
            Invocation::Version
        );
        assert_eq!(parse_args(&strings(&["-h"])).unwrap(), Invocation::Help);
        assert_eq!(parse_args(&strings(&["--help"])).unwrap(), Invocation::Help);
    }

    #[test]
    fn first_version_or_help_flag_wins_even_after_files() {
        assert_eq!(
            parse_args(&strings(&["a.txt", "-v"])).unwrap(),
            Invocation::Version
        );
        assert_eq!(
            parse_args(&strings(&["-v", "-h"])).unwrap(),
            Invocation::Version
        );
        assert_eq!(
            parse_args(&strings(&["-h", "-v"])).unwrap(),
            Invocation::Help
        );
    }

    #[test]
    fn earlier_argument_error_wins_over_flag() {
        assert_eq!(
            parse_args(&strings(&["+0", "-v"])).unwrap_err(),
            "invalid line number: +0"
        );
    }

    #[test]
    fn unknown_options_are_errors() {
        for arg in ["-x", "--foo", "-", "--versoin"] {
            assert_eq!(
                parse_args(&strings(&[arg])).unwrap_err(),
                format!("unknown option: {arg}")
            );
        }
        assert_eq!(
            parse_args(&strings(&["a.txt", "-x"])).unwrap_err(),
            "unknown option: -x"
        );
    }

    #[test]
    fn double_dash_makes_later_dash_arguments_files() {
        assert_eq!(
            files_of(&strings(&["--", "-x", "-v", "--", "-"])),
            vec![
                FileArg {
                    path: PathBuf::from("-x"),
                    line: None
                },
                FileArg {
                    path: PathBuf::from("-v"),
                    line: None
                },
                FileArg {
                    path: PathBuf::from("--"),
                    line: None
                },
                FileArg {
                    path: PathBuf::from("-"),
                    line: None
                },
            ]
        );
        // A pending `+n` before `--` still applies to the next file.
        assert_eq!(
            files_of(&strings(&["+3", "--", "-x"])),
            vec![FileArg {
                path: PathBuf::from("-x"),
                line: Some(3)
            }]
        );
        assert_eq!(
            files_of(&strings(&["--", "+3", "f"])),
            vec![FileArg {
                path: PathBuf::from("f"),
                line: Some(3)
            }]
        );
        // `--` does not hide the `+n` rules.
        assert_eq!(
            parse_args(&strings(&["--", "+0", "f"])).unwrap_err(),
            "invalid line number: +0"
        );
        assert_eq!(
            parse_args(&strings(&["+3", "--"])).unwrap_err(),
            "+n must be followed by a file"
        );
    }
}
