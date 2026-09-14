use async_trait::async_trait;
use sqlx::Row;
use tramontana_core::contracts::{
    CheckpointId, ConfigurationDiagnostic, ConfigurationKind, ConfigurationSource, ConversationId,
    ConversationRecord, DiagnosticSeverity, EffectiveConfiguration, EventEnvelope, ProjectId,
    ProjectRecord, ProviderEndpoint, SCHEMA_VERSION, TaskCheckpoint, TaskEventKind, TaskId,
    TaskRecord, TaskState, Timestamp,
};
use tramontana_core::ports::{
    CheckpointRepository, ConfigurationRepository, ConversationRepository, ProjectRepository,
    ProviderSettingsRepository, RepositoryError, TaskRepository,
};

use crate::Database;

/// Map a database failure to the port's error type.
fn unavailable(error: &sqlx::Error) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}

fn conflict_or_unavailable(
    error: &sqlx::Error,
    kind: &'static str,
    id: impl Into<String>,
) -> RepositoryError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        RepositoryError::Conflict {
            kind,
            id: id.into(),
        }
    } else {
        unavailable(error)
    }
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

/// Secret-free effective-configuration manifests.
#[derive(Debug, Clone)]
pub struct SqliteConfigurationRepository {
    database: Database,
}

impl SqliteConfigurationRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn read(
        &self,
        project_id: &ProjectId,
        manifest_hash: &str,
    ) -> Result<EffectiveConfiguration, RepositoryError> {
        let manifest = sqlx::query(
            "SELECT schema_version, resolved_at FROM configuration_manifest
             WHERE project_id = ? AND manifest_hash = ?",
        )
        .bind(project_id.as_str())
        .bind(manifest_hash)
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| not_found("configuration manifest", manifest_hash))?;

        let source_rows = sqlx::query(
            "SELECT source_id, relative_path, kind, declared_id, declared_name, scopes,
                    requested_capabilities, priority, selection_order, enabled, active,
                    precedence, content_hash, byte_count, unknown_keys, redactions, recipes
             FROM configuration_source
             WHERE project_id = ? AND manifest_hash = ?
             ORDER BY position",
        )
        .bind(project_id.as_str())
        .bind(manifest_hash)
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        let mut sources = Vec::with_capacity(source_rows.len());
        for row in source_rows {
            let kind: String = row.try_get("kind").map_err(|e| unavailable(&e))?;
            let scopes: String = row.try_get("scopes").map_err(|e| unavailable(&e))?;
            let requested: String = row
                .try_get("requested_capabilities")
                .map_err(|e| unavailable(&e))?;
            let unknown: String = row.try_get("unknown_keys").map_err(|e| unavailable(&e))?;
            let redactions: String = row.try_get("redactions").map_err(|e| unavailable(&e))?;
            let recipes: String = row.try_get("recipes").map_err(|e| unavailable(&e))?;
            let priority: Option<i64> = row.try_get("priority").map_err(|e| unavailable(&e))?;
            let selection_order: Option<i64> = row
                .try_get("selection_order")
                .map_err(|e| unavailable(&e))?;
            let precedence: i64 = row.try_get("precedence").map_err(|e| unavailable(&e))?;
            let byte_count: i64 = row.try_get("byte_count").map_err(|e| unavailable(&e))?;

            sources.push(ConfigurationSource {
                source_id: row.try_get("source_id").map_err(|e| unavailable(&e))?,
                relative_path: row.try_get("relative_path").map_err(|e| unavailable(&e))?,
                kind: parse_scalar(&kind).unwrap_or(ConfigurationKind::Unsupported),
                declared_id: row.try_get("declared_id").map_err(|e| unavailable(&e))?,
                declared_name: row.try_get("declared_name").map_err(|e| unavailable(&e))?,
                scopes: parse_json(&scopes, "configuration scopes")?,
                requested_capabilities: parse_json(
                    &requested,
                    "configuration requested capabilities",
                )?,
                priority,
                selection_order: selection_order.and_then(|value| u32::try_from(value).ok()),
                enabled: row
                    .try_get::<bool, _>("enabled")
                    .map_err(|e| unavailable(&e))?,
                active: row
                    .try_get::<bool, _>("active")
                    .map_err(|e| unavailable(&e))?,
                precedence: u32::try_from(precedence).unwrap_or(u32::MAX),
                content_hash: row.try_get("content_hash").map_err(|e| unavailable(&e))?,
                byte_count: u64::try_from(byte_count).unwrap_or(0),
                redactions: parse_json(&redactions, "configuration redactions")?,
                unknown_keys: parse_json(&unknown, "configuration unknown keys")?,
                recipes: parse_json(&recipes, "configuration recipes")?,
            });
        }

        let diagnostic_rows = sqlx::query(
            "SELECT severity, relative_path, message
             FROM configuration_diagnostic
             WHERE project_id = ? AND manifest_hash = ?
             ORDER BY position",
        )
        .bind(project_id.as_str())
        .bind(manifest_hash)
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;
        let mut diagnostics = Vec::with_capacity(diagnostic_rows.len());
        for row in diagnostic_rows {
            let severity: String = row.try_get("severity").map_err(|e| unavailable(&e))?;
            diagnostics.push(ConfigurationDiagnostic {
                severity: parse_scalar(&severity).unwrap_or(DiagnosticSeverity::Warning),
                relative_path: row.try_get("relative_path").map_err(|e| unavailable(&e))?,
                message: row.try_get("message").map_err(|e| unavailable(&e))?,
            });
        }

        Ok(EffectiveConfiguration {
            schema_version: u32::try_from(
                manifest
                    .try_get::<i64, _>("schema_version")
                    .map_err(|e| unavailable(&e))?,
            )
            .unwrap_or(0),
            sources,
            diagnostics,
            manifest_hash: manifest_hash.to_owned(),
            resolved_at: Timestamp::new(
                manifest
                    .try_get::<String, _>("resolved_at")
                    .map_err(|e| unavailable(&e))?,
            ),
        })
    }
}

