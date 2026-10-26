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
        .map_err(unavailable)?;

        let mut apps = Vec::with_capacity(rows.len());
        for row in rows {
            let config: String = row.try_get("config").map_err(unavailable)?;
            let schema_version: i64 = row.try_get("schema_version").map_err(unavailable)?;
            // A remote App's column holds `{"remote": ...}` (ADR 0050); any
            // other holds the launch settings, as every row did before.
            let stored: Stored = parse_json(&config, "App configuration")?;
            apps.push(AppConfig {
                schema_version: u32::try_from(schema_version).unwrap_or(SCHEMA_VERSION),
                app_id: row.try_get("app_id").map_err(unavailable)?,
                display_name: row.try_get("display_name").map_err(unavailable)?,
                enabled: row.try_get::<bool, _>("enabled").map_err(unavailable)?,
                launch: stored.launch,
                remote: stored.remote,
                created_at: Timestamp::new(
                    row.try_get::<String, _>("created_at")
                        .map_err(unavailable)?,
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
        .bind(match &app.remote {
            Some(remote) => to_json(
                &serde_json::json!({ "remote": remote }),
                "App configuration",
            )?,
            None => to_json(&app.launch, "App configuration")?,
        })
        .bind(app.created_at.as_str())
        .bind(i64::from(app.schema_version))
        .execute(self.database.pool())
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    async fn delete(&self, app_id: &str) -> Result<bool, RepositoryError> {
        let result = sqlx::query("DELETE FROM mcp_app WHERE app_id = ?")
            .bind(app_id)
            .execute(self.database.pool())
            .await
            .map_err(unavailable)?;
        Ok(result.rows_affected() > 0)
    }
}

/// What the `config` column holds: launch settings, or a remote address.
struct Stored {
    launch: tramontana_core::contracts::AppLaunch,
    remote: Option<tramontana_core::contracts::AppRemote>,
}

impl<'de> serde::Deserialize<'de> for Stored {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value.get("remote") {
            Some(remote) if value.get("program").is_none() => Ok(Self {
                launch: tramontana_core::contracts::AppLaunch {
                    program: String::new(),
                    arguments: Vec::new(),
                    working_directory: None,
                    environment: Vec::new(),
                    secrets: Vec::new(),
                },
                remote: Some(
                    serde_json::from_value(remote.clone()).map_err(serde::de::Error::custom)?,
                ),
            }),
            _ => Ok(Self {
                launch: serde_json::from_value(value).map_err(serde::de::Error::custom)?,
                remote: None,
            }),
        }
    }
}
