//! Dictation: speech to text on this device (ADR 0065).
//!
//! The rules are here; the transcriber is an adapter. Audio is checked,
//! handed to the transcriber once, and never kept or recorded.

use super::task_service::OrEnvelope as _;
use super::task_service::TaskService;
use crate::contracts::{DictationSetup, DictationStatus, ErrorCode, ErrorEnvelope};

/// The most audio one dictation may be: five minutes of 16 kHz mono 16-bit
/// PCM is 9.6 MB.
pub const DICTATION_AUDIO_BYTES: usize = 10 * 1024 * 1024;

/// Whether `bytes` start as a RIFF/WAVE file does.
#[must_use]
pub fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() > 44 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Whether `code` is a language whisper can be told: two or three lowercase
/// letters, or `auto`.
#[must_use]
pub fn is_language_code(code: &str) -> bool {
    code == "auto"
        || ((2..=3).contains(&code.len()) && code.chars().all(|c| c.is_ascii_lowercase()))
}

impl TaskService {
    fn dictation_ports(&self) -> Result<&super::task_service::DictationPorts, ErrorEnvelope> {
        self.dictation.as_ref().ok_or_else(|| {
            self.error(
                ErrorCode::CapabilityUnsupported,
                "This application has no dictation",
            )
        })
    }

    /// Whether dictation is set up, and with what.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::CapabilityUnsupported`] when the service has no
    /// dictation, or a persistence error.
    pub async fn dictation_status(&self) -> Result<DictationStatus, ErrorEnvelope> {
        let (store, _) = self.dictation_ports()?;
        let setup = store.load().await.or_envelope(self)?;
        Ok(DictationStatus {
            configured: setup.is_some(),
            binary_path: setup.as_ref().map(|setup| setup.binary_path.clone()),
            model_path: setup.as_ref().map(|setup| setup.model_path.clone()),
            language: setup.and_then(|setup| setup.language),
        })
    }

    /// Choose the `whisper-cli` and model to transcribe with, fingerprinting
    /// both as they are now (ADR 0043).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for a language that is not a code, or
    /// whatever reading the files reports.
    pub async fn set_dictation(
        &self,
        binary_path: &str,
        model_path: &str,
        language: Option<&str>,
    ) -> Result<DictationStatus, ErrorEnvelope> {
        let (store, transcriber) = self.dictation_ports()?;
        let language = language
            .map(str::trim)
            .filter(|code| !code.is_empty() && *code != "auto");
        if language.is_some_and(|code| !is_language_code(code)) {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                "A language is a two- or three-letter code, such as en or sl",
            ));
        }
        let provenance = transcriber.inspect(binary_path, model_path, self.clock.now())?;
        store
            .save(&DictationSetup {
                binary_path: binary_path.to_owned(),
                model_path: model_path.to_owned(),
                language: language.map(str::to_owned),
                provenance,
            })
            .await
            .or_envelope(self)?;
        self.dictation_status().await
    }

    /// Forget the dictation setup. The files themselves are not touched.
    ///
    /// # Errors
    ///
    /// As [`Self::dictation_status`].
    pub async fn clear_dictation(&self) -> Result<DictationStatus, ErrorEnvelope> {
        let (store, _) = self.dictation_ports()?;
        store.clear().await.or_envelope(self)?;
        self.dictation_status().await
    }

    /// The text spoken in `wav`. Nothing is kept, recorded or sent.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::CapabilityUnsupported`] when dictation is not set up,
    /// [`ErrorCode::InvalidRequest`] for audio that is not a WAV or is too
    /// long, or whatever the transcriber reports.
    pub async fn transcribe(&self, wav: Vec<u8>) -> Result<String, ErrorEnvelope> {
        let (store, transcriber) = self.dictation_ports()?;
        if wav.len() > DICTATION_AUDIO_BYTES {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                "A dictation can be at most five minutes long",
            ));
        }
        if !is_wav(&wav) {
            return Err(self.error(ErrorCode::InvalidRequest, "Dictation needs WAV audio"));
        }
        let setup = store.load().await.or_envelope(self)?.ok_or_else(|| {
            self.error(
                ErrorCode::CapabilityUnsupported,
                "Set up dictation in Settings first: choose a whisper.cpp program and a model",
            )
        })?;
        let text = transcriber.transcribe(&setup, wav).await?;
        Ok(text.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_wav_and_a_language_code_are_accepted() {
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.resize(64, 0);
        assert!(is_wav(&wav));
        assert!(!is_wav(
            b"OggS not a wav at all, but long enough to be checked...."
        ));
        assert!(is_language_code("en"));
        assert!(is_language_code("auto"));
        assert!(!is_language_code("EN"));
        assert!(!is_language_code("english"));
        assert!(!is_language_code("e"));
    }
}
