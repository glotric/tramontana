//! Where schedules are kept (ADR 0063).

use async_trait::async_trait;
use sqlx::Row;
use tramontana_core::contracts::{
    ConversationId, ProjectId, SCHEMA_VERSION, Schedule, TaskId, Timestamp,
};
use tramontana_core::ports::{RepositoryError, ScheduleRepository};

use crate::Database;
use crate::repositories::{parse_json, to_json, unavailable};

/// Schedules, in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteScheduleRepository {
    database: Database,
}

impl SqliteScheduleRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ScheduleRepository for SqliteScheduleRepository {
    async fn list(&self) -> Result<Vec<Schedule>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT schedule_id, title, goal, project_id, conversation_id, cadence,
                    routing_mode, enabled, created_at, created_local, last_run,
                    last_task_id, schema_version
             FROM schedule ORDER BY created_at, schedule_id",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(unavailable)?;
        rows.iter()
            .map(|row| {
                let get = |column: &str| -> Result<String, RepositoryError> {
                    row.try_get(column).map_err(unavailable)
                };
                let last_run: Option<String> = row.try_get("last_run").map_err(unavailable)?;
                let schema_version: i64 = row.try_get("schema_version").map_err(unavailable)?;
                Ok(Schedule {
                    schema_version: u32::try_from(schema_version).unwrap_or(SCHEMA_VERSION),
                    schedule_id: get("schedule_id")?,
                    title: get("title")?,
                    goal: get("goal")?,
                    project_id: row
                        .try_get::<Option<String>, _>("project_id")
                        .map_err(unavailable)?
                        .map(ProjectId::new),
                    conversation_id: ConversationId::new(get("conversation_id")?),
                    cadence: parse_json(&get("cadence")?, "schedule cadence")?,
                    routing_mode: parse_json(&get("routing_mode")?, "schedule routing mode")?,
                    enabled: row.try_get::<bool, _>("enabled").map_err(unavailable)?,
                    created_at: Timestamp::new(get("created_at")?),
                    created_local: parse_json(&get("created_local")?, "schedule creation time")?,
                    last_run: last_run
                        .map(|value| parse_json(&value, "schedule last run"))
                        .transpose()?,
                    last_task_id: row
                        .try_get::<Option<String>, _>("last_task_id")
                        .map_err(unavailable)?
                        .map(TaskId::new),
                })
            })
            .collect()
    }

    async fn save(&self, schedule: &Schedule) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO schedule (
                 schedule_id, title, goal, project_id, conversation_id, cadence,
                 routing_mode, enabled, created_at, created_local, last_run,
                 last_task_id, schema_version
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(schedule_id) DO UPDATE SET
                 title = excluded.title,
                 goal = excluded.goal,
                 cadence = excluded.cadence,
                 routing_mode = excluded.routing_mode,
                 enabled = excluded.enabled,
                 last_run = excluded.last_run,
                 last_task_id = excluded.last_task_id,
                 schema_version = excluded.schema_version",
        )
        .bind(&schedule.schedule_id)
        .bind(&schedule.title)
        .bind(&schedule.goal)
        .bind(schedule.project_id.as_ref().map(ProjectId::as_str))
        .bind(schedule.conversation_id.as_str())
        .bind(to_json(&schedule.cadence, "schedule cadence")?)
        .bind(to_json(&schedule.routing_mode, "schedule routing mode")?)
        .bind(schedule.enabled)
        .bind(schedule.created_at.as_str())
        .bind(to_json(&schedule.created_local, "schedule creation time")?)
        .bind(
            schedule
                .last_run
                .as_ref()
                .map(|last| to_json(last, "schedule last run"))
                .transpose()?,
        )
        .bind(schedule.last_task_id.as_ref().map(TaskId::as_str))
        .bind(i64::from(schedule.schema_version))
        .execute(self.database.pool())
        .await
        .map_err(unavailable)
        .map(|_| ())
    }

    async fn delete(&self, schedule_id: &str) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM schedule WHERE schedule_id = ?")
            .bind(schedule_id)
            .execute(self.database.pool())
            .await
            .map_err(unavailable)?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound {
                kind: "schedule",
                id: schedule_id.to_owned(),
            });
        }
        Ok(())
    }
}
