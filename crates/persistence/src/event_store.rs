use async_trait::async_trait;
use sqlx::Row;
use tramontana_core::contracts::{
    ConversationId, CorrelationId, EventEnvelope, EventId, EventPayload, TaskEventKind, TaskId,
    Timestamp, Visibility,
};
use tramontana_core::ports::{EventStore, EventStoreError};

use crate::Database;

/// The durable event journal.
///
/// Implements exactly the semantics the in-memory fake does, and is verified by
/// the same contract suite: sequences start at 1 and increase by one, and an
/// append is atomic and optimistic. Here those properties are enforced twice —
/// once by this code and once by the `(task_id, sequence)` primary key, so a
/// bug in the former still cannot corrupt a stream.
#[derive(Debug, Clone)]
pub struct SqliteEventStore {
    database: Database,
}

impl SqliteEventStore {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

/// Map a database failure to the port's error type.
fn unavailable(error: &sqlx::Error) -> EventStoreError {
    EventStoreError::Unavailable(error.to_string())
}

#[async_trait]
impl EventStore for SqliteEventStore {
    async fn append(
        &self,
        task_id: &TaskId,
        expected_sequence: u64,
        events: Vec<EventEnvelope>,
    ) -> Result<u64, EventStoreError> {
        // Verify the batch before touching the database, so a misnumbered
        // append cannot write its valid prefix and then fail.
        for (offset, event) in events.iter().enumerate() {
            let expected = expected_sequence + offset as u64 + 1;
            if event.sequence != expected || event.task_id != *task_id {
                return Err(EventStoreError::NonConsecutive {
                    task_id: task_id.clone(),
                    expected_first: expected_sequence + 1,
                });
            }
        }

        // One transaction covers the check and the writes. Without it, two
        // writers could both read the same `actual` and both believe they won.
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|error| unavailable(&error))?;

        let actual: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) FROM task_event WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| unavailable(&error))?;

        let actual = u64::try_from(actual).unwrap_or(0);
        if actual != expected_sequence {
            return Err(EventStoreError::SequenceConflict {
                task_id: task_id.clone(),
                expected: expected_sequence,
                actual,
            });
        }

        let mut last = expected_sequence;
        for event in &events {
            let payload = serde_json::to_string(&event.payload).map_err(|error| {
                EventStoreError::Unavailable(format!("could not serialize payload: {error}"))
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
            .bind(
                serde_json::to_string(&event.kind)
                    .unwrap_or_default()
                    .trim_matches('"'),
            )
            .bind(
                serde_json::to_string(&event.visibility)
                    .unwrap_or_default()
                    .trim_matches('"'),
            )
            .bind(event.correlation_id.as_str())
            .bind(event.causation_id.as_deref())
            .bind(i64::from(event.schema_version))
            .bind(payload)
            .execute(&mut *transaction)
            .await
            .map_err(|error| unavailable(&error))?;

            last = event.sequence;
        }

        transaction
            .commit()
            .await
            .map_err(|error| unavailable(&error))?;

        Ok(last)
    }

    async fn replay(
        &self,
        task_id: &TaskId,
        after_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, EventStoreError> {
        let rows = sqlx::query(
            "SELECT sequence, event_id, conversation_id, occurred_at, kind, visibility,
                    correlation_id, causation_id, schema_version, payload
             FROM task_event
             WHERE task_id = ? AND sequence > ?
             ORDER BY sequence",
        )
        .bind(task_id.as_str())
        .bind(i64::try_from(after_sequence).unwrap_or(i64::MAX))
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        rows.into_iter()
            .map(|row| {
                let sequence: i64 = row.try_get("sequence").map_err(|e| unavailable(&e))?;
                let kind: String = row.try_get("kind").map_err(|e| unavailable(&e))?;
                let visibility: String = row.try_get("visibility").map_err(|e| unavailable(&e))?;
                let payload: String = row.try_get("payload").map_err(|e| unavailable(&e))?;
                let conversation_id: Option<String> = row
                    .try_get("conversation_id")
                    .map_err(|e| unavailable(&e))?;
                let schema_version: i64 =
                    row.try_get("schema_version").map_err(|e| unavailable(&e))?;

                Ok(EventEnvelope {
                    schema_version: u32::try_from(schema_version).unwrap_or(0),
                    event_id: EventId::new(
                        row.try_get::<String, _>("event_id")
                            .map_err(|e| unavailable(&e))?,
                    ),
                    task_id: task_id.clone(),
                    conversation_id: conversation_id.map(ConversationId::new),
                    sequence: u64::try_from(sequence).unwrap_or(0),
                    occurred_at: Timestamp::new(
                        row.try_get::<String, _>("occurred_at")
                            .map_err(|e| unavailable(&e))?,
                    ),
                    // An unknown kind or payload from a newer build is read as
                    // unsupported rather than failing the replay
                    // (`CORE_CONTRACTS.md` §2).
                    kind: parse_json_scalar(&kind).unwrap_or(TaskEventKind::DiagnosticRecorded),
                    visibility: parse_json_scalar(&visibility).unwrap_or(Visibility::Diagnostic),
                    correlation_id: CorrelationId::new(
                        row.try_get::<String, _>("correlation_id")
                            .map_err(|e| unavailable(&e))?,
                    ),
                    causation_id: row.try_get("causation_id").map_err(|e| unavailable(&e))?,
                    payload: serde_json::from_str::<EventPayload>(&payload)
                        .unwrap_or(EventPayload::Unsupported),
                })
            })
            .collect()
    }

    async fn last_sequence(&self, task_id: &TaskId) -> Result<u64, EventStoreError> {
        let sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) FROM task_event WHERE task_id = ?",
        )
        .bind(task_id.as_str())
        .fetch_one(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        Ok(u64::try_from(sequence).unwrap_or(0))
    }

    async fn delete_stream(&self, task_id: &TaskId) -> Result<(), EventStoreError> {
        sqlx::query("DELETE FROM task_event WHERE task_id = ?")
            .bind(task_id.as_str())
            .execute(self.database.pool())
            .await
            .map_err(|error| unavailable(&error))
            .map(|_| ())
    }
}

/// Parse a bare enum value that was stored without its JSON quotes.
fn parse_json_scalar<T: serde::de::DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_str(&format!("\"{value}\"")).ok()
}
