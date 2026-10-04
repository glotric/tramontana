//! Files the person picks for a message, held until it is sent (ADR 0058).
//!
//! The window never names a path. The operating system's picker returns
//! files to this adapter, which reads them and holds the bytes under a random
//! token; the window holds only the token, the name and the size, and sends
//! the tokens with the message. Whether a file may be attached is core's
//! decision, asked here as soon as it is picked so a refusal is shown then.

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope};
use tramontana_core::services::{ATTACHMENT_BYTES, AttachmentInput, DOCUMENT_BYTES};

/// At most this many files are held waiting to be sent. Older ones are
/// dropped first; a window that picked this many and sent none is not
/// holding them for anything.
const HELD: usize = 20;

/// Files picked and not yet sent, by token.
#[derive(Default)]
pub(crate) struct StagedAttachments {
    held: Mutex<VecDeque<(String, AttachmentInput)>>,
}

/// What the window is told about a held file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StagedAttachmentView {
    /// What the window sends back to attach it.
    pub(crate) token: String,
    /// The file's name, without its folder.
    pub(crate) name: String,
    /// Its size in bytes.
    pub(crate) byte_count: u64,
}

fn invalid(message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidRequest,
        message,
        CorrelationId::new("attachments"),
    )
}

impl StagedAttachments {
    /// Hold `input` under `token`, and say what the window may show of it.
    pub(crate) fn stage(&self, token: String, input: AttachmentInput) -> StagedAttachmentView {
        let view = StagedAttachmentView {
            token: token.clone(),
            name: input.name.clone(),
            byte_count: input.bytes.len() as u64,
        };
        if let Ok(mut held) = self.held.lock() {
            held.push_back((token, input));
            while held.len() > HELD {
                held.pop_front();
            }
        }
        view
    }

    /// The files held under `tokens`, in that order, still held.
    ///
    /// Read rather than taken: a message that fails to start keeps its files,
    /// and the caller [`Self::discard`]s them once it has been sent.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when a token is not held, having been sent or
    /// dropped already.
    pub(crate) fn read(&self, tokens: &[String]) -> Result<Vec<AttachmentInput>, ErrorEnvelope> {
        let held = self
            .held
            .lock()
            .map_err(|_| invalid("the attached files could not be read back"))?;
        tokens
            .iter()
            .map(|token| {
                held.iter()
                    .find(|(held, _)| held == token)
                    .map(|(_, input)| input.clone())
                    .ok_or_else(|| {
                        ErrorEnvelope::new(
                            ErrorCode::NotFound,
                            "an attached file is no longer held; attach it again",
                            CorrelationId::new("attachments"),
                        )
                    })
            })
            .collect()
    }

    /// Stop holding the file under `token`, if it is held.
    pub(crate) fn discard(&self, token: &str) {
        if let Ok(mut held) = self.held.lock() {
            held.retain(|(held, _)| held != token);
        }
    }
}

/// Read a picked file. Whether it may be attached is core's to say, and the
/// caller asks before holding it.
///
/// Reads at most one byte more than any file may hold (a PDF or Word
/// document's limit, the larger), so a huge file is refused by core without
/// being read whole.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when it is not a regular file or cannot be
/// read.
pub(crate) fn read_picked(path: &Path) -> Result<AttachmentInput, ErrorEnvelope> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("the chosen file has no name Tramontana can show"))?
        .to_owned();
    let metadata =
        std::fs::metadata(path).map_err(|_| invalid(format!("{name} could not be read")))?;
    if !metadata.is_file() {
        return Err(invalid(format!("{name} is not a file")));
    }
    let limit = u64::try_from(ATTACHMENT_BYTES.max(DOCUMENT_BYTES)).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|_| invalid(format!("{name} could not be read")))?;
    Ok(AttachmentInput { name, bytes })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn input(name: &str) -> AttachmentInput {
        AttachmentInput {
            name: name.to_owned(),
            bytes: b"text".to_vec(),
        }
    }

    #[test]
    fn a_held_file_is_read_in_order_until_it_is_discarded() {
        let staged = StagedAttachments::default();
        let view = staged.stage("t1".into(), input("a.md"));
        assert_eq!(view.name, "a.md");
        assert_eq!(view.byte_count, 4);
        staged.stage("t2".into(), input("b.md"));

        assert!(staged.read(&["t1".into(), "nope".into()]).is_err());
        let read = staged.read(&["t2".into(), "t1".into()]).unwrap();
        assert_eq!(read[0].name, "b.md");
        assert_eq!(read[1].name, "a.md");
        // Still held: a message that failed to start keeps its files.
        assert!(staged.read(&["t1".into()]).is_ok());
        staged.discard("t1");
        assert!(staged.read(&["t1".into()]).is_err());
    }

    #[test]
    fn only_the_newest_files_are_held() {
        let staged = StagedAttachments::default();
        for index in 0..=HELD {
            staged.stage(format!("t{index}"), input("a.md"));
        }
        assert!(staged.read(&["t0".into()]).is_err());
        assert!(staged.read(&[format!("t{HELD}")]).is_ok());
        staged.discard("t1");
        assert!(staged.read(&["t1".into()]).is_err());
    }

    #[test]
    fn a_picked_file_is_read_whole_up_to_the_document_limit_and_no_further() {
        let folder = tempfile::tempdir().unwrap();
        let text = folder.path().join("notes.md");
        std::fs::write(&text, "# Notes").unwrap();
        let read = read_picked(&text).unwrap();
        assert_eq!(read.name, "notes.md");
        assert_eq!(read.bytes, b"# Notes");

        let big = folder.path().join("big.pdf");
        std::fs::write(&big, vec![0u8; DOCUMENT_BYTES + 100]).unwrap();
        assert_eq!(read_picked(&big).unwrap().bytes.len(), DOCUMENT_BYTES + 1);
        assert!(read_picked(folder.path()).is_err());
    }
}