#[async_trait]
impl ConfigurationRepository for SqliteConfigurationRepository {
    async fn save(
        &self,
        project_id: &ProjectId,
        configuration: &EffectiveConfiguration,
    ) -> Result<(), RepositoryError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| unavailable(&error))?;

        sqlx::query(
            "INSERT INTO configuration_manifest
                (project_id, manifest_hash, resolved_at, schema_version)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(project_id, manifest_hash) DO UPDATE SET
                resolved_at = excluded.resolved_at,
                schema_version = excluded.schema_version",
        )
        .bind(project_id.as_str())
        .bind(&configuration.manifest_hash)
        .bind(configuration.resolved_at.as_str())
        .bind(i64::from(configuration.schema_version))
        .execute(&mut *transaction)
        .await
        .map_err(|error| unavailable(&error))?;

        sqlx::query("DELETE FROM configuration_source WHERE project_id = ? AND manifest_hash = ?")
            .bind(project_id.as_str())
            .bind(&configuration.manifest_hash)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;
        sqlx::query(
            "DELETE FROM configuration_diagnostic WHERE project_id = ? AND manifest_hash = ?",
        )
        .bind(project_id.as_str())
        .bind(&configuration.manifest_hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| unavailable(&error))?;

        for (position, source) in configuration.sources.iter().enumerate() {
            sqlx::query(
                "INSERT INTO configuration_source (
                     project_id, manifest_hash, position, source_id, relative_path, kind,
                     declared_id, declared_name, scopes, requested_capabilities, priority,
                     selection_order, enabled, active, precedence, content_hash, byte_count,
                     unknown_keys, redactions, recipes
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(project_id.as_str())
            .bind(&configuration.manifest_hash)
            .bind(i64::try_from(position).unwrap_or(i64::MAX))
            .bind(&source.source_id)
            .bind(&source.relative_path)
            .bind(scalar(&source.kind))
            .bind(source.declared_id.as_deref())
            .bind(source.declared_name.as_deref())
            .bind(to_json(&source.scopes, "configuration scopes")?)
            .bind(to_json(
                &source.requested_capabilities,
                "configuration requested capabilities",
            )?)
            .bind(source.priority)
            .bind(source.selection_order.map(i64::from))
            .bind(source.enabled)
            .bind(source.active)
            .bind(i64::from(source.precedence))
            .bind(&source.content_hash)
            .bind(i64::try_from(source.byte_count).unwrap_or(i64::MAX))
            .bind(to_json(&source.unknown_keys, "configuration unknown keys")?)
            .bind(to_json(&source.redactions, "configuration redactions")?)
            .bind(to_json(&source.recipes, "configuration recipes")?)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;
        }

        for (position, diagnostic) in configuration.diagnostics.iter().enumerate() {
            sqlx::query(
                "INSERT INTO configuration_diagnostic (
                     project_id, manifest_hash, position, severity, relative_path, message
                 ) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(project_id.as_str())
            .bind(&configuration.manifest_hash)
            .bind(i64::try_from(position).unwrap_or(i64::MAX))
            .bind(scalar(&diagnostic.severity))
            .bind(&diagnostic.relative_path)
            .bind(&diagnostic.message)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| unavailable(&error))
    }

    async fn get(
        &self,
        project_id: &ProjectId,
        manifest_hash: &str,
    ) -> Result<EffectiveConfiguration, RepositoryError> {
        self.read(project_id, manifest_hash).await
    }

    async fn latest(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<EffectiveConfiguration>, RepositoryError> {
        let hash = sqlx::query_scalar::<_, String>(
            "SELECT manifest_hash FROM configuration_manifest
             WHERE project_id = ?
             ORDER BY resolved_at DESC, manifest_hash DESC
             LIMIT 1",
        )
        .bind(project_id.as_str())
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        match hash {
            Some(hash) => self.read(project_id, &hash).await.map(Some),
            None => Ok(None),
        }
    }
}

