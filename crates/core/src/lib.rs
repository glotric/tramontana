//! Provider-neutral domain types, ports, and application services for Tramontana.
//!
//! This crate is the inward-most layer described in `docs/ARCHITECTURE.md` §2.
//! Adapters depend on contracts defined here; core never imports a concrete
//! model, worker, MCP, database, platform, or UI implementation.
//!
//! Milestone M0 establishes only the contract module and the schema-generation
//! path. The task state machine, event store, ports, and services arrive in M1
//! (`docs/BUILD_ORDER.md` §8).

pub mod contracts;

pub use contracts::SCHEMA_VERSION;
