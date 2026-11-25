use crate::weave_tree::{Doc, DocBuilder};
use memchr::memchr_iter;
use std::ops::ControlFlow;
use triomphe::AllocError;

/// End-of-line mode.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    #[default]
    Lf,
    Crlf,
}

impl Eol {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }

    /// Whether normalizing these bytes after saving leaves the document unchanged.
    pub(crate) fn preserves_saved_doc(self, doc: &Doc) -> bool {
        if self == Self::Crlf || doc.total_lines() == 1 {
            return true;
        }
        // LF saves verbatim, but reopening strips CRs immediately before LF, even across slices.
        let mut trailing_cr = false;
        doc.try_for_each_slice(0..doc.total_bytes(), |chunk| {
            if (trailing_cr && chunk.first() == Some(&b'\n'))
                || memchr::memmem::find(chunk, b"\r\n").is_some()
            {
                return ControlFlow::Break(());
            }
            trailing_cr = chunk.last() == Some(&b'\r');
            ControlFlow::Continue(())
        })
        .expect("full range is valid")
        .is_continue()
    }

    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Lf => Self::Crlf,
            Self::Crlf => Self::Lf,
        }
    }
}

/// Streams raw file chunks into a `Doc` while detecting and normalizing CRLF line endings.
#[derive(Debug, Default)]
pub struct EolStreamDecoder {
    lf: usize,
    crlf: usize,
    pending_cr: bool,
    builder: DocBuilder,
}

