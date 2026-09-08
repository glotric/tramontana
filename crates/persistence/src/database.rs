use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};

/// Why opening or migrating the database failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DatabaseError {
    /// The database file could not be opened.
    #[error("could not open the database: {0}")]
    Open(String),
    /// A migration failed and was rolled back.
    #[error("migration failed: {0}")]
    Migration(String),
}

/// An open connection pool with its schema migrated.
#[derive(Debug, Clone)]
pub struct Database {
    pool: Pool<Sqlite>,
}

impl Database {
    /// Open (creating if absent) the database at `path` and migrate it.
    ///
    /// # Errors
    ///
    /// [`DatabaseError::Open`] when the file cannot be opened, or
    /// [`DatabaseError::Migration`] when a migration fails.
    pub async fn open(path: &Path) -> Result<Self, DatabaseError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // WAL lets a reader proceed during a write, which matters because
            // the interface replays events while a task is still appending.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // Without this, SQLite does not enforce the REFERENCES clauses the
            // schema declares — they would be documentation, not constraints.
            .foreign_keys(true)
            // A concurrent writer waits briefly instead of failing instantly.
            .busy_timeout(std::time::Duration::from_secs(5));

        Self::connect(options, 4).await
    }

    /// Open a private in-memory database, migrated. For tests.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub async fn open_in_memory() -> Result<Self, DatabaseError> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .shared_cache(true)
            .foreign_keys(true);
        // Separate `:memory:` SQLite connections do not share a schema
        // reliably across all supported builds. A single pooled connection is
        // deterministic and is sufficient for unit tests.
        Self::connect(options, 1).await
    }

    async fn connect(
        options: SqliteConnectOptions,
        max_connections: u32,
    ) -> Result<Self, DatabaseError> {
        let pool = SqlitePoolOptions::new()
            // SQLite serializes writes anyway; a small pool keeps lock
            // contention visible as a busy timeout rather than as queueing
            // spread across many connections.
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .map_err(|error| DatabaseError::Open(error.to_string()))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|error| DatabaseError::Migration(error.to_string()))?;

        Ok(Self { pool })
    }

    /// Borrow the pool for a repository implementation.
    #[must_use]
    pub(crate) fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    /// Close the pool, waiting for in-flight statements to finish.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good setup; a panic is the failure report"
)]
mod tests {
    use tramontana_core::contracts::{ProjectId, ProjectRecord, Timestamp};
    use tramontana_core::ports::{ProjectRepository, RepositoryError};

    use super::*;
    use crate::repositories::SqliteProjectRepository;

    #[tokio::test]
    async fn a_locked_database_becomes_a_normalized_repository_error() {
        let directory = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(directory.path().join("locked.sqlite"))
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_millis(20));
        let database = Database::connect(options, 4).await.unwrap();
        let mut blocker = database.pool().begin().await.unwrap();
        sqlx::query(
            "INSERT INTO project (project_id, name, created_at) VALUES ('held', 'Held', 't')",
        )
        .execute(&mut *blocker)
        .await
        .unwrap();

        let repository = SqliteProjectRepository::new(database);
        let error = repository
            .create(&ProjectRecord::new(
                ProjectId::new("blocked"),
                "Blocked",
                Vec::new(),
                Timestamp::new("2026-01-01T00:00:00.000Z"),
            ))
            .await
            .unwrap_err();

        assert!(matches!(error, RepositoryError::Unavailable(_)));
        blocker.rollback().await.unwrap();
    }
}
