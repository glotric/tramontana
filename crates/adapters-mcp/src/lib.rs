//! MCP Apps over stdio.
//!
//! An App is an MCP server Tramontana starts on the person's configuration.
//! This crate owns that process and speaks its protocol through rmcp
//! (ADR 0034), and presents what it offers as ordinary tools behind the
//! core's `ToolProvider` port. It decides nothing: whether a tool may run is
//! the permission manager's question, and the registry is the only place a
//! tool can be found.
//!
//! Everything an App sends is untrusted (`SECURITY_AND_PERMISSIONS.md` §9).
//! Its tool names become `mcp.<app>.<tool>` or are refused; its descriptions
//! and titles are redacted and cut to length; its schemas pass the core's
//! restricted schema compiler or the tool is refused; each line it writes is
//! bounded before rmcp reads it; and its results are cut to a limit.
//!
//! Only stdio is supported. Streamable HTTP and OAuth are deferred by a
//! recorded exception (ADR 0034), which is a limit of this adapter, not of
//! the port.

mod app;
mod bounded;
mod descriptor;

pub use app::{AppHealth, McpApp, McpLimits, McpServerSpec, StartError};
pub use descriptor::Refusal;
