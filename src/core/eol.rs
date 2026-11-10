use memchr::memchr_iter;

/// End-of-line mode.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    #[default]
    Lf,
    Crlf,
}

impl Eol {
    /// Detects line ending mode via majority vote and normalizes CRLF pairs to LF in memory.
    ///
    /// A file without any line break takes `default`. Pure LF or a tie yields `Lf`.
    /// Strictly more CRLF pairs than lone LF yields `Crlf`.
    /// All CRLF pairs are normalized by stripping the `\r` immediately preceding `\n`.
    /// Standalone `\r` characters not followed by `\n` are preserved.
    #[must_use]
    pub fn from_raw(raw: Vec<u8>, default: Self) -> (Self, Vec<u8>) {
        let lf = memchr_iter(b'\n', &raw).count();
        let crlf = raw
            .windows(2)
            .filter(|w| w[0] == b'\r' && w[1] == b'\n')
            .count();
        let lone_lf = lf.saturating_sub(crlf);
        let eol = if lf == 0 {
            default
        } else if crlf > lone_lf {
            Self::Crlf
        } else {
            Self::Lf
        };

        if crlf == 0 {
            return (eol, raw);
        }

        let mut normalized = Vec::with_capacity(raw.len() - crlf);
        for (i, &b) in raw.iter().enumerate() {
            if b == b'\r' && raw.get(i + 1) == Some(&b'\n') {
                continue;
            }
            normalized.push(b);
        }
        (eol, normalized)
    }

    /// Encodes normalized in-memory text for disk storage according to this EOL mode.
    #[must_use]
    pub fn encode(self, text: &[u8]) -> Vec<u8> {
        match self {
            Self::Lf => text.to_vec(),
            Self::Crlf => {
                let lf_count = memchr_iter(b'\n', text).count();
                let mut out = Vec::with_capacity(text.len() + lf_count);
                for &b in text {
                    if b == b'\n' {
                        out.push(b'\r');
                    }
                    out.push(b);
                }
                out
            }
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }

    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Lf => Self::Crlf,
            Self::Crlf => Self::Lf,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_crlf_is_detected_and_normalized() {
        let (eol, text) = Eol::from_raw(b"a\r\nb\r\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(text, b"a\nb\n");
    }

    #[test]
    fn default_eol_applies_only_when_there_is_no_line_break() {
        for raw in [&b"abc"[..], b""] {
            let (eol, text) = Eol::from_raw(raw.to_vec(), Eol::Crlf);
            assert_eq!(eol, Eol::Crlf);
            assert_eq!(text, raw);
        }
        let (eol, _) = Eol::from_raw(b"a\nb\n".to_vec(), Eol::Crlf);
        assert_eq!(eol, Eol::Lf);
        let (eol, _) = Eol::from_raw(b"a\r\nb\r\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
    }

    #[test]
    fn lf_files_are_kept_raw() {
        for raw in [&b"a\nb\n"[..], b"a\rb", b""] {
            let (eol, text) = Eol::from_raw(raw.to_vec(), Eol::Lf);
            assert_eq!(eol, Eol::Lf);
            assert_eq!(text, raw);
        }
    }

    #[test]
    fn majority_crlf_detected_and_normalized() {
        // 2 CRLF vs 1 LF -> CRLF mode, normalized text
        let (eol, text) = Eol::from_raw(b"a\r\nb\r\nc\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(text, b"a\nb\nc\n");
        assert_eq!(eol.encode(&text), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn majority_lf_or_tie_detected_and_normalized() {
        // 1 CRLF vs 1 LF -> tie -> LF mode, normalized text
        let (eol, text) = Eol::from_raw(b"a\r\nb\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Lf);
        assert_eq!(text, b"a\nb\n");
        assert_eq!(eol.encode(&text), b"a\nb\n");

        // 1 CRLF vs 2 LF -> LF mode, normalized text
        let (eol, text) = Eol::from_raw(b"a\r\nb\nc\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Lf);
        assert_eq!(text, b"a\nb\nc\n");
        assert_eq!(eol.encode(&text), b"a\nb\nc\n");
    }

    #[test]
    fn labels_name_each_mode() {
        assert_eq!(Eol::Lf.label(), "LF");
        assert_eq!(Eol::Crlf.label(), "CRLF");
    }

    #[test]
    fn double_cr_before_lf_keeps_one_cr_in_text() {
        let (eol, text) = Eol::from_raw(b"a\r\r\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(text, b"a\r\n");
        assert_eq!(eol.encode(&text), b"a\r\r\n");
    }

    #[test]
    fn standalone_cr_preserved_in_memory() {
        let (eol, text) = Eol::from_raw(b"a\rb\r\n".to_vec(), Eol::Lf);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(text, b"a\rb\n");
        assert_eq!(eol.encode(&text), b"a\rb\r\n");
    }

    #[test]
    fn encode_adds_cr_before_every_lf() {
        assert_eq!(Eol::Crlf.encode(b"a\nb"), b"a\r\nb");
        assert_eq!(Eol::Lf.encode(b"a\nb"), b"a\nb");
    }

    #[test]
    fn pure_files_round_trip() {
        for pure_lf in [b"" as &[u8], b"a", b"a\nb\n", b"line1\nline2\n"] {
            let (eol, text) = Eol::from_raw(pure_lf.to_vec(), Eol::Lf);
            assert_eq!(eol, Eol::Lf);
            assert_eq!(eol.encode(&text), pure_lf);
        }
        for pure_crlf in [b"a\r\n" as &[u8], b"a\r\nb\r\n", b"\r\n\r\n"] {
            let (eol, text) = Eol::from_raw(pure_crlf.to_vec(), Eol::Lf);
            assert_eq!(eol, Eol::Crlf);
            assert_eq!(eol.encode(&text), pure_crlf);
        }
    }

    #[test]
    fn toggled_flips_between_eol_mode() {
        assert_eq!(Eol::Lf.toggled(), Eol::Crlf);
        assert_eq!(Eol::Crlf.toggled(), Eol::Lf);
        assert_eq!(Eol::default().toggled().toggled(), Eol::default());
    }
}
