//! Buffer backing, and the buffer that a file read produces. Pure: the bytes come from the shell.

use crate::core::effect::Read;
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
    },
}

pub struct Loaded {
    pub text: Text,
    pub backing: Backing,
}

/// Builds the buffer for a file read. A file longer than `max_size` is `Truncated` (I-04).
/// A missing file is an empty buffer with unknown disk state.
pub fn loaded_from(path: &Path, read: Read, max_size: u64) -> Loaded {
    match read {
        Read::Missing => Loaded {
            text: Text::default(),
            backing: Backing::File {
                path: path.to_path_buf(),
                disk: DiskState::Unknown,
                extent: LoadExtent::Whole,
            },
        },
        Read::Found(raw) => {
            let extent = if raw.len > max_size {
                LoadExtent::Truncated
            } else {
                LoadExtent::Whole
            };
            Loaded {
                text: Text::from_vec(raw.bytes),
                backing: Backing::File {
                    path: path.to_path_buf(),
                    disk: raw.disk,
                    extent,
                },
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
}
