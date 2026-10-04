//! SQLite implementation of the local operational plane.
//!
//! `PROJECT_FORMAT.md` §2 splits the world in two: the *project-authored*
//! plane is Markdown in the user's repository, and the *local operational*
//! plane is this database. Nothing here is committed to a user's project, and
//! nothing here holds a secret value — only opaque references.
//!
//! Core defines the repository ports; this crate implements them. SQLx and
//! SQLite types never appear in a core signature (`DEVELOPMENT.md` §7), which
//! is what lets the same contract suites run against the in-memory fakes and
//! against this.

mod apps;
mod database;
mod dictation;
mod erasure;
mod event_store;
mod personal;
mod repositories;
mod schedules;

pub use apps::SqliteAppSettingsRepository;

pub use database::{Database, DatabaseError};
pub use dictation::SqliteDictationRepository;
pub use erasure::SqliteProfileEraser;
pub use event_store::SqliteEventStore;
pub use personal::SqlitePersonalRepository;
pub use repositories::{
    SqliteCheckpointRepository, SqliteConfigurationRepository, SqliteConversationRepository,
    SqliteProjectRepository, SqliteProviderSettingsRepository, SqliteTaskRepository,
};
pub use schedules::SqliteScheduleRepository;
