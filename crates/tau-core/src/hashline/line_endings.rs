//! Line-ending detection and preservation (the reference's `line-endings.ts`):
//! an edit replaces a byte range, so the bytes outside that range — including
//! every untouched line's terminator — come back byte-identical. The core
//! pipeline works on LF-normalized text; this module detects the file's
//! dominant ending and restores it around the edited span.

/// A file's line ending: dominant in the file, one per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Lf,
    Crlf,
    Cr,
}

impl Ending {
    /// The terminator byte sequence.
    #[must_use]
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::Crlf => b"\r\n",
            Self::Cr => b"\r",
        }
    }
}

/// The dominant ending of `raw`: CRLF beats CR beats LF; a file with no
/// terminators at all is LF (the pipeline's native form).
#[must_use]
pub fn detect(raw: &str) -> Ending {
    if raw.contains("\r\n") {
        return Ending::Crlf;
    }
    if raw.contains('\r') {
        return Ending::Cr;
    }
    Ending::Lf
}
/// Join `lines` with the given ending and (optionally) a trailing terminator.
#[must_use]
pub fn join(lines: &[String], ending: Ending, trailing: bool) -> String {
    let mut out = String::new();
    let sep = ending.bytes();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push_str(std::str::from_utf8(sep).expect("endings are valid UTF-8"));
        }
        out.push_str(line);
    }
    if trailing && !lines.is_empty() {
        out.push_str(std::str::from_utf8(sep).expect("endings are valid UTF-8"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_picks_the_dominant_ending() {
        assert_eq!(detect("a\r\nb\r\n"), Ending::Crlf);
        assert_eq!(detect("a\rb\r"), Ending::Cr);
        assert_eq!(detect("a\nb\n"), Ending::Lf);
        assert_eq!(detect("a\r\nb\n"), Ending::Crlf);
        assert_eq!(detect(""), Ending::Lf);
    }

    #[test]
    fn join_roundtrips_every_ending() {
        let lines = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(join(&lines, Ending::Lf, true), "a\nb\n");
        assert_eq!(join(&lines, Ending::Crlf, true), "a\r\nb\r\n");
        assert_eq!(join(&lines, Ending::Crlf, false), "a\r\nb");
        assert_eq!(join(&[], Ending::Lf, true), "");
    }
}
