//! What a runtime's files were, so a later change can be seen (ADR 0043).
//!
//! A local runtime is an executable and a model the person chose. Tramontana
//! did not download them and does not own them, so it cannot vouch for where
//! they came from; what it can do is remember exactly what they were when the
//! person chose them, and notice when that stops being true.
//!
//! Blocking: a model is gigabytes. Callers run these off the async runtime.

use std::io::Read as _;
use std::path::Path;

use sha2::{Digest as _, Sha256};

/// A file's or a folder's SHA-256 and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Lowercase hex.
    pub sha256: String,
    /// Bytes on disk, summed over a folder's files.
    pub bytes: u64,
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// One regular file, streamed.
///
/// # Errors
///
/// Whatever reading it reports, or [`std::io::ErrorKind::InvalidInput`] when
/// the path is not a regular file.
pub fn fingerprint_file(path: &Path) -> std::io::Result<Fingerprint> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a file", path.display()),
        ));
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok(Fingerprint {
        sha256: hex(&hasher.finalize()),
        bytes,
    })
}

/// A model: one file, or an MLX model folder.
///
/// A folder's digest covers every regular file under it, by its path within
/// the folder and its own digest, in a fixed order, so renaming, adding,
/// removing or changing any file changes it. A symbolic link is recorded by
/// where it points and not followed, so the digest cannot reach outside the
/// folder.
///
/// # Errors
///
/// Whatever reading it reports.
pub fn fingerprint_model(path: &Path) -> std::io::Result<Fingerprint> {
    if std::fs::metadata(path)?.is_file() {
        return fingerprint_file(path);
    }
    let mut entries = Vec::new();
    collect(path, path, &mut entries)?;
    entries.sort();
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    for (relative, kind, digest, size) in &entries {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(kind.as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update([b'\n']);
        bytes += size;
    }
    Ok(Fingerprint {
        sha256: hex(&hasher.finalize()),
        bytes,
    })
}

/// Every entry under `directory`: its path relative to `root` with `/`
/// separators, whether it is a file or a link, its digest or target, and its
/// size.
fn collect(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<(String, &'static str, String, u64)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if kind.is_symlink() {
            let target = std::fs::read_link(&path)?;
            entries.push((relative, "link", target.to_string_lossy().into_owned(), 0));
        } else if kind.is_dir() {
            collect(root, &path, entries)?;
        } else if kind.is_file() {
            let fingerprint = fingerprint_file(&path)?;
            entries.push((relative, "file", fingerprint.sha256, fingerprint.bytes));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_its_sha256_and_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.gguf");
        std::fs::write(&path, b"abc").unwrap();
        let fingerprint = fingerprint_file(&path).unwrap();
        // SHA-256 of "abc", from FIPS 180-2's own example.
        assert_eq!(
            fingerprint.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(fingerprint.bytes, 3);
        assert!(fingerprint_file(directory.path()).is_err());
    }

    #[test]
    fn any_change_to_a_model_folder_changes_its_digest() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("model");
        std::fs::create_dir_all(folder.join("tokenizer")).unwrap();
        std::fs::write(folder.join("config.json"), b"{}").unwrap();
        std::fs::write(folder.join("tokenizer/vocab.json"), b"[]").unwrap();
        let first = fingerprint_model(&folder).unwrap();
        assert_eq!(first.bytes, 4);
        assert_eq!(fingerprint_model(&folder).unwrap(), first, "not stable");

        // Changed content.
        std::fs::write(folder.join("config.json"), b"{ }").unwrap();
        let changed = fingerprint_model(&folder).unwrap();
        assert_ne!(changed, first);
        // A renamed file, with the same content.
        std::fs::write(folder.join("config.json"), b"{}").unwrap();
        std::fs::rename(folder.join("config.json"), folder.join("settings.json")).unwrap();
        assert_ne!(fingerprint_model(&folder).unwrap().sha256, first.sha256);
        std::fs::rename(folder.join("settings.json"), folder.join("config.json")).unwrap();
        assert_eq!(fingerprint_model(&folder).unwrap(), first);
        // An added file.
        std::fs::write(folder.join("extra.bin"), b"").unwrap();
        assert_ne!(fingerprint_model(&folder).unwrap().sha256, first.sha256);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_a_model_folder_is_recorded_not_followed() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("model");
        std::fs::create_dir_all(&folder).unwrap();
        let outside = directory.path().join("outside.bin");
        std::fs::write(&outside, b"one").unwrap();
        std::os::unix::fs::symlink(&outside, folder.join("weights.bin")).unwrap();
        let before = fingerprint_model(&folder).unwrap();
        // What the link points at is not part of the folder.
        std::fs::write(&outside, b"two").unwrap();
        assert_eq!(fingerprint_model(&folder).unwrap(), before);
        assert_eq!(before.bytes, 0);
        // Where it points is part of the folder: re-pointing it is a change.
        std::fs::remove_file(folder.join("weights.bin")).unwrap();
        std::os::unix::fs::symlink(
            directory.path().join("elsewhere.bin"),
            folder.join("weights.bin"),
        )
        .unwrap();
        assert_ne!(fingerprint_model(&folder).unwrap().sha256, before.sha256);
    }
}
