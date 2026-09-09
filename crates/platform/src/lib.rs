//! Platform capabilities.
//!
//! Path canonicalization today; credentials, process control, and sandboxing
//! join it in M3–M6. Everything here is a thin, testable adapter over an
//! operating-system behavior, so the domain never has to care which platform
//! it is running on.

mod clock;
mod discovery;
mod ids;
mod paths;
mod profile;

pub use clock::SystemClock;
pub use discovery::discover;
pub use ids::UuidIdSource;
pub use paths::{CanonicalRoot, PathError, canonicalize_root, contains};
pub use profile::ProfileLease;
