//! Reading the text out of a document a person attaches (ADR 0062).

/// Why a document's text could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// It is not a document of the kind its name says, or it is damaged.
    #[error("the file could not be read as a {kind} document")]
    Unreadable {
        /// The kind it was read as, such as "PDF".
        kind: &'static str,
    },
    /// It is protected by a password.
    #[error("the {kind} document is protected by a password")]
    Encrypted {
        /// The kind it was read as.
        kind: &'static str,
    },
    /// Reading it would unpack more than a document may hold.
    #[error("the {kind} document unpacks to more than Tramontana will read")]
    TooLarge {
        /// The kind it was read as.
        kind: &'static str,
    },
}

/// Something that reads the text of documents that are not plain text.
///
/// Called only with a file the person chose, after core checked its size.
/// Must not panic, touch the network or filesystem, or start a process: it is
/// given bytes and returns text. Core runs it off the async runtime, under a
/// deadline.
pub trait DocumentReader: Send + Sync {
    /// The media type of a document with this name that this reader reads,
    /// such as `application/pdf`, or `None`.
    fn media_type(&self, name: &str) -> Option<&'static str>;

    /// The document's text, in reading order as far as the format allows.
    ///
    /// # Errors
    ///
    /// A [`DocumentError`] saying why it could not be read.
    fn read_text(&self, name: &str, bytes: &[u8]) -> Result<String, DocumentError>;
}
