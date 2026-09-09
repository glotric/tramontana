//! Deterministic, project-scoped tools.
//!
//! Every tool here is read-only at M3. They share three properties that matter
//! more than what any one of them does:
//!
//! - **The granted scope is the authority.** Each tool re-checks containment
//!   against [`AuthorizedToolRequest::granted_scope`] rather than trusting the
//!   caller. A mistake upstream then fails closed instead of reading a file
//!   nobody approved (`SECURITY_AND_PERMISSIONS.md` §5).
//! - **Output is bounded before it is produced**, not truncated afterwards, so
//!   a large file cannot decide how much memory the process allocates.
//! - **Results carry a safe summary.** The structured content is for the model;
//!   the summary is what a person reads in the activity stream.

mod filesystem;
mod git;
mod identity;
mod limits;
mod scope;

pub use filesystem::FilesystemTools;
pub use git::GitTools;
pub use limits::ReadLimits;
