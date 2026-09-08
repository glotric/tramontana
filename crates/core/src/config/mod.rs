//! Loading the project-authored Markdown plane.
//!
//! `CONFIGURATION.md` is normative. This module is pure: it takes file paths
//! and their bytes and produces an [`EffectiveConfiguration`]. Discovery — the
//! actual filesystem walk — belongs to an adapter, which keeps precedence and
//! validation testable without a temporary directory.
//!
//! Two rules dominate everything here:
//!
//! - **Loading Markdown grants nothing.** A file cannot expand a root, select a
//!   secret, force a cloud destination, or authorize a tool. It contributes
//!   text and metadata, and nothing else (`CONFIGURATION.md` §4).
//! - **A broken file disables its item, never startup.** Malformed front
//!   matter, a duplicate identifier, or an oversized document produces a
//!   diagnostic and an inert source (§5).

mod front_matter;
mod loader;

pub use front_matter::{FrontMatter, FrontMatterError, split_front_matter};
pub use loader::{RawSource, resolve};
