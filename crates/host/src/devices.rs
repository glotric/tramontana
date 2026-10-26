//! People's paired devices, and the invites that pair them (ADR 0076).
//!
//! The host keeps only SHA-256 digests of invite codes and device tokens. A
//! plain digest is enough because every secret here is random, not chosen by
//! a person, so there is nothing for a slow password hash to protect against
//! guessing. A copy of this file lets no one sign in.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long an invite stays usable, in seconds (ADR 0076 §2).
const INVITE_LIFETIME: u64 = 15 * 60;

/// A secret on the wire: an invite code or a device token.
///
/// Its `Debug` never prints the value, so a frame can be logged whole.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    /// Wrap a value received from elsewhere, such as an invite link.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that must send or store it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 244 random bits from the operating system's generator. `uuid`'s v4
    /// draws from `getrandom`, and is already a dependency.
    fn random() -> Self {
        Self(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn digest(&self) -> String {
        Sha256::digest(self.0.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(…)")
    }
}

/// Why pairing failed.
#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    /// Unknown, used or expired. One answer, so a guesser learns nothing.
    InvalidInvite,
    /// The device list could not be saved; nothing was paired.
    Unsaved,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    invites: Vec<StoredInvite>,
    devices: Vec<StoredDevice>,
    next_device: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredInvite {
    code_digest: String,
    principal: String,
    expires_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredDevice {
    id: u64,
    token_digest: String,
    principal: String,
    name: String,
    paired_at: u64,
}

/// The devices paired with this host, kept in one file.
// shortcut: a JSON file, enough for SP1's one person. SP4 moves people,
// devices and invites into `crates/persistence` with a migration, when teams
// and company administrators arrive.
#[derive(Debug)]
pub struct Devices {
    file: PathBuf,
    stored: Mutex<Stored>,
}

impl Devices {
    /// Open the device list at `file`; no file yet is an empty list.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read or is not a device list.
    pub fn open(file: impl Into<PathBuf>) -> std::io::Result<Self> {
        let file = file.into();
        let stored = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Stored::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            file,
            stored: Mutex::new(stored),
        })
    }

    /// Whether any device is paired. A host with none prints an owner invite.
    #[must_use]
    pub fn has_devices(&self) -> bool {
        !self.lock().devices.is_empty()
    }

    /// A one-time invite for `principal`, usable until `now` plus fifteen
    /// minutes. Returns the code; only its digest is kept.
    ///
    /// # Errors
    ///
    /// When the device list cannot be saved.
    pub fn invite(&self, principal: &str, now: u64) -> std::io::Result<Token> {
        let code = Token::random();
        let mut stored = self.lock();
        stored.invites.retain(|invite| invite.expires_at > now);
        stored.invites.push(StoredInvite {
            code_digest: code.digest(),
            principal: principal.to_owned(),
            expires_at: now + INVITE_LIFETIME,
        });
        self.save(&stored)?;
        Ok(code)
    }

    /// Spend an invite code once and pair a device. Returns the principal and
    /// the device's token, which is shown once and never stored.
    ///
    /// # Errors
    ///
    /// [`PairError::InvalidInvite`] for an unknown, used or expired code —
    /// which is spent by this call either way — or [`PairError::Unsaved`].
    pub fn redeem(
        &self,
        code: &Token,
        device_name: &str,
        now: u64,
    ) -> Result<(String, Token), PairError> {
        let mut stored = self.lock();
        let digest = code.digest();
        let position = stored
            .invites
            .iter()
            .position(|invite| invite.code_digest == digest);
        let invite = position.map(|at| stored.invites.remove(at));
        let Some(invite) = invite.filter(|invite| invite.expires_at > now) else {
            // Spent even when refused; saving that is best effort.
            let _ = self.save(&stored);
            return Err(PairError::InvalidInvite);
        };
        let token = Token::random();
        stored.next_device += 1;
        let id = stored.next_device;
        stored.devices.push(StoredDevice {
            id,
            token_digest: token.digest(),
            principal: invite.principal.clone(),
            name: device_name.chars().take(100).collect(),
            paired_at: now,
        });
        self.save(&stored).map_err(|_| PairError::Unsaved)?;
        Ok((invite.principal, token))
    }

    /// Who a device token belongs to. `None` fails closed.
    #[must_use]
    pub fn authenticate(&self, token: &Token) -> Option<String> {
        let digest = token.digest();
        self.lock()
            .devices
            .iter()
            .find(|device| device.token_digest == digest)
            .map(|device| device.principal.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Stored> {
        // A panic while holding the lock leaves digests, not secrets; the
        // list is still usable.
        self.stored
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Written beside the file, flushed, then renamed over it.
    fn save(&self, stored: &Stored) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(stored).map_err(std::io::Error::other)?;
        let staging = self.file.with_extension("staging");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut out = options.open(&staging)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        std::fs::rename(&staging, &self.file)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn an_invite_pairs_once_and_the_file_holds_no_secret() {
        let dir = tempfile::tempdir().unwrap();
        let devices = Devices::open(dir.path().join("devices.json")).unwrap();
        assert!(!devices.has_devices());

        let code = devices.invite("owner", 0).unwrap();
        let (principal, token) = devices.redeem(&code, "laptop", 10).unwrap();
        assert_eq!(principal, "owner");
        assert_eq!(devices.authenticate(&token).as_deref(), Some("owner"));
        assert_eq!(
            devices.redeem(&code, "again", 11),
            Err(PairError::InvalidInvite)
        );

        let on_disk = std::fs::read_to_string(dir.path().join("devices.json")).unwrap();
        assert!(!on_disk.contains(token.expose()));
        assert!(!on_disk.contains(code.expose()));

        // Reopened from the file, the device is still known.
        let reopened = Devices::open(dir.path().join("devices.json")).unwrap();
        assert!(reopened.has_devices());
        assert_eq!(reopened.authenticate(&token).as_deref(), Some("owner"));
    }

    #[test]
    fn an_expired_invite_is_refused_and_spent() {
        let dir = tempfile::tempdir().unwrap();
        let devices = Devices::open(dir.path().join("devices.json")).unwrap();
        let code = devices.invite("owner", 0).unwrap();
        assert_eq!(
            devices.redeem(&code, "late", INVITE_LIFETIME),
            Err(PairError::InvalidInvite)
        );
        assert_eq!(
            devices.redeem(&code, "again", 1),
            Err(PairError::InvalidInvite)
        );
        assert!(!devices.has_devices());
    }

    #[test]
    fn a_token_never_prints() {
        let token = Token::new("very-secret");
        assert!(!format!("{token:?}").contains("very-secret"));
    }
}
