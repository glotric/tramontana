//! Where the dictation setup is kept (ADR 0065).

use async_trait::async_trait;
use tramontana_core::contracts::DictationSetup;
use tramontana_core::ports::{DictationRepository, RepositoryError};

use crate::Database;
use crate::repositories::{parse_json, to_json, unavailable};

/// The dictation setup, in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteDictationRepository {
    database: Database,
}

impl SqliteDictationRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl DictationRepository for SqliteDictationRepository {
    async fn load(&self) -> Result<Option<DictationSetup>, RepositoryError> {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT setup FROM dictation_setup WHERE setup_id = 1")
                .fetch_optional(self.database.pool())
                .await
                .map_err(unavailable)?;
        stored
            .map(|setup| parse_json(&setup, "dictation setup"))
            .transpose()
    }

    async fn save(&self, setup: &DictationSetup) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO dictation_setup (setup_id, setup) VALUES (1, ?)
             ON CONFLICT(setup_id) DO UPDATE SET setup = excluded.setup",
        )
        .bind(to_json(setup, "dictation setup")?)
        .execute(self.database.pool())
        .await
        .map_err(unavailable)
        .map(|_| ())
    }

    async fn clear(&self) -> Result<(), RepositoryError> {
        sqlx::query("DELETE FROM dictation_setup")
            .execute(self.database.pool())
            .await
            .map_err(unavailable)
            .map(|_| ())
    }
}
