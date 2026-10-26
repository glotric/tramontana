//! Provider-neutral domain types, ports, and application services for Tramontana.
//!
//! This crate is the inward-most layer described in `docs/ARCHITECTURE.md` §2.
//! Adapters depend on contracts defined here; core never imports a concrete
//! model, worker, MCP, database, platform, or UI implementation.
//!
//! M1 adds the contract types, the stable ports, in-memory fakes for every
//! port, and the application services that drive one task from creation to a
//! single terminal state (`docs/BUILD_ORDER.md` §8).

#[macro_use]
mod macros;

pub mod config;
pub mod contracts;
pub mod ports;
pub mod services;
pub mod testing;

pub use contracts::SCHEMA_VERSION;
