//! Where a profile keeps its Apps (ADR 0036).
//!
//! One row per App in `mcp_app`, its launch settings as JSON like a model
//! endpoint's. A secret appears only as the reference `AppSecret` holds; the
//! type has no field a value could be put in.

use async_trait::async_trait;
use sqlx::Row;
use tramontana_core::contracts::{AppConfig, SCHEMA_VERSION, Timestamp};
use tramontana_core::ports::{AppSettingsRepository, RepositoryError};

use crate::Database;
use crate::repositories::{parse_json, to_json, unavailable};

/// The profile's configured Apps, in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteAppSettingsRepository {
    database: Database,
}

impl SqliteAppSettingsRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl AppSettingsRepository for SqliteAppSettingsRepository {
    async fn list(&self) -> Result<Vec<AppConfig>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT app_id, display_name, enabled, config, created_at, schema_version
             FROM mcp_app
             ORDER BY created_at, app_id",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;

        let mut apps = Vec::with_capacity(rows.len());
        for row in rows {
            let config: String = row.try_get("config").map_err(|e| unavailable(&e))?;
            let schema_version: i64 = row.try_get("schema_version").map_err(|e| unavailable(&e))?;
            apps.push(AppConfig {
                schema_version: u32::try_from(schema_version).unwrap_or(SCHEMA_VERSION),
                app_id: row.try_get("app_id").map_err(|e| unavailable(&e))?,
                display_name: row.try_get("display_name").map_err(|e| unavailable(&e))?,
                enabled: row
                    .try_get::<bool, _>("enabled")
                    .map_err(|e| unavailable(&e))?,
                launch: parse_json(&config, "App configuration")?,
                created_at: Timestamp::new(
                    row.try_get::<String, _>("created_at")
                        .map_err(|e| unavailable(&e))?,
                ),
            });
        }
        Ok(apps)
    }

    async fn save(&self, app: &AppConfig) -> Result<(), RepositoryError> {
        // Replace rather than insert-or-fail, as for a model endpoint:
        // turning an App on or off is an edit.
        sqlx::query(
            "INSERT INTO mcp_app (
                 app_id, display_name, enabled, config, created_at, schema_version
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(app_id) DO UPDATE SET
                 display_name = excluded.display_name,
                 enabled = excluded.enabled,
                 config = excluded.config",
        )
        .bind(&app.app_id)
        .bind(&app.display_name)
        .bind(app.enabled)
        .bind(to_json(&app.launch, "App configuration")?)
        .bind(app.created_at.as_str())
        .bind(i64::from(app.schema_version))
        .execute(self.database.pool())
        .await
        .map_err(|error| unavailable(&error))?;
        Ok(())
    }

    async fn delete(&self, app_id: &str) -> Result<bool, RepositoryError> {
        let result = sqlx::query("DELETE FROM mcp_app WHERE app_id = ?")
            .bind(app_id)
            .execute(self.database.pool())
            .await
            .map_err(|error| unavailable(&error))?;
        Ok(result.rows_affected() > 0)
    }
}
