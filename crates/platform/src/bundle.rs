//! Reading and writing an export bundle as a directory.
//!
//! The rules about what a bundle may contain live in `tramontana-core`, which
//! has no filesystem. This is the other half: it turns a validated in-memory
//! bundle into files and back, and it makes no decisions of its own.
//!
//! One decision it does enforce is the boundary between a path *claim* and a
//! path *use*. A bundle's manifest names its own files, and those names come
//! from wherever the user got the bundle, so nothing here joins one onto a
//! destination directory before core has agreed it is safe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tramontana_core::contracts::{ExportBundle, ExportManifest};
use tramontana_core::services::is_safe_relative_path;

/// The manifest's own file name. Not itself a declared entry: it is the
/// document that declares the others.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Why a bundle could not be written or read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BundleError {
    /// The directory could not be created, read, or written.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The operating system's reason.
        source: std::io::Error,
    },
    /// The manifest is missing or is not a manifest.
    #[error("{path} is not a readable bundle manifest: {detail}")]
    Manifest {
        /// The manifest path.
        path: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// An entry path could address something outside the bundle directory.
    #[error("the manifest declares an unsafe entry path {path:?}")]
    UnsafePath {
        /// The rejected path, exactly as declared.
        path: String,
    },
    /// A declared file was not there, or a file was not valid text.
    #[error("{path}: {detail}")]
    Entry {
        /// The bundle-relative path.
        path: String,
        /// What went wrong.
        detail: String,
    },
}

fn io(action: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> BundleError {
    let path = path.to_path_buf();
    move |source| BundleError::Io {
        action,
        path,
        source,
    }
}

/// Write a bundle into `directory`, creating it if needed.
///
/// # Errors
///
/// [`BundleError::UnsafePath`] when the bundle declares a path that could
/// escape `directory`, or [`BundleError::Io`] when a write fails.
pub fn write_bundle(bundle: &ExportBundle, directory: &Path) -> Result<(), BundleError> {
    std::fs::create_dir_all(directory).map_err(io("creating", directory))?;

    for (path, contents) in &bundle.files {
        if !is_safe_relative_path(path) {
            return Err(BundleError::UnsafePath { path: path.clone() });
        }
        let destination = directory.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(io("creating", parent))?;
        }
        std::fs::write(&destination, contents).map_err(io("writing", &destination))?;
    }

    let manifest_path = directory.join(MANIFEST_FILE);
    let mut manifest =
        serde_json::to_string_pretty(&bundle.manifest).map_err(|error| BundleError::Manifest {
            path: manifest_path.clone(),
            detail: error.to_string(),
        })?;
    manifest.push('\n');
    std::fs::write(&manifest_path, manifest).map_err(io("writing", &manifest_path))?;

    Ok(())
}

/// Read a bundle from `directory` without trusting it.
///
/// Every file present is returned, including ones the manifest does not
/// declare: core refuses those, and it can only refuse what it is shown.
/// Reading is deliberately not validation — the caller passes the result to
/// `validate_import`, which is the one place the rules live.
///
/// # Errors
///
/// [`BundleError::Manifest`] when the manifest is missing or unreadable,
/// [`BundleError::UnsafePath`] when a declared path could escape the
/// directory, and [`BundleError::Io`] or [`BundleError::Entry`] when a file
/// cannot be read.
pub fn read_bundle(directory: &Path) -> Result<ExportBundle, BundleError> {
    let manifest_path = directory.join(MANIFEST_FILE);
    let document =
        std::fs::read_to_string(&manifest_path).map_err(|error| BundleError::Manifest {
            path: manifest_path.clone(),
            detail: error.to_string(),
        })?;
    let manifest: ExportManifest =
        serde_json::from_str(&document).map_err(|error| BundleError::Manifest {
            path: manifest_path.clone(),
            detail: error.to_string(),
        })?;

    // Paths are checked before any of them is joined onto `directory`, so a
    // hostile manifest never gets to name a file outside the bundle even to
    // have its existence tested.
    for entry in &manifest.entries {
        if !is_safe_relative_path(&entry.path) {
            return Err(BundleError::UnsafePath {
                path: entry.path.clone(),
            });
        }
    }

    let mut files = BTreeMap::new();
    for entry in &manifest.entries {
        let source = directory.join(&entry.path);
        let contents = std::fs::read_to_string(&source).map_err(|error| BundleError::Entry {
            path: entry.path.clone(),
            detail: error.to_string(),
        })?;
        files.insert(entry.path.clone(), contents);
    }

    // Anything else in the directory is surfaced rather than skipped, so the
    // "undeclared entry" rule in core has something to refuse.
    for found in std::fs::read_dir(directory).map_err(io("reading", directory))? {
        let found = found.map_err(io("reading", directory))?;
        let name = found.file_name().to_string_lossy().into_owned();
        if name == MANIFEST_FILE || files.contains_key(&name) {
            continue;
        }
        if found.path().is_file()
            && let Ok(contents) = std::fs::read_to_string(found.path())
        {
            files.insert(name, contents);
        }
    }

    Ok(ExportBundle { manifest, files })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use tramontana_core::contracts::Timestamp;
    use tramontana_core::services::{ExportRequest, ImportLimits, build_export, validate_import};

    use super::*;

    fn bundle() -> ExportBundle {
        build_export(ExportRequest::at(Timestamp::new(
            "2026-01-01T00:00:00.000Z",
        )))
    }

    #[test]
    fn a_bundle_survives_a_directory_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let written = bundle();
        write_bundle(&written, directory.path()).unwrap();

        let read = read_bundle(directory.path()).unwrap();
        assert_eq!(read, written);
        validate_import(&read, ImportLimits::default()).unwrap();
    }

    #[test]
    fn a_manifest_naming_a_path_outside_the_bundle_is_refused_before_it_is_used() {
        let directory = tempfile::tempdir().unwrap();
        let mut hostile = bundle();
        write_bundle(&hostile, directory.path()).unwrap();

        hostile.manifest.entries[0].path = "../../../etc/passwd".to_owned();
        let mut document = serde_json::to_string_pretty(&hostile.manifest).unwrap();
        document.push('\n');
        std::fs::write(directory.path().join(MANIFEST_FILE), document).unwrap();

        match read_bundle(directory.path()) {
            Err(BundleError::UnsafePath { path }) => assert_eq!(path, "../../../etc/passwd"),
            other => panic!("expected an unsafe-path refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_file_the_manifest_does_not_declare_is_surfaced_rather_than_ignored() {
        // Reading hides nothing; core is what refuses it. A reader that
        // silently dropped the file would make the rule unenforceable.
        let directory = tempfile::tempdir().unwrap();
        write_bundle(&bundle(), directory.path()).unwrap();
        std::fs::write(directory.path().join("smuggled.ndjson"), "{}\n").unwrap();

        let read = read_bundle(directory.path()).unwrap();
        assert!(read.files.contains_key("smuggled.ndjson"));
        assert!(validate_import(&read, ImportLimits::default()).is_err());
    }

    #[test]
    fn a_missing_manifest_is_a_clear_refusal_rather_than_an_empty_bundle() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_bundle(directory.path()),
            Err(BundleError::Manifest { .. })
        ));
    }
}
