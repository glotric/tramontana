//! The frames on a host's WebSocket (ADR 0074 §3).
//!
//! Every frame is one JSON object with one key naming its kind, so a reader
//! can route on the key without understanding the body.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tramontana_core::contracts::{ApprovalId, ApprovalRequest, ErrorEnvelope, TaskUpdate};

use crate::devices::Token;

/// From a desktop to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientFrame {
    /// The first frame of a paired device: who it is.
    Hello {
        /// The contract version the desktop speaks.
        schema_version: u32,
        /// The device token from pairing. Never in a URL (ADR 0074).
        token: Token,
    },
    /// The first frame of a device being paired: an invite code, used once.
    Pair {
        /// The contract version the desktop speaks.
        schema_version: u32,
        /// The code from the invite link.
        code: Token,
        /// What the owner will see in the device list.
        device_name: String,
    },
    /// One command, named as the desktop's Tauri command is.
    Command {
        /// Echoed in the reply, so replies can arrive in any order.
        id: u64,
        /// The command's name, such as `list_projects`.
        name: String,
        /// Its arguments, as the Tauri command takes them.
        #[serde(default)]
        args: Value,
    },
}

/// From the host to a desktop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerFrame {
    /// The device is known; commands may follow.
    Welcome {
        /// The contract version the host speaks.
        schema_version: u32,
        /// Who the device belongs to.
        principal: String,
    },
    /// Pairing succeeded. The token is shown once; the desktop keeps it in
    /// its credential store.
    Paired {
        /// The contract version the host speaks.
        schema_version: u32,
        /// Who the device now belongs to.
        principal: String,
        /// The device's token for every later connection.
        token: Token,
    },
    /// The answer to one command: a result or an error, never both.
    Reply {
        /// The command's `id`.
        id: u64,
        /// The result, when it succeeded.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        result: Option<Value>,
        /// The error, when it failed.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        error: Option<ErrorEnvelope>,
    },
    /// One update of a task this connection watches.
    Update {
        /// The watch it belongs to, from `watch_task`'s reply.
        watch: u64,
        /// The update, exactly as the desktop's own channel carries it.
        update: Box<TaskUpdate>,
    },
    /// A question a task is waiting on, as the desktop's own card shows it.
    /// Answered with the `resolve_approval` command.
    ApprovalRequested {
        /// The question, with the answers that may be given.
        request: Box<ApprovalRequest>,
    },
    /// A question is no longer waiting — answered, here or on another
    /// desktop, or expired — so its card goes away.
    ApprovalSettled {
        /// Which question.
        approval_id: ApprovalId,
    },
    /// The connection is refused, and closes after this frame.
    Refused {
        /// Why, without detail an attacker could use.
        error: ErrorEnvelope,
    },
}
