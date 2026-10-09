//! Where the person's own skills are kept (ADR 0070).

use async_trait::async_trait;

use super::RepositoryError;

/// One skill as it is stored: the folder it lives in, and its `SKILL.md`.
///
/// Raw, as read: core parses, checks and redacts it before anything uses it,
/// so a skill copied into the folder by hand is held to the same rules as
/// one imported in the window.
#[derive(Clone, PartialEq, Eq)]
pub struct SkillFile {
    /// The folder's name, which is the skill's id.
    pub folder: String,
    /// The text of its `SKILL.md`.
    pub markdown: String,
}

impl std::fmt::Debug for SkillFile {
    // Its size only: a debug print must not copy the skill into a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillFile")
            .field("folder", &self.folder)
            .field("bytes", &self.markdown.len())
            .finish()
    }
}

/// The person's skill library.
#[async_trait]
pub trait SkillLibrary: Send + Sync {
    /// Every skill in the library, in the order of their folders' names.
    /// A folder without a readable `SKILL.md` is left out.
    ///
    /// # Errors
    ///
    /// When the library cannot be read at all.
    async fn list(&self) -> Result<Vec<SkillFile>, RepositoryError>;

    /// Keep a new skill in `folder`.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Conflict`] when a skill already has that folder.
    async fn add(&self, file: &SkillFile) -> Result<(), RepositoryError>;

    /// Delete the skill in `folder`, and its folder.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::NotFound`] when there is none.
    async fn remove(&self, folder: &str) -> Result<(), RepositoryError>;
}
