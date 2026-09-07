//! Canonical serializable contract types.
//!
//! `docs/CORE_CONTRACTS.md` is normative for the meaning of everything in this
//! module; these types are the executable source of truth for its shape. The
//! two are changed together.
//!
//! Serialization rules that apply to every type here (`CORE_CONTRACTS.md` §2):
//!
//! - JSON field names and enum values are `camelCase`;
//! - every top-level command, response, event, checkpoint, and export object
//!   carries a `schemaVersion`;
//! - secrets are never serialized — only an opaque [`SecretRef`].

mod primitives;

pub use primitives::{
    DataClass, ExecutionLocation, RiskClass, RoutingMode, SecretRef, SecretStore, SideEffect,
    Visibility,
};

/// Version of the schemas exchanged across the IPC and export boundaries.
///
/// Initial executable schemas use version `1`. Bumping this is a breaking
/// change: it requires a migration, regenerated artifacts, and a decision
/// record (`docs/CORE_CONTRACTS.md` §15).
pub const SCHEMA_VERSION: u32 = 1;
