//! Removing a conversation's or a project's record in one commit (ADR 0067).

use async_trait::async_trait;
use tramontana_core::contracts::{ConversationId, ProjectId};
use tramontana_core::ports::{RecordRemoval, RepositoryError};

use crate::database::Database;

/// Conversations and projects, each removed with everything under it at once.
#[derive(Debug, Clone)]
pub struct SqliteRecordRemoval {
    database: Database,
}

impl SqliteRecordRemoval {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Run `events`, which deletes the events of the tasks under the record,
    /// then `parent`, which deletes the record's row, in one transaction.
    /// The schema cascades from that row to the rest; `task_event` has no
    /// foreign key, so its rows are named here rather than left to a cascade.
    async fn remove(
        &self,
        events: &'static str,
        parent: &'static str,
        id: &str,
        kind: &'static str,
    ) -> Result<(), RepositoryError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| unavailable(&error))?;
        sqlx::query(events)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;
        let removed = sqlx::query(parent)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;
        if removed.rows_affected() == 0 {
            // Dropping the transaction rolls the event delete back.
            return Err(RepositoryError::NotFound {
                kind,
                id: id.to_owned(),
            });
        }
        transaction
            .commit()
            .await
            .map_err(|error| unavailable(&error))
    }
}

fn unavailable(error: &sqlx::Error) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}

#[async_trait]
impl RecordRemoval for SqliteRecordRemoval {
    async fn remove_conversation(&self, id: &ConversationId) -> Result<(), RepositoryError> {
        self.remove(
            "DELETE FROM task_event WHERE task_id IN
                 (SELECT task_id FROM task WHERE conversation_id = ?1)",
            "DELETE FROM conversation WHERE conversation_id = ?1",
            id.as_str(),
            "conversation",
        )
        .await
    }

    async fn remove_project(&self, id: &ProjectId) -> Result<(), RepositoryError> {
        self.remove(
            "DELETE FROM task_event WHERE task_id IN
                 (SELECT task_id FROM task WHERE project_id = ?1
                  UNION SELECT t.task_id FROM task t
                        JOIN conversation c ON c.conversation_id = t.conversation_id
                        WHERE c.project_id = ?1)",
            "DELETE FROM project WHERE project_id = ?1",
            id.as_str(),
            "project",
        )
        .await
    }

    async fn remove_orphaned_events(&self) -> Result<u64, RepositoryError> {
        sqlx::query("DELETE FROM task_event WHERE task_id NOT IN (SELECT task_id FROM task)")
            .execute(self.database.pool())
            .await
            .map(|result| result.rows_affected())
            .map_err(|error| unavailable(&error))
    }
}
