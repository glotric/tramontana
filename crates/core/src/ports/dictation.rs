//! Transcribing speech on this device (ADR 0065).

use async_trait::async_trait;

use crate::contracts::{DictationSetup, ErrorEnvelope, RuntimeProvenance, Timestamp};

use super::RepositoryError;

/// Something that turns recorded speech into text, on this device.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Fingerprint the chosen executable and model, as they are now.
    ///
    /// # Errors
    ///
    /// When either cannot be read.
    fn inspect(
        &self,
        binary_path: &str,
        model_path: &str,
        recorded_at: Timestamp,
    ) -> Result<RuntimeProvenance, ErrorEnvelope>;

    /// The text spoken in `wav`: 16 kHz mono 16-bit PCM. Checks the
    /// executable against its fingerprint first, and keeps nothing.
    ///
    /// # Errors
    ///
    /// When the executable changed, cannot run, runs over its deadline, or
    /// prints nothing.
    async fn transcribe(
        &self,
        setup: &DictationSetup,
        wav: Vec<u8>,
    ) -> Result<String, ErrorEnvelope>;
}

/// Where the dictation setup is kept.
#[async_trait]
pub trait DictationRepository: Send + Sync {
    /// The setup, if one was chosen.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn load(&self) -> Result<Option<DictationSetup>, RepositoryError>;

    /// Replace the setup.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn save(&self, setup: &DictationSetup) -> Result<(), RepositoryError>;

    /// Forget the setup.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Unavailable`] when storage failed.
    async fn clear(&self) -> Result<(), RepositoryError>;
}
