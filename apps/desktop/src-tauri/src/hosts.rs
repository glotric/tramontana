//! Hosts this desktop is paired with (SP1 step 4).
//!
//! The desktop's Rust side pairs with a host from an invite link, keeps the
//! device token in the OS credential store, and holds one connection per
//! host. The webview names a host by id and never sees a token or a socket
//! (ADR 0074 §1). Commands and task watches are forwarded as they are; the
//! host refuses any it does not serve.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};
use tramontana_core::contracts::{
    ErrorCode, ErrorEnvelope, SecretRef, SecretStore, TaskId, TaskUpdate,
};
use tramontana_core::ports::SecretVault;
use tramontana_host::{HostClient, HostEvent, PairLink, Token};

use crate::ipc_error;
use crate::state::AppState;

/// Events the webview listens for, besides a watch's own channel.
const HOST_APPROVAL_REQUESTED: &str = "tramontana://host-approval-requested";
const HOST_APPROVAL_SETTLED: &str = "tramontana://host-approval-settled";
const HOST_DISCONNECTED: &str = "tramontana://host-disconnected";

/// A paired host, as the desktop remembers it. No secret: the token is in
/// the OS credential store under [`token_ref`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostRecord {
    /// Stable id for the webview: the start of the certificate fingerprint.
    pub host_id: String,
    /// Where it listens.
    pub address: String,
    /// The pinned certificate fingerprint.
    pub fingerprint: String,
    /// Who this device is on that host.
    pub principal: String,
}

/// What a host announces, for one connection's event pump.
type Announce = Arc<dyn Fn(&str, HostEvent) + Send + Sync>;

/// The paired hosts and their live connections.
pub(crate) struct Hosts {
    // shortcut: a JSON file beside the profile, enough for SP1's one person.
    // Moves into the profile database when SP4 adds people and teams.
    file: PathBuf,
    vault: Arc<dyn SecretVault>,
    connections: Arc<tokio::sync::Mutex<HashMap<String, Arc<HostClient>>>>,
}

impl std::fmt::Debug for Hosts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hosts")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

fn token_ref(host_id: &str) -> SecretRef {
    SecretRef {
        key: format!("host.{host_id}"),
        store: SecretStore::Platform,
    }
}

fn unknown_host() -> ErrorEnvelope {
    ipc_error(
        ErrorCode::NotFound,
        "this desktop is not paired with that host",
    )
}

impl Hosts {
    /// The hosts remembered in `file`, with tokens in `vault`.
    pub(crate) fn new(file: PathBuf, vault: Arc<dyn SecretVault>) -> Self {
        Self {
            file,
            vault,
            connections: Arc::default(),
        }
    }

    /// Every paired host. No file yet is none.
    pub(crate) fn records(&self) -> Result<Vec<HostRecord>, ErrorEnvelope> {
        match std::fs::read(&self.file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| ipc_error(ErrorCode::PersistenceError, "the host list is damaged")),
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(ipc_error(
                ErrorCode::PersistenceError,
                "the host list could not be read",
            )),
        }
    }

    fn save(&self, records: &[HostRecord]) -> Result<(), ErrorEnvelope> {
        let unsaved = |_| {
            ipc_error(
                ErrorCode::PersistenceError,
                "the host list could not be saved",
            )
        };
        let bytes = serde_json::to_vec_pretty(records).map_err(|_| {
            ipc_error(
                ErrorCode::PersistenceError,
                "the host list could not be saved",
            )
        })?;
        let staging = self.file.with_extension("staging");
        std::fs::write(&staging, bytes).map_err(unsaved)?;
        std::fs::rename(&staging, &self.file).map_err(unsaved)
    }

    /// Pair with the host an invite link names, and remember it.
    pub(crate) async fn pair(
        &self,
        link: &str,
        device_name: &str,
    ) -> Result<HostRecord, ErrorEnvelope> {
        let link = PairLink::parse(link).ok_or_else(|| {
            ipc_error(
                ErrorCode::InvalidRequest,
                "that is not a Tramontana invite link",
            )
        })?;
        let (principal, token) = tramontana_host::pair(&link, device_name)
            .await
            .map_err(|error| ipc_error(ErrorCode::ProviderUnavailable, error.to_string()))?;
        let record = HostRecord {
            host_id: link.fingerprint[..16].to_owned(),
            address: link.address,
            fingerprint: link.fingerprint,
            principal,
        };
        self.vault
            .store(&token_ref(&record.host_id), token.expose())
            .await?;
        // A connection made with the token this one replaces is closed.
        self.connections.lock().await.remove(&record.host_id);
        let mut records = self.records()?;
        records.retain(|known| known.host_id != record.host_id);
        records.push(record.clone());
        self.save(&records)?;
        Ok(record)
    }

    /// Forget a host: its record, its token, and any open connection.
    pub(crate) async fn forget(&self, host_id: &str) -> Result<bool, ErrorEnvelope> {
        self.connections.lock().await.remove(host_id);
        let mut records = self.records()?;
        let before = records.len();
        records.retain(|known| known.host_id != host_id);
        if records.len() == before {
            return Ok(false);
        }
        self.save(&records)?;
        self.vault.delete(&token_ref(host_id)).await?;
        Ok(true)
    }

    /// The live connection to a host, opened if there is none. Its events
    /// go to `announce` until it ends.
    pub(crate) async fn connected(
        &self,
        host_id: &str,
        announce: Announce,
    ) -> Result<Arc<HostClient>, ErrorEnvelope> {
        let mut connections = self.connections.lock().await;
        if let Some(client) = connections.get(host_id) {
            return Ok(Arc::clone(client));
        }
        let record = self
            .records()?
            .into_iter()
            .find(|known| known.host_id == host_id)
            .ok_or_else(unknown_host)?;
        let token = Token::new(self.vault.resolve(&token_ref(host_id)).await?);
        let (client, mut events) =
            HostClient::connect(&record.address, &record.fingerprint, &token)
                .await
                .map_err(|error| ipc_error(ErrorCode::ProviderUnavailable, error.to_string()))?;
        let client = Arc::new(client);
        connections.insert(host_id.to_owned(), Arc::clone(&client));

        let host_id = host_id.to_owned();
        let pool = Arc::clone(&self.connections);
        let mine = Arc::as_ptr(&client).addr();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let gone = event == HostEvent::Disconnected;
                announce(&host_id, event);
                if gone {
                    // The next command reconnects. Only this connection is
                    // dropped: the host may have been forgotten and paired
                    // again since, with a newer one in its place.
                    let mut pool = pool.lock().await;
                    if pool
                        .get(&host_id)
                        .is_some_and(|current| Arc::as_ptr(current).addr() == mine)
                    {
                        pool.remove(&host_id);
                    }
                    break;
                }
            }
        });
        Ok(client)
    }
}

