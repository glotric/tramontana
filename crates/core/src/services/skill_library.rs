//! The person's skill library, through the service (ADR 0070).

use std::sync::Arc;

use crate::contracts::{ErrorCode, ErrorEnvelope, PersonalSkill, TaskSkill};
use crate::ports::{RepositoryError, SkillLibrary};

use super::{TaskService, is_skill_id, prepare_import, read_skill};

impl TaskService {
    /// Keep the person's own skills in `library` (ADR 0070).
    #[must_use]
    pub fn with_skill_library(mut self, library: Arc<dyn SkillLibrary>) -> Self {
        self.skill_library = Some(library);
        self
    }

    fn library(&self) -> Result<&Arc<dyn SkillLibrary>, ErrorEnvelope> {
        self.skill_library.as_ref().ok_or_else(|| {
            self.error(
                ErrorCode::CapabilityUnsupported,
                "This application keeps no skills of its own",
            )
        })
    }

    fn library_error(&self, error: &RepositoryError) -> ErrorEnvelope {
        self.error(ErrorCode::PersistenceError, error.to_string())
    }

    /// Every skill in the person's library, by id. A folder that breaks the
    /// rules is left out rather than failing the list: one bad file copied in
    /// by hand must not hide the rest.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::CapabilityUnsupported`] when the service keeps no
    /// library, or when it cannot be read.
    pub async fn personal_skills(&self) -> Result<Vec<PersonalSkill>, ErrorEnvelope> {
        let files = self
            .library()?
            .list()
            .await
            .map_err(|error| self.library_error(&error))?;
        Ok(files
            .iter()
            .filter_map(|file| read_skill(file).ok())
            .map(|(listed, _)| listed)
            .collect())
    }

    /// Add a skill from its `SKILL.md`, named by its declared `name` or by
    /// `fallback`. Checked and redacted before anything is kept.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] when it breaks a rule,
    /// [`ErrorCode::Conflict`] when a skill already has its name.
    pub async fn import_skill(
        &self,
        markdown: &str,
        fallback: &str,
    ) -> Result<PersonalSkill, ErrorEnvelope> {
        let library = self.library()?;
        let (file, removed) = prepare_import(markdown, fallback)?;
        let (mut listed, _) = read_skill(&file)?;
        // What the import took out, which the kept text can no longer show.
        listed.redactions = removed;
        library.add(&file).await.map_err(|error| match error {
            RepositoryError::Conflict { .. } => self.error(
                ErrorCode::Conflict,
                format!(
                    "You already have a skill called \"{}\". Remove it first to replace it.",
                    file.folder
                ),
            ),
            other => self.library_error(&other),
        })?;
        Ok(listed)
    }

    /// Delete a skill from the person's library. Tasks that used it keep the
    /// text they were given; nothing else refers to it.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when there is no such skill.
    pub async fn remove_skill(&self, id: &str) -> Result<(), ErrorEnvelope> {
        if !is_skill_id(id) {
            return Err(self.error(ErrorCode::NotFound, format!("No skill \"{id}\"")));
        }
        self.library()?
            .remove(id)
            .await
            .map_err(|error| match error {
                RepositoryError::NotFound { .. } => {
                    self.error(ErrorCode::NotFound, format!("No skill \"{id}\""))
                }
                other => self.library_error(&other),
            })
    }

    /// The skills a task starting now is given, in the order chosen.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] when one is not in the library or too
    /// many are chosen.
    pub(super) async fn skills_for(&self, ids: &[String]) -> Result<Vec<TaskSkill>, ErrorEnvelope> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > super::TASK_SKILLS {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                format!("A task may use at most {} skills", super::TASK_SKILLS),
            ));
        }
        let files = self
            .library()?
            .list()
            .await
            .map_err(|error| self.library_error(&error))?;
        let mut chosen: Vec<TaskSkill> = Vec::new();
        for id in ids {
            if chosen.iter().any(|skill| &skill.id == id) {
                continue;
            }
            let file = files
                .iter()
                .find(|file| &file.folder == id)
                .ok_or_else(|| {
                    self.error(
                        ErrorCode::InvalidRequest,
                        format!("There is no skill \"{id}\" in your skills"),
                    )
                })?;
            chosen.push(read_skill(file)?.1);
        }
        Ok(chosen)
    }
}
