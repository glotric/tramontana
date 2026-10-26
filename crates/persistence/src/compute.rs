//! Where lease events are kept (ADR 0072).

use async_trait::async_trait;
use tramontana_core::contracts::{ComputeLeaseEvent, CorrelationId, ErrorCode, ErrorEnvelope};
use tramontana_core::ports::{ComputeLeaseJournal, RepositoryError};

use crate::Database;
use crate::repositories::{parse_json, to_json, unavailable};

/// Lease events, in SQLite, in the order they happened.
#[derive(Debug, Clone)]
pub struct SqliteComputeLeaseJournal {
    database: Database,
}

impl SqliteComputeLeaseJournal {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

fn envelope(error: &RepositoryError) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::Internal,
        error.to_string(),
        CorrelationId::new("compute-lease-journal"),
    )
}

#[async_trait]
impl ComputeLeaseJournal for SqliteComputeLeaseJournal {
    async fn append(&self, event: &ComputeLeaseEvent) -> Result<(), ErrorEnvelope> {
        sqlx::query("INSERT INTO compute_lease_events (lease_id, event) VALUES (?, ?)")
            .bind(event.lease_id().as_str())
            .bind(to_json(event, "lease event").map_err(|error| envelope(&error))?)
            .execute(self.database.pool())
            .await
            .map_err(|error| envelope(&unavailable(&error)))
            .map(|_| ())
    }

    async fn list(&self) -> Result<Vec<ComputeLeaseEvent>, ErrorEnvelope> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT event FROM compute_lease_events ORDER BY sequence")
                .fetch_all(self.database.pool())
                .await
                .map_err(|error| envelope(&unavailable(&error)))?;
        rows.iter()
            .map(|row| parse_json(row, "lease event").map_err(|error| envelope(&error)))
            .collect()
    }
}
