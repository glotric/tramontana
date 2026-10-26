//! Platform capabilities.
//!
//! Path canonicalization today; credentials, process control, and sandboxing
//! join it in M3–M6. Everything here is a thin, testable adapter over an
//! operating-system behavior, so the domain never has to care which platform
//! it is running on.

mod bundle;
mod clock;
mod discovery;
#[cfg(feature = "file-vault")]
mod file_secrets;
mod ids;
mod loopback;
mod paths;
mod profile;
mod project_sources;
mod secrets;
mod skills;

pub use bundle::{BundleError, MANIFEST_FILE, read_bundle, write_bundle};
pub use clock::{SystemClock, SystemLocalClock};
pub use discovery::discover;
#[cfg(feature = "file-vault")]
pub use file_secrets::FileSecretVault;
pub use ids::UuidIdSource;
pub use loopback::{Redirect, RedirectError};
pub use paths::{CanonicalRoot, PathError, canonicalize_root, contains};
pub use profile::ProfileLease;
pub use project_sources::ProjectFiles;
pub use secrets::PlatformSecretVault;
pub use skills::{FsSkillLibrary, SKILL_FILE, SkillSourceError, read_skill_source};
