//! Erasing every record in a profile (ADR 0068).

use async_trait::async_trait;
use sqlx::Row as _;
use tramontana_core::ports::{ProfileEraser, RepositoryError};

use crate::database::Database;
use crate::repositories::unavailable;

/// The profile's records, removed all at once.
#[derive(Debug, Clone)]
pub struct SqliteProfileEraser {
    database: Database,
}

impl SqliteProfileEraser {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProfileEraser for SqliteProfileEraser {
    async fn erase_everything(&self) -> Result<(), RepositoryError> {
        let pool = self.database.pool();
        let mut transaction = pool.begin().await.map_err(unavailable)?;
        // Every table, read from the schema rather than listed here: a table
        // a later migration adds is erased without anyone remembering to add
        // it. Only SQLite's own tables and the migration ledger are kept, so
        // the profile opens again as new rather than as unmigrated.
        let tables: Vec<String> = sqlx::query(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(unavailable)?
        .iter()
        .map(|row| row.try_get::<String, _>("name"))
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
        // Every row goes, so every reference is satisfied at commit; checked
        // there rather than after each table, whose order would otherwise
        // matter.
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        for table in &tables {
            // The name comes from the schema, never from a caller, and is
            // quoted as an identifier besides.
            let quoted = table.replace('"', "\"\"");
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM \"{quoted}\"")))
                .execute(&mut *transaction)
                .await
                .map_err(unavailable)?;
        }
        transaction.commit().await.map_err(unavailable)?;

        // Deleted rows stay in the file's free pages, and in the write-ahead
        // log, until something overwrites them. Rewriting the file and
        // emptying the log is what makes "deleted" true of the file as well
        // as of every query.
        sqlx::query("VACUUM")
            .execute(pool)
            .await
            .map_err(unavailable)?;
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
