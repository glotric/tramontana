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

mod database;
mod event_store;
mod repositories;

pub use database::{Database, DatabaseError};
pub use event_store::SqliteEventStore;
pub use repositories::{
    SqliteConfigurationRepository, SqliteConversationRepository, SqliteProjectRepository,
    SqliteTaskRepository,
};