fn to_json<T: serde::Serialize>(value: &T, label: &str) -> Result<String, RepositoryError> {
    serde_json::to_string(value).map_err(|error| {
        RepositoryError::Unavailable(format!("could not serialize {label}: {error}"))
    })
}

fn parse_json<T: serde::de::DeserializeOwned>(
    value: &str,
    label: &str,
) -> Result<T, RepositoryError> {
    serde_json::from_str(value)
        .map_err(|error| RepositoryError::Unavailable(format!("could not read {label}: {error}")))
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
        configuration_manifest_hash: row
            .try_get("configuration_manifest_hash")
            .map_err(|e| unavailable(&e))?,
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
            "INSERT INTO task (task_id, conversation_id, project_id, goal_summary,
                               configuration_manifest_hash, state, last_sequence,
                               created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.task_id.as_str())
        .bind(record.conversation_id.as_ref().map(ConversationId::as_str))
        .bind(record.project_id.as_ref().map(ProjectId::as_str))
        .bind(&record.goal_summary)
        .bind(record.configuration_manifest_hash.as_deref())
        .bind(scalar(&record.state))
        .bind(i64::try_from(record.last_sequence).unwrap_or(i64::MAX))
        .bind(record.created_at.as_str())
        .bind(record.updated_at.as_str())
        .execute(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))
        .map(|_| ())
    }

    async fn create_with_initial_events(
        &self,
        record: &TaskRecord,
        events: &[EventEnvelope],
    ) -> Result<(), RepositoryError> {
        if events.is_empty()
            || events.iter().enumerate().any(|(index, event)| {
                event.task_id != record.task_id
                    || event.conversation_id != record.conversation_id
                    || event.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
            })
            || !matches!(
                events.first().map(|event| (&event.kind, &event.payload)),
                Some((
                    TaskEventKind::TaskCreated,
                    tramontana_core::contracts::EventPayload::TaskCreated {
                        goal_summary,
                        conversation_id,
                    }
                )) if goal_summary == &record.goal_summary
                    && conversation_id == &record.conversation_id
            )
        {
            return Err(RepositoryError::Unavailable(
                "initial task events must be consecutive from sequence 1 for the new task"
                    .to_owned(),
            ));
        }
        let last_event = events.last().ok_or_else(|| {
            RepositoryError::Unavailable("initial task event batch is empty".to_owned())
        })?;

        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| unavailable(&error))?;

        sqlx::query(
            "INSERT INTO task (task_id, conversation_id, project_id, goal_summary, state,
                               last_sequence, created_at, updated_at, configuration_manifest_hash)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.task_id.as_str())
        .bind(record.conversation_id.as_ref().map(ConversationId::as_str))
        .bind(record.project_id.as_ref().map(ProjectId::as_str))
        .bind(&record.goal_summary)
        .bind(scalar(&record.state))
        .bind(i64::try_from(events.len()).unwrap_or(i64::MAX))
        .bind(record.created_at.as_str())
        .bind(last_event.occurred_at.as_str())
        .bind(record.configuration_manifest_hash.as_deref())
        .execute(&mut *transaction)
        .await
        .map_err(|error| conflict_or_unavailable(&error, "task", record.task_id.to_string()))?;

        for event in events {
            let payload = serde_json::to_string(&event.payload).map_err(|error| {
                RepositoryError::Unavailable(format!("could not serialize initial event: {error}"))
            })?;
            sqlx::query(
                "INSERT INTO task_event (
                     task_id, sequence, event_id, conversation_id, occurred_at,
                     kind, visibility, correlation_id, causation_id, schema_version, payload
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(event.task_id.as_str())
            .bind(i64::try_from(event.sequence).unwrap_or(i64::MAX))
            .bind(event.event_id.as_str())
            .bind(event.conversation_id.as_ref().map(ConversationId::as_str))
            .bind(event.occurred_at.as_str())
            .bind(scalar(&event.kind))
            .bind(scalar(&event.visibility))
            .bind(event.correlation_id.as_str())
            .bind(event.causation_id.as_deref())
            .bind(i64::from(event.schema_version))
            .bind(payload)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                conflict_or_unavailable(&error, "event", event.event_id.to_string())
            })?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| unavailable(&error))
    }

    async fn get(&self, task_id: &TaskId) -> Result<TaskRecord, RepositoryError> {
        let row = sqlx::query(
            "SELECT task_id, conversation_id, project_id, goal_summary,
                    configuration_manifest_hash, state,
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
                    "SELECT task_id, conversation_id, project_id, goal_summary,
                            configuration_manifest_hash, state,
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
                    "SELECT task_id, conversation_id, project_id, goal_summary,
                            configuration_manifest_hash, state,
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
                    t.updated_at AS cached_updated_at,
                    COALESCE(MAX(e.sequence), 0) AS actual_sequence,
                    COALESCE(
                        (SELECT latest.occurred_at FROM task_event latest
                         WHERE latest.task_id = t.task_id
                         ORDER BY latest.sequence DESC LIMIT 1),
                        t.created_at
                    ) AS actual_updated_at
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
            let cached_updated_at: String = row
                .try_get("cached_updated_at")
                .map_err(|e| unavailable(&e))?;
            let actual_updated_at: String = row
                .try_get("actual_updated_at")
                .map_err(|e| unavailable(&e))?;

            let actual_state = self.state_from_events(&task_id).await?;
            let actual_name =
                actual_state.map_or_else(|| cached_state.clone(), |state| scalar(&state));

            if cached_sequence == actual_sequence
                && cached_state == actual_name
                && cached_updated_at == actual_updated_at
            {
                continue;
            }

            sqlx::query(
                "UPDATE task SET state = ?, last_sequence = ?, updated_at = ? WHERE task_id = ?",
            )
            .bind(&actual_name)
            .bind(actual_sequence)
            .bind(actual_updated_at)
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

/// Durable task checkpoints.
#[derive(Debug, Clone)]
pub struct SqliteCheckpointRepository {
    database: Database,
}

impl SqliteCheckpointRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Rebuild a checkpoint from its stored document.
    ///
    /// The indexed columns exist to query by; the document is the record. A
    /// row whose document no longer parses is reported as storage failure
    /// rather than silently skipped, because a checkpoint that cannot be read
    /// is exactly the situation the user needs told about.
    fn decode(row: &sqlx::sqlite::SqliteRow) -> Result<TaskCheckpoint, RepositoryError> {
        let document: String = row.get("checkpoint");
        serde_json::from_str(&document).map_err(|error| {
            RepositoryError::Unavailable(format!("stored checkpoint could not be read: {error}"))
        })
    }
}

#[async_trait]
impl CheckpointRepository for SqliteCheckpointRepository {
    async fn save(&self, checkpoint: &TaskCheckpoint) -> Result<(), RepositoryError> {
        let document = serde_json::to_string(checkpoint).map_err(|error| {
            RepositoryError::Unavailable(format!("checkpoint could not be serialized: {error}"))
        })?;

        sqlx::query(
            "INSERT INTO task_checkpoint (
                 checkpoint_id, task_id, event_sequence, created_at, task_state,
                 configuration_manifest_hash, checkpoint
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(checkpoint.checkpoint_id.as_str())
        .bind(checkpoint.task_id.as_str())
        .bind(i64::try_from(checkpoint.event_sequence).unwrap_or(i64::MAX))
        .bind(checkpoint.created_at.as_str())
        .bind(scalar(&checkpoint.task_state))
        .bind(checkpoint.configuration_manifest_hash.as_deref())
        .bind(&document)
        .execute(self.database.pool())
        .await
        .map_err(|error| {
            conflict_or_unavailable(
                &error,
                "checkpoint",
                format!("{}@{}", checkpoint.task_id, checkpoint.event_sequence),
            )
        })?;

        Ok(())
    }

    async fn get(&self, checkpoint_id: &CheckpointId) -> Result<TaskCheckpoint, RepositoryError> {
        let row = sqlx::query("SELECT checkpoint FROM task_checkpoint WHERE checkpoint_id = ?")
            .bind(checkpoint_id.as_str())
            .fetch_optional(self.database.pool())
            .await
            .map_err(|error| unavailable(&error))?
            .ok_or_else(|| not_found("checkpoint", checkpoint_id))?;

        Self::decode(&row)
    }

    async fn latest(&self, task_id: &TaskId) -> Result<Option<TaskCheckpoint>, RepositoryError> {
        let row = sqlx::query(
            "SELECT checkpoint FROM task_checkpoint
             WHERE task_id = ?
             ORDER BY event_sequence DESC
             LIMIT 1",
        )
        .bind(task_id.as_str())
        .fetch_optional(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        row.as_ref().map(Self::decode).transpose()
    }

    async fn list(&self, task_id: &TaskId) -> Result<Vec<TaskCheckpoint>, RepositoryError> {
        sqlx::query(
            "SELECT checkpoint FROM task_checkpoint
             WHERE task_id = ?
             ORDER BY event_sequence ASC",
        )
        .bind(task_id.as_str())
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?
        .iter()
        .map(Self::decode)
        .collect()
    }
}

/// Model providers configured on this machine.
#[derive(Debug, Clone)]
pub struct SqliteProviderSettingsRepository {
    database: Database,
}

impl SqliteProviderSettingsRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProviderSettingsRepository for SqliteProviderSettingsRepository {
    async fn list(&self) -> Result<Vec<ProviderEndpoint>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT endpoint_id, display_name, enabled, config, created_at, schema_version, supports_tool_calls
             FROM provider_endpoint
             ORDER BY created_at, endpoint_id",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        let mut endpoints = Vec::with_capacity(rows.len());
        for row in rows {
            let config: String = row.try_get("config").map_err(|e| unavailable(&e))?;
            let schema_version: i64 = row.try_get("schema_version").map_err(|e| unavailable(&e))?;
            endpoints.push(ProviderEndpoint {
                supports_tool_calls: row
                    .try_get::<bool, _>("supports_tool_calls")
                    .map_err(|e| unavailable(&e))?,
                schema_version: u32::try_from(schema_version).unwrap_or(SCHEMA_VERSION),
                endpoint_id: row.try_get("endpoint_id").map_err(|e| unavailable(&e))?,
                display_name: row.try_get("display_name").map_err(|e| unavailable(&e))?,
                enabled: row
                    .try_get::<bool, _>("enabled")
                    .map_err(|e| unavailable(&e))?,
                config: parse_json(&config, "provider configuration")?,
                created_at: Timestamp::new(
                    row.try_get::<String, _>("created_at")
                        .map_err(|e| unavailable(&e))?,
                ),
            });
        }
        Ok(endpoints)
    }

    async fn save(&self, endpoint: &ProviderEndpoint) -> Result<(), RepositoryError> {
        // Replace rather than insert-or-fail: editing an endpoint is the
        // normal case, and making the caller delete first would leave a
        // window with no configuration.
        sqlx::query(
            "INSERT INTO provider_endpoint (
                 endpoint_id, display_name, enabled, kind, config, created_at, schema_version, supports_tool_calls
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(endpoint_id) DO UPDATE SET
                 display_name = excluded.display_name,
                 enabled = excluded.enabled,
                 kind = excluded.kind,
                 config = excluded.config,
                 supports_tool_calls = excluded.supports_tool_calls",
        )
        .bind(&endpoint.endpoint_id)
        .bind(&endpoint.display_name)
        .bind(endpoint.enabled)
        .bind(endpoint.config.kind())
        .bind(to_json(&endpoint.config, "provider configuration")?)
        .bind(endpoint.created_at.as_str())
        .bind(i64::from(endpoint.schema_version))
        .bind(endpoint.supports_tool_calls)
        .execute(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;
        Ok(())
    }

    async fn delete(&self, endpoint_id: &str) -> Result<bool, RepositoryError> {
        let result = sqlx::query("DELETE FROM provider_endpoint WHERE endpoint_id = ?")
            .bind(endpoint_id)
            .execute(self.database.pool())
            .await
            .map_err(|error| unavailable(&error))?;
        Ok(result.rows_affected() > 0)
    }
}
