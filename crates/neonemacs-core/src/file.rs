//! Buffer backing, and the buffer that a file read produces. Pure: the bytes come from the shell.

use crate::Doc;
use crate::eol::{Eol, EolStreamDecoder};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    Known { mtime: Duration, size: u64 },
    Unknown,
}

/// The absolute path of a file. `resolve` is its only constructor, so two buffers visiting the
/// same file hold equal values (I-37).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePath(String);

fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// Splits a path into `(prefix, has_root, rest)`.
/// Recognizes Windows drive (`C:`) and UNC (`\\server\share` / `//server/share`) prefixes.
fn split_prefix(path: &str) -> (&str, bool, &str) {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && is_sep(bytes[0]) && is_sep(bytes[1]) && !is_sep(bytes[2]) {
        // UNC: `\\server\share` or `//server/share`
        let after_slashes = &path[2..];
        if let Some(s_end) = after_slashes.find(['/', '\\']) {
            let after_server = &after_slashes[s_end + 1..];
            let share_end = after_server.find(['/', '\\']).unwrap_or(after_server.len());
            let prefix_len = 2 + s_end + 1 + share_end;
            let rest = &path[prefix_len..];
            let rest = rest.strip_prefix(['/', '\\']).unwrap_or(rest);
            return (&path[..prefix_len], true, rest);
        }
        return (path, true, "");
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let rest = &path[2..];
        let has_root = rest.as_bytes().first().is_some_and(|&b| is_sep(b));
        let rest = if has_root { &rest[1..] } else { rest };
        return (&path[..2], has_root, rest);
    }
    let has_root = bytes.first().is_some_and(|&b| is_sep(b));
    let rest = if has_root { &path[1..] } else { path };
    ("", has_root, rest)
}

fn push_segments<'a>(mut segments: Vec<&'a str>, rest: &'a str, anchored: bool) -> Vec<&'a str> {
    for seg in rest.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|&s| s != "..") {
                    segments.pop();
                } else if !anchored {
                    segments.push("..");
                }
            }
            s => segments.push(s),
        }
    }
    segments
}