/// What a host's events become in the window.
fn announce_to(app: AppHandle) -> Announce {
    Arc::new(move |host_id: &str, event: HostEvent| {
        let _ = match event {
            HostEvent::ApprovalRequested(request) => app.emit(
                HOST_APPROVAL_REQUESTED,
                serde_json::json!({ "hostId": host_id, "request": request }),
            ),
            HostEvent::ApprovalSettled(approval_id) => app.emit(
                HOST_APPROVAL_SETTLED,
                serde_json::json!({ "hostId": host_id, "approvalId": approval_id }),
            ),
            HostEvent::Disconnected => {
                app.emit(HOST_DISCONNECTED, serde_json::json!({ "hostId": host_id }))
            }
        };
    })
}

/// Pair with a host from an invite link.
#[tauri::command]
pub(crate) async fn pair_host(
    link: String,
    device_name: String,
    state: State<'_, AppState>,
) -> Result<HostRecord, ErrorEnvelope> {
    state.hosts.pair(&link, device_name.trim()).await
}

/// The hosts this desktop is paired with.
#[tauri::command]
pub(crate) fn list_hosts(state: State<'_, AppState>) -> Result<Vec<HostRecord>, ErrorEnvelope> {
    state.hosts.records()
}

/// Forget a paired host. Returns whether it was known.
#[tauri::command]
pub(crate) async fn forget_host(
    host_id: String,
    state: State<'_, AppState>,
) -> Result<bool, ErrorEnvelope> {
    state.hosts.forget(&host_id).await
}

/// Run one of the desktop's commands on a host instead of locally.
#[tauri::command]
pub(crate) async fn host_call(
    host_id: String,
    name: String,
    args: Option<Value>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, ErrorEnvelope> {
    let client = state.hosts.connected(&host_id, announce_to(app)).await?;
    client.call(&name, args.unwrap_or(Value::Null)).await
}

/// Follow a task on a host, as `watch_task` follows a local one.
#[tauri::command]
pub(crate) async fn watch_host_task(
    host_id: String,
    task_id: TaskId,
    after_sequence: u64,
    channel: Channel<TaskUpdate>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<u64, ErrorEnvelope> {
    let client = state.hosts.connected(&host_id, announce_to(app)).await?;
    client
        .watch(&task_id, after_sequence, move |update| {
            channel.send(update).is_ok()
        })
        .await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
)]
mod tests {
    use tramontana_core::testing::InMemorySecretVault;

    use super::*;

    #[tokio::test]
    async fn a_host_list_starts_empty_and_an_unknown_host_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = Hosts::new(
            dir.path().join("hosts.json"),
            Arc::new(InMemorySecretVault::new()),
        );
        assert_eq!(hosts.records().unwrap(), Vec::new());
        assert!(!hosts.forget("nope").await.unwrap());
        let refused = hosts
            .connected("nope", Arc::new(|_: &str, _: HostEvent| {}))
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn a_link_that_is_not_an_invite_is_refused_before_any_connection() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = Hosts::new(
            dir.path().join("hosts.json"),
            Arc::new(InMemorySecretVault::new()),
        );
        let refused = hosts
            .pair("https://example.com/pair?code=1", "laptop")
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidRequest);
        assert!(!dir.path().join("hosts.json").exists());
    }
}
