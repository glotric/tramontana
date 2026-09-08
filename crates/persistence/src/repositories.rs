use async_trait::async_trait;
use sqlx::Row;
use tramontana_core::contracts::{
    ConversationId, ConversationRecord, ProjectId, ProjectRecord, TaskId, TaskRecord, TaskState,
    Timestamp,
};
use tramontana_core::ports::{
    ConversationRepository, ProjectRepository, RepositoryError, TaskRepository,
};

use crate::Database;

/// Map a database failure to the port's error type.
fn unavailable(error: &sqlx::Error) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}

fn not_found(kind: &'static str, id: impl std::fmt::Display) -> RepositoryError {
    RepositoryError::NotFound {
        kind,
        id: id.to_string(),
    }
}

/// Serialize an enum to its bare camelCase name for storage.
fn scalar<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

/// Parse a bare enum value stored without its JSON quotes.
fn parse_scalar<T: serde::de::DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_str(&format!("\"{value}\"")).ok()
}

/// Projects and their approved roots.
#[derive(Debug, Clone)]
pub struct SqliteProjectRepository {
    database: Database,
}

impl SqliteProjectRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProjectRepository for SqliteProjectRepository {
    async fn create(&self, record: &ProjectRecord) -> Result<(), RepositoryError> {
        // Project and roots land together: a project whose roots half-committed
        // would grant access nobody approved, or none at all.
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| unavailable(&error))?;

        sqlx::query("INSERT INTO project (project_id, name, created_at) VALUES (?, ?, ?)")
            .bind(record.project_id.as_str())
            .bind(&record.name)
            .bind(record.created_at.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;

        for root in &record.roots {
            sqlx::query(
                "INSERT INTO project_root (project_id, canonical_path, granted_at)
                 VALUES (?, ?, ?)",
            )
            .bind(record.project_id.as_str())
            .bind(root)
            .bind(record.created_at.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| unavailable(&error))
    }

    async fn get(&self, project_id: &ProjectId) -> Result<ProjectRecord, RepositoryError> {
        let row = sqlx::query("SELECT name, created_at FROM project WHERE project_id = ?")
            .bind(project_id.as_str())
            .fetch_optional(self.database.pool())
            .await
            .map_err(|error| unavailable(&error))?
            .ok_or_else(|| not_found("project", project_id))?;

        let roots = self.roots_for(project_id).await?;
        Ok(ProjectRecord {
            schema_version: tramontana_core::SCHEMA_VERSION,
            project_id: project_id.clone(),
            name: row.try_get("name").map_err(|e| unavailable(&e))?,
            roots,
            created_at: Timestamp::new(
                row.try_get::<String, _>("created_at")
                    .map_err(|e| unavailable(&e))?,
            ),
        })
    }

    async fn list(&self) -> Result<Vec<ProjectRecord>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT project_id, name, created_at FROM project ORDER BY created_at DESC, project_id",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        let mut projects = Vec::with_capacity(rows.len());
        for row in rows {
            let project_id = ProjectId::new(
                row.try_get::<String, _>("project_id")
                    .map_err(|e| unavailable(&e))?,
            );
            let roots = self.roots_for(&project_id).await?;
            projects.push(ProjectRecord {
                schema_version: tramontana_core::SCHEMA_VERSION,
                project_id,
                name: row.try_get("name").map_err(|e| unavailable(&e))?,
                roots,
                created_at: Timestamp::new(
                    row.try_get::<String, _>("created_at")
                        .map_err(|e| unavailable(&e))?,
                ),
            });
        }
        Ok(projects)
    }
}

impl SqliteProjectRepository {
    async fn roots_for(&self, project_id: &ProjectId) -> Result<Vec<String>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT canonical_path FROM project_root WHERE project_id = ? ORDER BY canonical_path",
        )
        .bind(project_id.as_str())
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        rows.into_iter()
            .map(|row| row.try_get("canonical_path").map_err(|e| unavailable(&e)))
            .collect()
    }
}

/// Conversations.
#[derive(Debug, Clone)]
pub struct SqliteConversationRepository {
    database: Database,
}

impl SqliteConversationRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

