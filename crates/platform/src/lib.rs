//! Platform capabilities.
//!
//! Path canonicalization today; credentials, process control, and sandboxing
//! join it in M3–M6. Everything here is a thin, testable adapter over an
//! operating-system behavior, so the domain never has to care which platform
//! it is running on.

mod bundle;
mod clock;
mod discovery;
mod ids;
mod paths;
mod profile;
mod project_sources;
mod secrets;

pub use bundle::{BundleError, MANIFEST_FILE, read_bundle, write_bundle};
pub use clock::SystemClock;
pub use discovery::discover;
pub use ids::UuidIdSource;
pub use paths::{CanonicalRoot, PathError, canonicalize_root, contains};
pub use profile::ProfileLease;
pub use project_sources::ProjectFiles;
pub use secrets::PlatformSecretVault;
