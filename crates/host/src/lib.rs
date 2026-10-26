//! A host: the application service, served to paired desktops.
//!
//! `SHARED_PROJECTS_PLAN.md` SP1. A host runs the same service a desktop does
//! (`tramontana_composition::open_service_plane`) and lets a desktop elsewhere
//! drive it:
//!
//! - over one WebSocket per desktop, on TLS whose certificate the desktop
//!   pins (ADR 0074, `tls.rs`);
//! - after the desktop proves which paired device it is (ADR 0076,
//!   `devices.rs`);
//! - with the same command names and payloads as the desktop's own Tauri
//!   commands (`dispatch.rs`), and the same task updates (`server.rs`).
//!
//! No policy lives here. A rule a host needs that the desktop does not have
//! belongs in `crates/core`.

mod approvals;
mod client;
mod devices;
mod dispatch;
mod protocol;
mod server;
mod tls;

pub use approvals::RemoteApprover;
pub use client::{ClientError, HostClient, HostEvent, PairLink, pair};
pub use devices::{Devices, PairError, Token};
pub use protocol::{ClientFrame, ServerFrame};
pub use server::Host;
pub use tls::{ConnectError, acceptor, connect, fingerprint};