fn conversation_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ConversationRecord, RepositoryError> {
    Ok(ConversationRecord {
        schema_version: tramontana_core::SCHEMA_VERSION,
        conversation_id: ConversationId::new(
            row.try_get::<String, _>("conversation_id")
                .map_err(|e| unavailable(&e))?,
        ),
        project_id: row
            .try_get::<Option<String>, _>("project_id")
            .map_err(|e| unavailable(&e))?
            .map(ProjectId::new),
        title: row.try_get("title").map_err(|e| unavailable(&e))?,
        created_at: Timestamp::new(
            row.try_get::<String, _>("created_at")
                .map_err(|e| unavailable(&e))?,
        ),
        archived_at: row
            .try_get::<Option<String>, _>("archived_at")
            .map_err(|e| unavailable(&e))?
            .map(Timestamp::new),
    })
}

#[async_trait]
impl ConversationRepository for SqliteConversationRepository {
    async fn create(&self, record: &ConversationRecord) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO conversation (conversation_id, project_id, title, created_at, archived_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(record.conversation_id.as_str())
        .bind(record.project_id.as_ref().map(ProjectId::as_str))
        .bind(&record.title)
        .bind(record.created_at.as_str())
        .bind(record.archived_at.as_ref().map(Timestamp::as_str))
        .execute(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))
        .map(|_| ())
    }

    async fn get(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<ConversationRecord, RepositoryError> {
        let row = sqlx::query(
            "SELECT conversation_id, project_id, title, created_at, archived_at
             FROM conversation WHERE conversation_id = ?",
        )
        .bind(conversation_id.as_str())
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| not_found("conversation", conversation_id))?;

        conversation_from_row(&row)
    }

    async fn list(
        &self,
        project_id: Option<&ProjectId>,
    ) -> Result<Vec<ConversationRecord>, RepositoryError> {
        // `IS` rather than `=` so that passing no project matches the
        // projectless rows: in SQL, `NULL = NULL` is not true.
        let rows = sqlx::query(
            "SELECT conversation_id, project_id, title, created_at, archived_at
             FROM conversation WHERE project_id IS ?
             ORDER BY created_at DESC, conversation_id",
        )
        .bind(project_id.map(ProjectId::as_str))
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        rows.iter().map(conversation_from_row).collect()
    }

    async fn archive(
        &self,
        conversation_id: &ConversationId,
        at: &Timestamp,
    ) -> Result<(), RepositoryError> {
        let result =
            sqlx::query("UPDATE conversation SET archived_at = ? WHERE conversation_id = ?")
                .bind(at.as_str())
                .bind(conversation_id.as_str())
                .execute(self.database.pool())
                .await
                .map_err(|error| unavailable(&error))?;

        if result.rows_affected() == 0 {
            return Err(not_found("conversation", conversation_id));
        }
        Ok(())
    }
}

/// Task rows and their cached projection.
#[derive(Debug, Clone)]
pub struct SqliteTaskRepository {
    database: Database,
}

impl SqliteTaskRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

fn task_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TaskRecord, RepositoryError> {
    let state: String = row.try_get("state").map_err(|e| unavailable(&e))?;
    let last_sequence: i64 = row.try_get("last_sequence").map_err(|e| unavailable(&e))?;
    Ok(TaskRecord {
        schema_version: tramontana_core::SCHEMA_VERSION,
        task_id: TaskId::new(
            row.try_get::<String, _>("task_id")
                .map_err(|e| unavailable(&e))?,
        ),
        conversation_id: row
            .try_get::<Option<String>, _>("conversation_id")
            .map_err(|e| unavailable(&e))?
            .map(ConversationId::new),
        project_id: row
            .try_get::<Option<String>, _>("project_id")
            .map_err(|e| unavailable(&e))?
            .map(ProjectId::new),
        goal_summary: row.try_get("goal_summary").map_err(|e| unavailable(&e))?,
        // An unrecognized state from a newer build is read as interrupted
        // rather than failing the load: the task needs a human decision, which
        // is exactly what `interrupted` means.
        state: parse_scalar(&state).unwrap_or(TaskState::Interrupted),
        last_sequence: u64::try_from(last_sequence).unwrap_or(0),
        created_at: Timestamp::new(
            row.try_get::<String, _>("created_at")
                .map_err(|e| unavailable(&e))?,
        ),
        updated_at: Timestamp::new(
            row.try_get::<String, _>("updated_at")
                .map_err(|e| unavailable(&e))?,
        ),
    })
}

