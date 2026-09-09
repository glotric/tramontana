//! The operating system's own credential store.
//!
//! `SECURITY_AND_PERMISSIONS.md` §7 and
//! `docs/decisions/0008-platform-credential-store.md`. macOS Keychain, Windows
//! Credential Manager, and Secret Service on Linux, behind one pair of ports.
//!
//! The shape of this file is a security decision rather than a style one. A
//! credential value appears only as a local and a return value — never as a
//! struct field, never in a `Debug` format, never in an error message, never
//! in a log line, and never as a process argument. Anything that would keep a
//! value alive longer than the call that asked for it is a place it can leak
//! from later.

use async_trait::async_trait;
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, SecretRef};
use tramontana_core::ports::{SecretResolver, SecretVault};

/// The service name every Tramontana credential is filed under.
///
/// One namespace, so a user looking at Keychain Access or Credential Manager
/// sees Tramontana's credentials grouped and can revoke them without guessing
/// which entries are ours.
const SERVICE: &str = "dev.tramontana";

/// Credentials in the platform store.
///
/// Holds no credential material: every operation opens an entry, does one
/// thing, and drops it.
pub struct PlatformSecretVault {
    /// Overrides [`SERVICE`], so a test run can be swept without touching a
    /// developer's real Tramontana credentials.
    service: String,
}

impl std::fmt::Debug for PlatformSecretVault {
    /// Prints the service namespace and nothing else.
    ///
    /// Not derived. A derived `Debug` would print whatever fields this struct
    /// grows later, and the whole point of the type is that some things must
    /// never be printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformSecretVault")
            .field("service", &self.service)
            .finish()
    }
}

impl Default for PlatformSecretVault {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformSecretVault {
    /// The vault the application uses.
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: SERVICE.to_owned(),
        }
    }

    /// A vault filed under a different service name.
    ///
    /// For tests, which must not be able to read or delete the credentials a
    /// developer actually uses.
    #[must_use]
    pub fn under_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Whether a credential store is reachable at all.
    ///
    /// Answered by attempting one read of a name nothing uses. A headless
    /// Linux session has no Secret Service provider and a CI runner has no
    /// desktop session, so a test that needs a real store has to be able to
    /// find out rather than fail with a confusing error.
    #[must_use]
    pub fn is_available(&self) -> bool {
        match keyring::Entry::new(&self.service, "availability.probe") {
            // Both answers mean the store answered.
            Ok(entry) => !matches!(entry.get_password(), Err(error) if is_unavailable(&error)),
            Err(error) => !is_unavailable(&error),
        }
    }

    /// Open the entry for `reference`.
    fn entry(&self, reference: &SecretRef) -> Result<keyring::Entry, ErrorEnvelope> {
        keyring::Entry::new(&self.service, &reference.key).map_err(|error| translate(&error))
    }
}

/// Whether an error means the store itself could not be reached.
///
/// A locked Keychain, a Linux session with no Secret Service provider, and a
/// platform with no store at all are all "unavailable". They are not "no
/// credential is stored", and telling a user with a locked Keychain to sign in
/// again is the failure this distinction exists to prevent.
fn is_unavailable(error: &keyring::Error) -> bool {
    matches!(
        error,
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_)
    )
}

/// Turn a store error into a normalized one, without echoing anything.
///
/// The message names the failure class only. `keyring` errors can carry
/// platform detail, and platform detail around credentials is exactly the sort
/// of thing that ends up in a log.
fn translate(error: &keyring::Error) -> ErrorEnvelope {
    let (code, message) = match error {
        keyring::Error::NoEntry => (
            ErrorCode::CredentialMissing,
            "no credential is stored under that reference",
        ),
        keyring::Error::NoStorageAccess(_) => (
            ErrorCode::ProviderUnavailable,
            "the credential store refused access; it may be locked",
        ),
        keyring::Error::PlatformFailure(_) => (
            ErrorCode::ProviderUnavailable,
            "the credential store is unavailable",
        ),
        keyring::Error::BadEncoding(_) => (
            ErrorCode::ProtocolError,
            "the stored credential is not valid text",
        ),
        keyring::Error::TooLong(_, _) => (
            ErrorCode::LimitExceeded,
            "the credential is longer than the platform store accepts",
        ),
        keyring::Error::Invalid(_, _) => (
            ErrorCode::InvalidRequest,
            "the credential reference is not valid for this platform store",
        ),
        keyring::Error::Ambiguous(_) => (
            ErrorCode::Conflict,
            "more than one credential matches that reference",
        ),
        _ => (
            ErrorCode::ProviderUnavailable,
            "the credential store reported an unrecognized failure",
        ),
    };
    ErrorEnvelope::new(code, message, CorrelationId::new("secret"))
}

#[async_trait]
impl SecretResolver for PlatformSecretVault {
    async fn resolve(&self, reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        // The value is returned straight out. It is never assigned to
        // anything this type owns, so there is nothing here to leak.
        self.entry(reference)?
            .get_password()
            .map_err(|error| translate(&error))
    }
}

#[async_trait]
impl SecretVault for PlatformSecretVault {
    async fn store(&self, reference: &SecretRef, value: &str) -> Result<(), ErrorEnvelope> {
        self.entry(reference)?
            .set_password(value)
            .map_err(|error| translate(&error))
    }

    async fn delete(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope> {
        match self.entry(reference)?.delete_credential() {
            Ok(()) => Ok(true),
            // Absent is not a failure: the caller asked for it to be gone.
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(translate(&error)),
        }
    }

    async fn contains(&self, reference: &SecretRef) -> Result<bool, ErrorEnvelope> {
        match self.entry(reference)?.get_password() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            // An unreachable store is not an absent credential, and must not
            // collapse into `false`.
            Err(error) => Err(translate(&error)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_format_carries_no_credential_material() {
        let vault = PlatformSecretVault::under_service("dev.tramontana.test");
        let rendered = format!("{vault:?}");
        assert!(rendered.contains("dev.tramontana.test"));
        assert!(!rendered.contains("password"), "{rendered}");
    }

    #[test]
    fn a_missing_credential_and_an_unreachable_store_are_different_errors() {
        assert_eq!(
            translate(&keyring::Error::NoEntry).code,
            ErrorCode::CredentialMissing
        );
        assert_eq!(
            translate(&keyring::Error::PlatformFailure(Box::new(
                std::io::Error::other("locked")
            )))
            .code,
            ErrorCode::ProviderUnavailable
        );
    }

    #[test]
    fn no_translated_error_echoes_the_platform_detail() {
        // Platform detail around credentials is what ends up in a log.
        let error = translate(&keyring::Error::PlatformFailure(Box::new(
            std::io::Error::other("keychain item ABC123 for user gasper"),
        )));
        assert!(!error.message.contains("ABC123"), "{}", error.message);
        assert!(!error.message.contains("gasper"), "{}", error.message);
    }
}
