/// Bounds applied to every read.
///
/// Applied while reading rather than after, so a hostile or merely enormous
/// file cannot decide how much memory the process allocates
/// (`SECURITY_AND_PERMISSIONS.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    /// Largest amount of file content returned.
    pub max_bytes: usize,
    /// Largest number of lines returned.
    pub max_lines: usize,
    /// Largest number of directory entries or search hits returned.
    pub max_entries: usize,
    /// How long a tool may run before it is abandoned.
    pub deadline: std::time::Duration,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_lines: 4_000,
            max_entries: 1_000,
            deadline: std::time::Duration::from_secs(15),
        }
    }
}

/// How many bytes at the start of a file are inspected for binary content.
const SNIFF_BYTES: usize = 8_000;

/// Whether a byte slice looks like binary rather than text.
///
/// A NUL byte is the signal, which is what `git` and `grep` use. Returning
/// binary content as text would put terminal escape sequences into an activity
/// stream and megabytes of noise into a model's context.
#[must_use]
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_not_binary() {
        assert!(!looks_binary(b"fn main() {}\n"));
        assert!(!looks_binary("ünïcode — em dash".as_bytes()));
        assert!(!looks_binary(b""));
    }

    #[test]
    fn a_nul_byte_marks_content_as_binary() {
        assert!(looks_binary(b"\x7fELF\x02\x01\x01\x00"));
        assert!(looks_binary(&[0x00]));
    }

    #[test]
    fn only_the_first_bytes_are_inspected() {
        // A bounded sniff: deciding "is this text" must not require reading a
        // gigabyte first.
        let mut content = vec![b'a'; SNIFF_BYTES * 2];
        content.push(0);
        assert!(!looks_binary(&content));
    }
}
