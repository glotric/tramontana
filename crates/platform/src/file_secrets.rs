//! Credentials in an encrypted file, for a host with no credential store.
//!
//! SP0-B spike (`docs/SHARED_PROJECTS_PLAN.md`). A Docker container on a NAS
//! has no Keychain, Credential Manager or Secret Service, so the ports in
//! `secrets.rs` have nothing to talk to. This keeps every credential in one
//! file sealed with XChaCha20-Poly1305 under a 32-byte key that lives in a
//! separate file — on a host, a Docker secret mounted at `/run/secrets/…`.
//!
//! What it protects: the vault file copied off the host on its own — a
//! backup, a snapshot, a synced data folder — is unreadable and tamper-evident.
//! What it does not: anyone who can read both files, which includes whoever
//! administers the host. That is the honest limit `SHARED_PROJECTS_PLAN.md`
//! asks the UI to state.
//!
//! Same rules as `secrets.rs`: a value is a local or a return value, never a
//! field, never in `Debug`, an error or a log. The key is read per operation
//! for the same reason, so this type holds no key material either.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use async_trait::async_trait;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use tokio::sync::Mutex;
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, SecretRef};
use tramontana_core::ports::{SecretResolver, SecretVault};

/// Bound into every seal as associated data, so a file of another format or
/// version is refused rather than misread.
const FORMAT: &[u8] = b"tramontana-file-vault-v1";

/// XChaCha20's nonce, stored in front of the ciphertext. 192 random bits, so
/// a fresh nonce per write never needs a counter.
const NONCE_BYTES: usize = 24;

/// Credentials sealed in one file.
pub struct FileSecretVault {
    file: PathBuf,
    key_file: PathBuf,
    /// Every operation is read-modify-write of the whole file; serializing
    /// them stops two stores from losing one of the writes.
    /// shortcut: one process only; a second process on the same file can
    /// still race. Upgrade to a file lock if a host ever runs two.
    lock: Mutex<()>,
}

impl std::fmt::Debug for FileSecretVault {
    /// The two paths and nothing else. Not derived, as in `secrets.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSecretVault")
            .field("file", &self.file)
            .field("key_file", &self.key_file)
            .finish()
    }
}

impl FileSecretVault {
    /// A vault at `file`, sealed with the hex key in `key_file`.
    ///
    /// Neither is touched here: a missing key is reported by the first
    /// operation as an unavailable store, never as a missing credential.
    #[must_use]
    pub fn new(file: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            key_file: key_file.into(),
            lock: Mutex::new(()),
        }
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, ErrorEnvelope> {
        let text = std::fs::read_to_string(&self.key_file).map_err(|_| {
            error(
                ErrorCode::ProviderUnavailable,
                "the vault key is not readable",
            )
        })?;
        let key = decode_key(text.trim()).ok_or_else(|| {
            error(
                ErrorCode::ProviderUnavailable,
                "the vault key is not 64 hexadecimal characters",
            )
        })?;
        Ok(XChaCha20Poly1305::new(&key))
    }

    /// Every credential, opened. No file yet is an empty vault.
    // shortcut: the opened map is not zeroized when dropped; values live in
    // freed memory until reused. Add `zeroize` if the host process ever
    // shares memory with less trusted code.
    fn open(&self, cipher: &XChaCha20Poly1305) -> Result<BTreeMap<String, String>, ErrorEnvelope> {
        let sealed = match std::fs::read(&self.file) {
            Ok(sealed) => sealed,
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(_) => {
                return Err(error(
                    ErrorCode::ProviderUnavailable,
                    "the vault file is not readable",
                ));
            }
        };
        // Wrong key and altered file are one error on purpose: telling them
        // apart would tell an attacker which one they got right.
        let refused = || {
            error(
                ErrorCode::PersistenceError,
                "the vault cannot be opened with this key, or it was altered",
            )
        };
        if sealed.len() < NONCE_BYTES {
            return Err(refused());
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_BYTES);
        let opened = cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: FORMAT,
                },
            )
            .map_err(|_| refused())?;
        serde_json::from_slice(&opened).map_err(|_| refused())
    }

    /// Seal and replace the file: written beside it, flushed, then renamed
    /// over it, so a crash leaves the old vault or the new one, never half.
    fn seal(
        &self,
        cipher: &XChaCha20Poly1305,
        credentials: &BTreeMap<String, String>,
    ) -> Result<(), ErrorEnvelope> {
        let unwritable = || {
            error(
                ErrorCode::PersistenceError,
                "the vault file could not be written",
            )
        };
        let opened = serde_json::to_vec(credentials).map_err(|_| unwritable())?;
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &opened,
                    aad: FORMAT,
                },
            )
            .map_err(|_| unwritable())?;

        let staging = self.file.with_extension("staging");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut out = options.open(&staging).map_err(|_| unwritable())?;
        out.write_all(&nonce)
            .and_then(|()| out.write_all(&ciphertext))
            .and_then(|()| out.sync_all())
            .map_err(|_| unwritable())?;
        std::fs::rename(&staging, &self.file).map_err(|_| unwritable())
    }
}

fn decode_key(hex: &str) -> Option<Key> {
    if hex.len() != 64 {
        return None;
    }
    let mut key = [0_u8; 32];
    for (byte, pair) in key.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(Key::from(key))
}

fn error(code: ErrorCode, message: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("secret"))
}

#[async_trait]
impl SecretResolver for FileSecretVault {
    async fn resolve(&self, reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        let _held = self.lock.lock().await;
        let cipher = self.cipher()?;
        self.open(&cipher)?.remove(&reference.key).ok_or_else(|| {
            error(
                ErrorCode::CredentialMissing,
                "no credential is stored under that reference",
            )
        })
    }
}

#[async_trait]
impl SecretVault for FileSecretVault {
    async fn store(&self, reference: &SecretRef, value: &str) -> Result<(), ErrorEnvelope> {
        let _held = self.lock.lock().await;
        let cipher = self.cipher()?;
        let mut credentials = self.open(&cipher)?;
        credentials.insert(reference.key.clone(), value.to_owned());
        self.seal(&cipher, &credentials)
    }

    async fn delete(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope> {
        let _held = self.lock.lock().await;
        let cipher = self.cipher()?;
        let mut credentials = self.open(&cipher)?;
        if credentials.remove(&reference.key).is_none() {
            return Ok(false);
        }
        self.seal(&cipher, &credentials)?;
        Ok(true)
    }

    async fn contains(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope> {
        let _held = self.lock.lock().await;
        let cipher = self.cipher()?;
        Ok(self.open(&cipher)?.contains_key(&reference.key))
    }
}