// Column lists are written out in each query rather than interpolated: sqlx
// refuses runtime-built SQL, and spelling them out keeps every statement
// readable as the literal it is.
#[async_trait]
impl TaskRepository for SqliteTaskRepository {
    async fn create(&self, record: &TaskRecord) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO task (task_id, conversation_id, project_id, goal_summary, state,
                               last_sequence, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.task_id.as_str())
        .bind(record.conversation_id.as_ref().map(ConversationId::as_str))
        .bind(record.project_id.as_ref().map(ProjectId::as_str))
        .bind(&record.goal_summary)
        .bind(scalar(&record.state))
        .bind(i64::try_from(record.last_sequence).unwrap_or(i64::MAX))
        .bind(record.created_at.as_str())
        .bind(record.updated_at.as_str())
        .execute(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))
        .map(|_| ())
    }

    async fn get(&self, task_id: &TaskId) -> Result<TaskRecord, RepositoryError> {
        let row = sqlx::query(
            "SELECT task_id, conversation_id, project_id, goal_summary, state,
                    last_sequence, created_at, updated_at
             FROM task WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| not_found("task", task_id))?;
        task_from_row(&row)
    }

    async fn list(
        &self,
        conversation_id: Option<&ConversationId>,
    ) -> Result<Vec<TaskRecord>, RepositoryError> {
        let rows = match conversation_id {
            Some(id) => {
                sqlx::query(
                    "SELECT task_id, conversation_id, project_id, goal_summary, state,
                            last_sequence, created_at, updated_at
                     FROM task WHERE conversation_id = ?
                     ORDER BY created_at DESC, task_id",
                )
                .bind(id.as_str())
                .fetch_all(self.database.pool())
                .await
            }
            None => {
                sqlx::query(
                    "SELECT task_id, conversation_id, project_id, goal_summary, state,
                            last_sequence, created_at, updated_at
                     FROM task ORDER BY created_at DESC, task_id",
                )
                .fetch_all(self.database.pool())
                .await
            }
        }
        .map_err(|error| unavailable(&error))?;

        rows.iter().map(task_from_row).collect()
    }

    async fn update_projection(
        &self,
        task_id: &TaskId,
        state: TaskState,
        last_sequence: u64,
        at: &Timestamp,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE task SET state = ?, last_sequence = ?, updated_at = ? WHERE task_id = ?",
        )
        .bind(scalar(&state))
        .bind(i64::try_from(last_sequence).unwrap_or(i64::MAX))
        .bind(at.as_str())
        .bind(task_id.as_str())
        .execute(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        if result.rows_affected() == 0 {
            return Err(not_found("task", task_id));
        }
        Ok(())
    }

    async fn rebuild_from_events(&self) -> Result<usize, RepositoryError> {
        // The events are the record; this row is a cache. A process that died
        // between appending an event and updating the cache leaves the two
        // disagreeing, so at startup the cache is recomputed rather than
        // trusted.
        let rows = sqlx::query(
            "SELECT t.task_id AS task_id,
                    t.state AS cached_state,
                    t.last_sequence AS cached_sequence,
                    COALESCE(MAX(e.sequence), 0) AS actual_sequence
             FROM task t
             LEFT JOIN task_event e ON e.task_id = t.task_id
             GROUP BY t.task_id",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        let mut corrected = 0;
        for row in rows {
            let task_id: String = row.try_get("task_id").map_err(|e| unavailable(&e))?;
            let cached_sequence: i64 = row
                .try_get("cached_sequence")
                .map_err(|e| unavailable(&e))?;
            let actual_sequence: i64 = row
                .try_get("actual_sequence")
                .map_err(|e| unavailable(&e))?;
            let cached_state: String = row.try_get("cached_state").map_err(|e| unavailable(&e))?;

            let actual_state = self.state_from_events(&task_id).await?;
            let actual_name =
                actual_state.map_or_else(|| cached_state.clone(), |state| scalar(&state));

            if cached_sequence == actual_sequence && cached_state == actual_name {
                continue;
            }

            sqlx::query("UPDATE task SET state = ?, last_sequence = ? WHERE task_id = ?")
                .bind(&actual_name)
                .bind(actual_sequence)
                .bind(&task_id)
                .execute(self.database.pool())
                .await
                .map_err(|error| unavailable(&error))?;
            corrected += 1;
        }

        Ok(corrected)
    }
}

impl SqliteTaskRepository {
    /// The state implied by a task's most recent `taskStateChanged` event.
    async fn state_from_events(&self, task_id: &str) -> Result<Option<TaskState>, RepositoryError> {
        let row = sqlx::query(
            "SELECT payload FROM task_event
             WHERE task_id = ? AND kind = 'taskStateChanged'
             ORDER BY sequence DESC LIMIT 1",
        )
        .bind(task_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        let Some(row) = row else { return Ok(None) };
        let payload: String = row.try_get("payload").map_err(|e| unavailable(&e))?;
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
        Ok(parsed
            .get("current")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_scalar))
    }
}
