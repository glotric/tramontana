//! Reading the project-authored files a task runs under.

use async_trait::async_trait;

use crate::config::RawSource;
use crate::contracts::{ErrorEnvelope, ProjectId};

/// The project's own Markdown, as it is on disk now.
///
/// Core owns precedence, validation and what may be sent; the walk itself is
/// an adapter's, because only an adapter knows where the approved root is and
/// how to stay inside it (`CONFIGURATION.md` §3).
///
/// This is read at the moment a task starts rather than from the last
/// resolution stored for the project: the record a task keeps must describe
/// the instructions it actually ran under, and a file edited since the
/// project was opened would otherwise be quoted from a stale inventory
/// (ADR 0052).
#[async_trait]
pub trait ProjectSourceReader: Send + Sync {
    /// Every project-authored file for `project_id`, with its bytes.
    ///
    /// # Errors
    ///
    /// When the approved root cannot be reopened. An individual unreadable
    /// file is not an error: discovery reports what it found, and the loader
    /// turns anything missing or malformed into a diagnostic.
    async fn read(&self, project_id: &ProjectId) -> Result<Vec<RawSource>, ErrorEnvelope>;
}
