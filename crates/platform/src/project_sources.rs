//! Reading a project's own instruction files for a task that is starting.

use std::sync::Arc;

use async_trait::async_trait;
use tramontana_core::config::RawSource;
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, ProjectId};
use tramontana_core::ports::{ProjectRepository, ProjectSourceReader};

use crate::discovery::discover;
use crate::paths::canonicalize_root;

/// Discovery over the roots a project was opened with.
///
/// The walk is [`discover`]'s, so a task reads exactly the files the
/// configuration view shows, under the same bounds and the same refusal to
/// leave the approved root (ADR 0052).
#[derive(Clone)]
pub struct ProjectFiles {
    projects: Arc<dyn ProjectRepository>,
}

impl std::fmt::Debug for ProjectFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectFiles").finish_non_exhaustive()
    }
}

impl ProjectFiles {
    /// Read the instruction files of projects kept in `projects`.
    #[must_use]
    pub fn new(projects: Arc<dyn ProjectRepository>) -> Self {
        Self { projects }
    }
}

#[async_trait]
impl ProjectSourceReader for ProjectFiles {
    async fn read(&self, project_id: &ProjectId) -> Result<Vec<RawSource>, ErrorEnvelope> {
        let invalid = |message: String| {
            ErrorEnvelope::new(
                ErrorCode::InvalidRequest,
                message,
                CorrelationId::new("project-sources"),
            )
        };
        let project = self.projects.get(project_id).await.map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::PersistenceError,
                error.to_string(),
                CorrelationId::new("project-sources"),
            )
        })?;
        let [root] = project.roots.as_slice() else {
            return Err(invalid(format!(
                "project {project_id} has {} roots; configuration discovery supports the default single-root project case",
                project.roots.len()
            )));
        };
        // Canonicalized again here, not trusted from the record: the folder may
        // have been moved or replaced since it was approved, and a task must
        // not start on instructions read from somewhere else.
        let root = canonicalize_root(std::path::Path::new(root)).map_err(|error| {
            invalid(format!(
                "could not reopen the approved project root: {error}"
            ))
        })?;
        Ok(discover(&root))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use tramontana_core::contracts::{ProjectRecord, Timestamp};
    use tramontana_core::ports::RepositoryError;

    struct OneProject {
        roots: Vec<String>,
    }

    #[async_trait]
    impl ProjectRepository for OneProject {
        async fn create(&self, _record: &ProjectRecord) -> Result<(), RepositoryError> {
            unimplemented!("the reader only reads")
        }

        async fn get(&self, project_id: &ProjectId) -> Result<ProjectRecord, RepositoryError> {
            Ok(ProjectRecord::new(
                project_id.clone(),
                "Fixture",
                self.roots.clone(),
                Timestamp::new("2026-01-01T00:00:00.000Z"),
            ))
        }

        async fn list(&self) -> Result<Vec<ProjectRecord>, RepositoryError> {
            unimplemented!("the reader only reads")
        }
    }

    fn reader(roots: Vec<String>) -> ProjectFiles {
        ProjectFiles::new(Arc::new(OneProject { roots }))
    }

    #[tokio::test]
    async fn the_project_authored_files_are_read_from_the_approved_root() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("AGENTS.md"), "Write tests first.").unwrap();

        let sources = reader(vec![temp.path().to_string_lossy().into_owned()])
            .read(&ProjectId::new("project-1"))
            .await
            .unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].relative_path, "AGENTS.md");
        assert_eq!(sources[0].content, b"Write tests first.");
    }

    #[tokio::test]
    async fn a_root_that_is_gone_is_an_error_rather_than_an_empty_project() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("moved");
        let error = reader(vec![root.to_string_lossy().into_owned()])
            .read(&ProjectId::new("project-1"))
            .await
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(
            error.message.contains("could not reopen"),
            "a missing root must not read as a project with no instructions"
        );
    }
}
