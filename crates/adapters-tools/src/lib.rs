//! Deterministic, project-scoped tools.
//!
//! Every tool here was read-only until M7, which adds the first writes:
//! `builtin.fs.create`, `builtin.fs.patch` and `builtin.fs.move`. They share
//! four properties that matter more than what any one of them does:
//!
//! - **The granted scope is the authority.** Each tool re-checks containment
//!   against [`AuthorizedToolRequest::granted_scope`] rather than trusting the
//!   caller. A mistake upstream then fails closed instead of reading a file
//!   nobody approved (`SECURITY_AND_PERMISSIONS.md` §5).
//! - **Output is bounded before it is produced**, not truncated afterwards, so
//!   a large file cannot decide how much memory the process allocates.
//! - **Results carry a safe summary.** The structured content is for the model;
//!   the summary is what a person reads in the activity stream.
//! - **A write checks before it acts.** It never overwrites what it did not
//!   expect, and it reports an effect it finds already made as applied, so
//!   recovery can re-invoke an interrupted write without applying it twice
//!   (ADR 0019). A patch goes further and proves a replay is its own edit, by
//!   reversing it and comparing hashes (ADR 0020). A move links the file under
//!   its new name before unlinking the old, so it never replaces a file and
//!   never leaves one without a name (ADR 0022).

mod filesystem;
mod git;
mod identity;
mod limits;
mod scope;

pub use filesystem::FilesystemTools;
pub use git::GitTools;
pub use limits::ReadLimits;