impl EolStreamDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one raw chunk from disk, handling `\r\n` pairs split across chunk boundaries.
    #[must_use]
    pub fn push_chunk(self, chunk: &[u8]) -> Self {
        self.try_push_chunk(chunk)
            .expect("cannot allocate decoded text")
    }

    /// Decodes a chunk, returning an error if normalization or slice allocation fails.
    pub fn try_push_chunk(mut self, chunk: &[u8]) -> Result<Self, AllocError> {
        if chunk.is_empty() {
            return Ok(self);
        }
        if self.pending_cr {
            self.pending_cr = false;
            if chunk[0] == b'\n' {
                self.crlf += 1;
            } else {
                self.builder = self.builder.try_push_bytes(b"\r")?;
            }
        }
        let body = if chunk.last() == Some(&b'\r') {
            self.pending_cr = true;
            &chunk[..chunk.len() - 1]
        } else {
            chunk
        };
        if body.is_empty() {
            return Ok(self);
        }

        let chunk_lf = memchr_iter(b'\n', body).count();
        self.lf += chunk_lf;
        let chunk_crlf = memchr_iter(b'\r', body)
            .filter(|&i| body.get(i + 1) == Some(&b'\n'))
            .count();
        self.crlf += chunk_crlf;

        if chunk_crlf == 0 {
            self.builder = self.builder.try_push_bytes(body)?;
            return Ok(self);
        }

        let mut normalized = Vec::new();
        normalized
            .try_reserve_exact(body.len() - chunk_crlf)
            .map_err(|_| AllocError)?;
        let mut last = 0;
        for i in memchr_iter(b'\r', body) {
            if body.get(i + 1) == Some(&b'\n') {
                normalized.extend_from_slice(&body[last..i]);
                last = i + 1;
            }
        }
        normalized.extend_from_slice(&body[last..]);
        self.builder = self.builder.try_push_bytes(&normalized)?;
        Ok(self)
    }

    /// Finalizes the `Doc` and returns the detected `Eol` (`None` if no `\n` was seen).
    #[must_use]
    pub fn finish(self) -> (Doc, Option<Eol>) {
        self.try_finish().expect("cannot allocate decoded document")
    }

    /// Finalizes the decoded document, returning an error if allocation fails.
    pub fn try_finish(mut self) -> Result<(Doc, Option<Eol>), AllocError> {
        if self.pending_cr {
            self.builder = self.builder.try_push_bytes(b"\r")?;
        }
        let lone_lf = self.lf.saturating_sub(self.crlf);
        let eol = if self.lf == 0 {
            None
        } else if self.crlf > lone_lf {
            Some(Eol::Crlf)
        } else {
            Some(Eol::Lf)
        };
        Ok((self.builder.try_finish()?, eol))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weave_tree::Unit;

    fn decode(raw: &[u8], default: Eol) -> (Eol, Doc) {
        let (doc, eol) = EolStreamDecoder::new().push_chunk(raw).finish();
        (eol.unwrap_or(default), doc)
    }

    #[test]
    fn pure_crlf_is_detected_and_normalized() {
        let (eol, doc) = decode(b"a\r\nb\r\n", Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(doc, Doc::from_bytes(b"a\nb\n"));
    }

    #[test]
    fn default_eol_applies_only_when_there_is_no_line_break() {
        for raw in [&b"abc"[..], b""] {
            let (eol, doc) = decode(raw, Eol::Crlf);
            assert_eq!(eol, Eol::Crlf);
            assert_eq!(doc, Doc::from_bytes(raw));
        }
        let (eol, _) = decode(b"a\nb\n", Eol::Crlf);
        assert_eq!(eol, Eol::Lf);
        let (eol, _) = decode(b"a\r\nb\r\n", Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
    }

    #[test]
    fn lf_files_are_kept_raw() {
        for raw in [&b"a\nb\n"[..], b"a\rb", b""] {
            let (eol, doc) = decode(raw, Eol::Lf);
            assert_eq!(eol, Eol::Lf);
            assert_eq!(doc, Doc::from_bytes(raw));
        }
    }

    #[test]
    fn majority_crlf_detected_and_normalized() {
        // 2 CRLF vs 1 LF -> CRLF mode, normalized text
        let (eol, doc) = decode(b"a\r\nb\r\nc\n", Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(doc, Doc::from_bytes(b"a\nb\nc\n"));
    }

    #[test]
    fn majority_lf_or_tie_detected_and_normalized() {
        // 1 CRLF vs 1 LF -> tie -> LF mode, normalized text
        let (eol, doc) = decode(b"a\r\nb\n", Eol::Lf);
        assert_eq!(eol, Eol::Lf);
        assert_eq!(doc, Doc::from_bytes(b"a\nb\n"));

        // 1 CRLF vs 2 LF -> LF mode, normalized text
        let (eol, doc) = decode(b"a\r\nb\nc\n", Eol::Lf);
        assert_eq!(eol, Eol::Lf);
        assert_eq!(doc, Doc::from_bytes(b"a\nb\nc\n"));
    }

    #[test]
    fn labels_name_each_mode() {
        assert_eq!(Eol::Lf.label(), "LF");
        assert_eq!(Eol::Crlf.label(), "CRLF");
    }

    #[test]
    fn double_cr_before_lf_keeps_one_cr_in_text() {
        let (eol, doc) = decode(b"a\r\r\n", Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(doc, Doc::from_bytes(b"a\r\n"));
    }

    #[test]
    fn standalone_cr_preserved_in_memory() {
        let (eol, doc) = decode(b"a\rb\r\n", Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(doc, Doc::from_bytes(b"a\rb\n"));
    }

    #[test]
    fn saved_doc_round_trip_detects_crlf_across_slices() {
        let doc = DocBuilder::new()
            .push_bytes(b"a\r")
            .push_bytes(b"\nb\n")
            .finish();
        assert!(!Eol::Lf.preserves_saved_doc(&doc));
        assert!(Eol::Crlf.preserves_saved_doc(&doc));
        assert!(Eol::Lf.preserves_saved_doc(&Doc::from_bytes(b"a\rb\n")));
    }

    #[test]
    fn toggled_flips_between_eol_mode() {
        assert_eq!(Eol::Lf.toggled(), Eol::Crlf);
        assert_eq!(Eol::Crlf.toggled(), Eol::Lf);
        assert_eq!(Eol::default().toggled().toggled(), Eol::default());
    }

    #[test]
    fn stream_decoder_handles_split_crlf_and_trailing_cr() {
        let dec = EolStreamDecoder::new()
            .push_chunk(b"a\r")
            .push_chunk(b"\nb\r")
            .push_chunk(b"c\r");
        let (doc, eol) = dec.finish();
        assert_eq!(eol, Some(Eol::Crlf));
        assert_eq!(doc, Doc::from_bytes(b"a\nb\rc\r"));
    }

    #[test]
    fn stream_decoder_preserves_multibyte_utf8_across_chunks() {
        let (doc, eol) = EolStreamDecoder::new()
            .push_chunk(&[0xEA])
            .push_chunk(&[0xB0, 0x80, b'\r'])
            .push_chunk(b"\n")
            .finish();
        assert_eq!(eol, Some(Eol::Crlf));
        assert_eq!(doc.decode_at(0), Some((Unit::Char('가'), 3)));
        assert!(!doc.is_boundary(1));
        assert!(!doc.is_boundary(2));
        assert_eq!(doc, Doc::from_bytes("가\n".as_bytes()));
    }
}
