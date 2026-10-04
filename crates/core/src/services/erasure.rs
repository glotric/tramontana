//! Deleting everything a profile holds, at the person's request (ADR 0068).

use super::task_service::OrEnvelope as _;
use super::task_service::TaskService;
use crate::contracts::{ErrorEnvelope, SecretRef};
use crate::ports::{AppSettingsRepository, ProfileEraser, ProviderSettingsRepository, SecretVault};

/// What erasing a profile needs beyond the service's own stores.
#[allow(
    missing_debug_implementations,
    reason = "every field is a port behind `dyn`, which has nothing to print"
)]
pub struct ProfileErasure<'a> {
    /// Removes every record in one commit.
    pub records: &'a dyn ProfileEraser,
    /// The model endpoints, read for the credentials they name.
    pub endpoints: &'a dyn ProviderSettingsRepository,
    /// The Apps, read for the secrets and tokens they name.
    pub apps: &'a dyn AppSettingsRepository,
    /// Where those credentials are kept.
    pub vault: &'a dyn SecretVault,
}

/// What an erasure did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErasureReport {
    /// Credentials the profile named and the store held, now removed.
    pub credentials_removed: usize,
    /// Credentials the profile named that the store no longer held.
    pub credentials_absent: usize,
}

impl TaskService {
    /// Delete everything this profile holds: every record, and every
    /// credential its endpoints and Apps name in the credential store
    /// (ADR 0068). Project folders and their files are never touched.
    ///
    /// Refused while any task is under way, as removing a project is.
    /// Credentials go first: if a later step fails the records still name
    /// them, so trying again finds and finishes the rest, where the other
    /// order would leave keys in the store that nothing names any more.
    ///
    /// A credential named by nothing — one whose endpoint was removed
    /// earlier, which leaves its key in place (ADR 0008) — cannot be found
    /// this way: the credential store cannot be listed.
    ///
    /// # Errors
    ///
    /// [`crate::contracts::ErrorCode::Conflict`] while a task runs, or a
    /// storage or credential-store error; nothing is removed from the
    /// records then.
    pub async fn erase_everything(
        &self,
        erasure: &ProfileErasure<'_>,
    ) -> Result<ErasureReport, ErrorEnvelope> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_execution()?;
        let tasks = self.list_tasks(None).await?;
        self.none_under_way(&tasks, "Stop the running tasks before deleting all data")
            .await?;

        let mut credentials: Vec<SecretRef> = Vec::new();
        for endpoint in erasure.endpoints.list().await.or_envelope(self)? {
            credentials.extend(endpoint.config.credential().cloned());
        }
        for app in erasure.apps.list().await.or_envelope(self)? {
            credentials.extend(
                app.launch
                    .secrets
                    .into_iter()
                    .map(|secret| secret.reference),
            );
            if let Some(remote) = app.remote {
                credentials.extend(remote.token);
                credentials.extend(remote.refresh);
            }
        }
        credentials.sort_by(|a, b| a.key.cmp(&b.key));
        credentials.dedup();

        let mut report = ErasureReport::default();
        for credential in &credentials {
            if erasure.vault.delete(credential).await? {
                report.credentials_removed += 1;
            } else {
                report.credentials_absent += 1;
            }
        }

        erasure.records.erase_everything().await.or_envelope(self)?;
        Ok(report)
    }
}
