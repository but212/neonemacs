//! Buffer backing, and the buffer that a file read produces. Pure: the bytes come from the shell.

use crate::core::effect::Read;
use crate::core::eol::Eol;
use crate::core::text::Text;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    Known { mtime: SystemTime, size: u64 },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadExtent {
    Whole,
    Truncated,
    TruncatedConfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backing {
    None,
    File {
        path: PathBuf,
        disk: DiskState,
        extent: LoadExtent,
        wal_dirty: bool,
    },
}

impl Backing {
    pub const fn file(path: PathBuf, disk: DiskState, extent: LoadExtent) -> Self {
        Self::File {
            path,
            disk,
            extent,
            wal_dirty: false,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::File { path, .. } => Some(path),
            Self::None => None,
        }
    }

    pub const fn disk(&self) -> DiskState {
        match self {
            Self::File { disk, .. } => *disk,
            Self::None => DiskState::Unknown,
        }
    }

    pub const fn is_wal_dirty(&self) -> bool {
        match self {
            Self::File { wal_dirty, .. } => *wal_dirty,
            Self::None => false,
        }
    }

    pub fn set_wal_dirty(self, dirty: bool) -> Self {
        match self {
            Self::File {
                path, disk, extent, ..
            } => Self::File {
                path,
                disk,
                extent,
                wal_dirty: dirty,
            },
            Self::None => Self::None,
        }
    }

    pub fn with_disk(self, disk: DiskState) -> Self {
        match self {
            Self::File {
                path,
                extent,
                wal_dirty,
                ..
            } => Self::File {
                path,
                disk,
                extent,
                wal_dirty,
            },
            Self::None => Self::None,
        }
    }

    pub fn confirm_truncated(self) -> Self {
        match self {
            Self::File {
                path,
                disk,
                wal_dirty,
                ..
            } => Self::File {
                path,
                disk,
                extent: LoadExtent::TruncatedConfirmed,
                wal_dirty,
            },
            Self::None => Self::None,
        }
    }
}

pub struct Loaded {
    pub text: Text,
    pub backing: Backing,
    pub eol: Eol,
}

/// Builds the buffer for a file read. A file longer than `max_size` is `Truncated` (I-04).
/// A missing file is an empty buffer with unknown disk state.
pub fn loaded_from(path: &Path, read: Read, max_size: u64) -> Loaded {
    match read {
        Read::Missing => Loaded {
            text: Text::default(),
            backing: Backing::file(path.to_path_buf(), DiskState::Unknown, LoadExtent::Whole),
            eol: Eol::default(),
        },
        Read::Found(raw) => {
            let truncated = raw.len > max_size;
            let extent = if truncated {
                LoadExtent::Truncated
            } else {
                LoadExtent::Whole
            };

            let (eol, text) = Eol::from_raw(raw.bytes);

            Loaded {
                text: Text::from_vec(text),
                backing: Backing::file(path.to_path_buf(), raw.disk, extent),
                eol,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::effect::RawFile;

    fn found(len: u64, bytes: &[u8]) -> Read {
        Read::Found(RawFile {
            len,
            disk: DiskState::Unknown,
            bytes: bytes.to_vec(),
        })
    }

    #[test]
    fn oversized_file_is_truncated() {
        let loaded = loaded_from(Path::new("big.txt"), found(10, b"0123"), 4);
        assert_eq!(loaded.text.to_bytes(), b"0123");
        assert!(matches!(
            loaded.backing,
            Backing::File {
                extent: LoadExtent::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn rawfile_truncation_is_derived_in_core() {
        let at_limit = loaded_from(Path::new("f"), found(4, b"0123"), 4);
        assert!(matches!(
            at_limit.backing,
            Backing::File {
                extent: LoadExtent::Whole,
                ..
            }
        ));
        let over = loaded_from(Path::new("f"), found(5, b"0123"), 4);
        assert!(matches!(
            over.backing,
            Backing::File {
                extent: LoadExtent::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn missing_file_has_unknown_disk_state() {
        let loaded = loaded_from(Path::new("new.txt"), Read::Missing, 1 << 20);
        assert_eq!(loaded.text.len(), 0);
        assert!(matches!(
            loaded.backing,
            Backing::File {
                disk: DiskState::Unknown,
                extent: LoadExtent::Whole,
                ..
            }
        ));
    }

    #[test]
    fn crlf_file_loads_as_normalized_text() {
        let loaded = loaded_from(Path::new("f"), found(6, b"a\r\nb\r\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text.to_bytes(), b"a\nb\n");
    }

    #[test]
    fn truncated_file_normalizes_and_detects_eol() {
        let loaded = loaded_from(Path::new("f"), found(10, b"a\r\nb\r\n"), 6);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text.to_bytes(), b"a\nb\n");
        assert!(matches!(
            loaded.backing,
            Backing::File {
                extent: LoadExtent::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn missing_file_is_lf() {
        let loaded = loaded_from(Path::new("new.txt"), Read::Missing, 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
    }

    #[test]
    fn mixed_file_is_normalized_with_majority_mode() {
        let loaded = loaded_from(Path::new("f"), found(5, b"a\r\nb\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
        assert_eq!(loaded.text.to_bytes(), b"a\nb\n");
    }

    #[test]
    fn lone_cr_is_not_a_line_ending() {
        let loaded = loaded_from(Path::new("f"), found(3, b"a\rb"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
        assert_eq!(loaded.text.to_bytes(), b"a\rb");
    }

    #[test]
    fn crlf_only_file_becomes_one_newline() {
        let loaded = loaded_from(Path::new("f"), found(2, b"\r\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text.to_bytes(), b"\n");
    }

    #[test]
    fn double_cr_before_lf_keeps_one_cr_in_text() {
        let loaded = loaded_from(Path::new("f"), found(4, b"a\r\r\n"), 1 << 20);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text.to_bytes(), b"a\r\n");
    }

    #[test]
    fn empty_file_is_lf_and_empty() {
        let loaded = loaded_from(Path::new("f"), found(0, b""), 1 << 20);
        assert_eq!(loaded.eol, Eol::Lf);
        assert!(loaded.text.is_empty());
    }

    #[test]
    fn crlf_at_size_limit_is_whole_and_normalized() {
        let loaded = loaded_from(Path::new("f"), found(6, b"a\r\nb\r\n"), 6);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text.to_bytes(), b"a\nb\n");
        assert!(matches!(
            loaded.backing,
            Backing::File {
                extent: LoadExtent::Whole,
                ..
            }
        ));
    }

    #[test]
    fn truncated_crlf_normalizes_pair_and_keeps_cut_cr() {
        // The cut falls between \r and \n. The leading CRLF is normalized, the lone \r is kept.
        let loaded = loaded_from(Path::new("f"), found(6, b"a\r\nb\r"), 5);
        assert_eq!(loaded.eol, Eol::Crlf);
        assert_eq!(loaded.text.to_bytes(), b"a\nb\r");
        assert!(matches!(
            loaded.backing,
            Backing::File {
                extent: LoadExtent::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn disk_size_is_raw_size_not_normalized_length() {
        let disk = DiskState::Known {
            mtime: SystemTime::UNIX_EPOCH,
            size: 3,
        };
        let read = Read::Found(RawFile {
            len: 3,
            disk,
            bytes: b"a\r\n".to_vec(),
        });
        let loaded = loaded_from(Path::new("f"), read, 1 << 20);
        assert_eq!(loaded.text.len(), 2);
        assert!(matches!(
            loaded.backing,
            Backing::File { disk: d, .. } if d == disk
        ));
    }

    #[test]
    fn pure_files_round_trip_through_load_and_encode() {
        const ALPHA: [u8; 4] = [b'\r', b'\n', b'a', 0xFF];
        for len in 0..=7usize {
            for n in 0..4usize.pow(len as u32) {
                let raw: Vec<u8> = (0..len)
                    .map(|i| ALPHA[(n / 4usize.pow(i as u32)) % 4])
                    .collect();
                for max in [1u64 << 20, (len / 2) as u64] {
                    let head = &raw[..raw.len().min(max as usize)];
                    let lf = head.iter().filter(|&&b| b == b'\n').count();
                    let crlf = head
                        .windows(2)
                        .filter(|w| w[0] == b'\r' && w[1] == b'\n')
                        .count();
                    // Pure files (pure CRLF or pure LF) must round trip exactly.
                    let is_pure = crlf == 0 || crlf == lf;
                    if !is_pure {
                        continue;
                    }
                    let read = Read::Found(RawFile {
                        len: raw.len() as u64,
                        disk: DiskState::Unknown,
                        bytes: head.to_vec(),
                    });
                    let loaded = loaded_from(Path::new("f"), read, max);
                    assert_eq!(
                        loaded.eol.encode(&loaded.text.to_bytes()),
                        head,
                        "raw={raw:?} head={head:?} max={max}"
                    );
                }
            }
        }
    }
}