impl FilePath {
    /// Joins `input` onto `cwd` and folds the result lexically: `.` is dropped and `..` removes the
    /// component before it. A relative path keeps the `..` it cannot fold. No link is followed, so
    /// the function is pure (I-14).
    #[must_use]
    pub fn resolve(cwd: &str, input: &str) -> Self {
        let (cwd_prefix, cwd_root, cwd_rest) = split_prefix(cwd);
        let (in_prefix, in_root, in_rest) = split_prefix(input);

        let (prefix, has_root, combined_first, combined_second) = if !in_prefix.is_empty() {
            (in_prefix, in_root, in_rest, "")
        } else if in_root {
            (cwd_prefix, true, in_rest, "")
        } else {
            (cwd_prefix, cwd_root, cwd_rest, in_rest)
        };

        let anchored = has_root || !prefix.is_empty();
        let segments = push_segments(Vec::new(), combined_first, anchored);
        let segments = push_segments(segments, combined_second, anchored);

        let mut out = String::new();
        out.push_str(prefix);
        if has_root {
            out.push('/');
        }
        for (i, seg) in segments.iter().enumerate() {
            if i > 0 {
                out.push('/');
            }
            out.push_str(seg);
        }
        Self(out)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The last normal path segment of `path`, or `None` when it has none (e.g. `/` or `\\server\share`).
#[must_use]
pub(crate) fn file_name(path: &str) -> Option<&str> {
    let (_, _, rest) = split_prefix(path);
    rest.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .filter(|&s| s != "." && s != "..")
}

/// What a file read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Missing,
    Found(RawFile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    /// Length on disk, which can exceed `doc.total_bytes()`.
    pub len: u64,
    pub disk: DiskState,
    /// Normalized document tree, or empty when `len` exceeds `max_size`.
    pub doc: Doc,
    /// Detected line ending mode (`None` if no `\n` was seen).
    pub eol: Option<Eol>,
}

impl RawFile {
    #[must_use]
    pub fn from_bytes(len: u64, disk: DiskState, raw: &[u8]) -> Self {
        let (doc, eol) = EolStreamDecoder::new().push_chunk(raw).finish();
        Self {
            len,
            disk,
            doc,
            eol,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backing {
    None,
    /// The `*scratch*` buffer: it has no file, but its unsaved text is kept in a WAL slot (I-42).
    Scratch {
        wal_dirty: bool,
    },
    File {
        path: FilePath,
        disk: DiskState,
        wal_dirty: bool,
    },
}

impl Backing {
    #[must_use]
    pub const fn file(path: FilePath, disk: DiskState) -> Self {
        Self::File {
            path,
            disk,
            wal_dirty: false,
        }
    }

    /// The backing of the `*scratch*` buffer. Only the buffer pool and `switch-to-buffer` make it,
    /// and only for a buffer named `*scratch*` (I-42).
    #[must_use]
    pub(crate) const fn scratch() -> Self {
        Self::Scratch { wal_dirty: false }
    }

    #[must_use]
    pub const fn is_scratch(&self) -> bool {
        matches!(self, Self::Scratch { .. })
    }

    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::File { path, .. } => Some(path.as_str()),
            Self::None | Self::Scratch { .. } => None,
        }
    }

    #[must_use]
    pub const fn disk(&self) -> DiskState {
        match self {
            Self::File { disk, .. } => *disk,
            Self::None | Self::Scratch { .. } => DiskState::Unknown,
        }
    }

    #[must_use]
    pub const fn is_wal_dirty(&self) -> bool {
        match self {
            Self::File { wal_dirty, .. } | Self::Scratch { wal_dirty } => *wal_dirty,
            Self::None => false,
        }
    }

    #[must_use]
    pub(crate) fn set_wal_dirty(self, dirty: bool) -> Self {
        match self {
            Self::File { path, disk, .. } => Self::File {
                path,
                disk,
                wal_dirty: dirty,
            },
            Self::Scratch { .. } => Self::Scratch { wal_dirty: dirty },
            Self::None => Self::None,
        }
    }

    #[must_use]
    pub fn with_disk(self, disk: DiskState) -> Self {
        match self {
            Self::File {
                path, wal_dirty, ..
            } => Self::File {
                path,
                disk,
                wal_dirty,
            },
            other @ (Self::None | Self::Scratch { .. }) => other,
        }
    }
}

pub struct Loaded {
    pub text: Doc,
    pub backing: Backing,
    pub eol: Eol,
}

/// A file longer than `max-file-size`. It is refused, never loaded in part (I-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    pub len: u64,
    pub max: u64,
}

impl TooLarge {
    /// The echo-area message for a refused file.
    #[must_use]
    pub fn message(self) -> String {
        format!(
            "File too large: {} bytes (max-file-size {})",
            self.len, self.max
        )
    }
}

/// Builds the buffer for a file read. A file longer than `max_size` is refused (I-04).
/// A missing file is an empty buffer with unknown disk state.
///
/// # Errors
/// Returns `TooLarge` if the raw file length exceeds `max_size`.
pub fn loaded_from(
    path: &FilePath,
    read: Read,
    max_size: u64,
    default_eol: Eol,
) -> Result<Loaded, TooLarge> {
    match read {
        Read::Missing => Ok(Loaded {
            text: Doc::default(),
            backing: Backing::file(path.clone(), DiskState::Unknown),
            eol: default_eol,
        }),
        Read::Found(raw) => {
            if raw.len > max_size {
                return Err(TooLarge {
                    len: raw.len,
                    max: max_size,
                });
            }
            let eol = raw.eol.unwrap_or(default_eol);
            Ok(Loaded {
                text: raw.doc,
                backing: Backing::file(path.clone(), raw.disk),
                eol,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use memchr::memchr_iter;

    use super::*;

    fn found(len: u64, bytes: &[u8]) -> Read {
        Read::Found(RawFile::from_bytes(len, DiskState::Unknown, bytes))
    }

    fn load(read: Read, max_size: u64) -> Loaded {
        loaded_from(&FilePath::resolve("", "f"), read, max_size, Eol::Lf).unwrap()
    }

    #[test]
    fn scratch_backing_has_no_file_but_tracks_its_wal() {
        let b = Backing::scratch();
        assert!(b.is_scratch());
        assert_eq!(b.path(), None);
        assert_eq!(b.disk(), DiskState::Unknown);
        assert!(!b.is_wal_dirty());
        let b = b.set_wal_dirty(true);
        assert!(b.is_wal_dirty());
        assert_eq!(b.clone().with_disk(DiskState::Unknown), b);
        assert!(!b.set_wal_dirty(false).is_wal_dirty());
        assert!(!Backing::None.is_scratch());
    }

    #[test]
    fn resolve_folds_parent() {
        let cwd = "/w";
        let expected = "/w/a.txt";
        assert_eq!(FilePath::resolve(cwd, "./a.txt").as_str(), expected);
        assert_eq!(FilePath::resolve(cwd, "a.txt").as_str(), expected);
        assert_eq!(FilePath::resolve(cwd, "/w/./a.txt").as_str(), expected);
        assert_eq!(FilePath::resolve(cwd, "../w/a.txt").as_str(), expected);
        assert_eq!(FilePath::resolve(cwd, "a/../b.txt").as_str(), "/w/b.txt");
        assert_eq!(FilePath::resolve(cwd, "/..").as_str(), "/");
    }

    #[test]
    fn resolve_keeps_leading_parent_of_relative_path() {
        let cwd = "";
        assert_eq!(FilePath::resolve(cwd, "../x").as_str(), "../x");
        assert_eq!(FilePath::resolve(cwd, "a/../../x").as_str(), "../x");
    }

    #[test]
    fn resolve_handles_windows_prefixes_and_separators() {
        assert_eq!(
            FilePath::resolve(r"C:\work\proj", r"..\other\a.txt").as_str(),
            "C:/work/other/a.txt"
        );
        assert_eq!(
            FilePath::resolve(r"C:\work", r"C:\..\root.txt").as_str(),
            "C:/root.txt"
        );
        assert_eq!(
            FilePath::resolve(r"\\server\share\dir", r"..\file.txt").as_str(),
            r"\\server\share/file.txt"
        );
        assert_eq!(
            FilePath::resolve(r"\\server\share", "file.txt").as_str(),
            r"\\server\share/file.txt"
        );
        assert_eq!(
            FilePath::resolve("", r"\\server\share"),
            FilePath::resolve("", r"\\server\share\a\..")
        );
        assert_eq!(FilePath::resolve("/w", "///w/a.txt").as_str(), "/w/a.txt");
        assert_eq!(FilePath::resolve("", "///a/b/../..").as_str(), "/");
        assert_eq!(file_name(FilePath::resolve("", "/").as_str()), None);
        assert_eq!(file_name(r"\\server\share"), None);
        assert_eq!(
            file_name(FilePath::resolve("/w", "a.txt").as_str()),
            Some("a.txt")
        );
    }

    #[test]
    fn a_missing_file_takes_the_default_eol() {
        let path = FilePath::resolve("", "new.txt");
        let loaded = loaded_from(&path, Read::Missing, 4, Eol::Crlf).unwrap();
        assert_eq!(loaded.eol, Eol::Crlf);
    }

    #[test]
    fn oversized_file_is_refused() {
        let big = FilePath::resolve("", "big.txt");
        let err = loaded_from(&big, found(10, b"0123"), 4, Eol::Lf).err();
        assert_eq!(err, Some(TooLarge { len: 10, max: 4 }));
    }

    #[test]
    fn file_at_size_limit_is_loaded_whole() {
        let loaded = load(found(4, b"0123"), 4);
        assert_eq!(loaded.text, Doc::from_bytes(b"0123"));
    }

    #[test]
    fn too_large_message_names_the_limit() {
        let err = TooLarge { len: 10, max: 4 };
        assert_eq!(err.message(), "File too large: 10 bytes (max-file-size 4)");
    }

    #[test]
    fn missing_file_has_unknown_disk_state() {
        let loaded = load(Read::Missing, 1 << 20);
        assert_eq!(loaded.text.total_bytes(), 0);
        assert!(matches!(
            loaded.backing,
            Backing::File {
                disk: DiskState::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn crlf_file_loads_as_normalized_text() {
        let loaded = load(found(6, b"a\r\nb\r\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text, Doc::from_bytes(b"a\nb\n"));
    }

    #[test]
    fn missing_file_is_lf() {
        let loaded = load(Read::Missing, 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
    }

    #[test]
    fn mixed_file_is_normalized_with_majority_mode() {
        let loaded = load(found(5, b"a\r\nb\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
        assert_eq!(loaded.text, Doc::from_bytes(b"a\nb\n"));
    }

    #[test]
    fn lone_cr_is_not_a_line_ending() {
        let loaded = load(found(3, b"a\rb"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
        assert_eq!(loaded.text, Doc::from_bytes(b"a\rb"));
    }

    #[test]
    fn crlf_only_file_becomes_one_newline() {
        let loaded = load(found(2, b"\r\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text, Doc::from_bytes(b"\n"));
    }

    #[test]
    fn double_cr_before_lf_keeps_one_cr_in_text() {
        let loaded = load(found(4, b"a\r\r\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text, Doc::from_bytes(b"a\r\n"));
    }

    #[test]
    fn empty_file_is_lf_and_empty() {
        let loaded = load(found(0, b""), 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
        assert_eq!(loaded.text.total_bytes(), 0);
    }

    #[test]
    fn crlf_at_size_limit_is_loaded_and_normalized() {
        let loaded = load(found(6, b"a\r\nb\r\n"), 6);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text, Doc::from_bytes(b"a\nb\n"));
    }

    #[test]
    fn disk_size_is_raw_size_not_normalized_length() {
        let disk = DiskState::Known {
            mtime: Duration::ZERO,
            size: 3,
        };
        let read = Read::Found(RawFile::from_bytes(3, disk, b"a\r\n"));
        let loaded = load(read, 1 << 20);
        assert_eq!(loaded.text.total_bytes(), 2);
        assert!(matches!(
            loaded.backing,
            Backing::File { disk: d, .. } if d == disk
        ));
    }

    #[test]
    fn pure_files_round_trip_through_load_and_encode() {
        const ALPHA: [u8; 4] = [b'\r', b'\n', b'a', 0xFF];
        for len in 0..=7u32 {
            for n in 0..4usize.pow(len) {
                let raw: Vec<u8> = (0..len).map(|i| ALPHA[(n / 4usize.pow(i)) % 4]).collect();
                let lf = memchr_iter(b'\n', &raw).count();
                let crlf = raw
                    .windows(2)
                    .filter(|w| w[0] == b'\r' && w[1] == b'\n')
                    .count();
                // Pure files (pure CRLF or pure LF) must round trip exactly.
                if crlf != 0 && crlf != lf {
                    continue;
                }
                let loaded = load(found(raw.len() as u64, &raw), 1 << 20);
                assert_eq!(
                    crate::test_support::doc_encode_eol(&loaded.text, loaded.eol),
                    raw,
                    "raw={raw:?}"
                );
            }
        }
    }
}
