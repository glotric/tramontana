//! Where the person's profile and memory notes are kept (ADR 0061).

use async_trait::async_trait;
use sqlx::Row;
use tramontana_core::contracts::{
    AnswerStyle, MemoryNote, PersonalProfile, SCHEMA_VERSION, Timestamp,
};
use tramontana_core::ports::{PersonalRepository, RepositoryError};

use crate::Database;
use crate::repositories::unavailable;

/// The person's profile, in SQLite.
#[derive(Debug, Clone)]
pub struct SqlitePersonalRepository {
    database: Database,
}

impl SqlitePersonalRepository {
    /// Wrap an open, migrated database.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

fn style_name(style: AnswerStyle) -> &'static str {
    match style {
        AnswerStyle::Quick => "quick",
        AnswerStyle::Balanced => "balanced",
        AnswerStyle::Thorough => "thorough",
    }
}

fn style_from(name: &str) -> AnswerStyle {
    match name {
        "quick" => AnswerStyle::Quick,
        "thorough" => AnswerStyle::Thorough,
        // An unknown style from a newer build reads as the default rather
        // than failing the whole profile.
        _ => AnswerStyle::Balanced,
    }
}

#[async_trait]
impl PersonalRepository for SqlitePersonalRepository {
    async fn load(&self) -> Result<PersonalProfile, RepositoryError> {
        let pool = self.database.pool();
        let Some(row) = sqlx::query(
            "SELECT name, about, answer_style, memory_enabled
             FROM personal_profile WHERE profile_id = 1",
        )
        .fetch_optional(pool)
        .await
        .map_err(unavailable)?
        else {
            return Ok(PersonalProfile::default());
        };
        let notes = sqlx::query(
            "SELECT note_id, text, created_at FROM memory_note ORDER BY position, note_id",
        )
        .fetch_all(pool)
        .await
        .map_err(unavailable)?
        .iter()
        .map(|note| {
            Ok(MemoryNote {
                note_id: note.try_get("note_id").map_err(unavailable)?,
                text: note.try_get("text").map_err(unavailable)?,
                created_at: Timestamp::new(
                    note.try_get::<String, _>("created_at")
                        .map_err(unavailable)?,
                ),
            })
        })
        .collect::<Result<Vec<_>, RepositoryError>>()?;
        Ok(PersonalProfile {
            schema_version: SCHEMA_VERSION,
            name: row.try_get("name").map_err(unavailable)?,
            about: row.try_get("about").map_err(unavailable)?,
            answer_style: style_from(
                &row.try_get::<String, _>("answer_style")
                    .map_err(unavailable)?,
            ),
            memory_enabled: row
                .try_get::<bool, _>("memory_enabled")
                .map_err(unavailable)?,
            notes,
        })
    }

    async fn save(&self, profile: &PersonalProfile) -> Result<(), RepositoryError> {
        // One transaction: the notes and the details are one profile, and a
        // reader never sees half of an edit.
        let mut transaction = self.database.pool().begin().await.map_err(unavailable)?;
        sqlx::query(
            "INSERT INTO personal_profile
                 (profile_id, name, about, answer_style, memory_enabled, schema_version)
             VALUES (1, ?, ?, ?, ?, ?)
             ON CONFLICT(profile_id) DO UPDATE SET
                 name = excluded.name,
                 about = excluded.about,
                 answer_style = excluded.answer_style,
                 memory_enabled = excluded.memory_enabled,
                 schema_version = excluded.schema_version",
        )
        .bind(profile.name.as_deref())
        .bind(profile.about.as_deref())
        .bind(style_name(profile.answer_style))
        .bind(profile.memory_enabled)
        .bind(i64::from(profile.schema_version))
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;
        sqlx::query("DELETE FROM memory_note")
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        for (position, note) in profile.notes.iter().enumerate() {
            sqlx::query(
                "INSERT INTO memory_note (note_id, text, created_at, position)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&note.note_id)
            .bind(&note.text)
            .bind(note.created_at.as_str())
            .bind(i64::try_from(position).unwrap_or(i64::MAX))
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        }
        transaction.commit().await.map_err(unavailable)
    }
}
