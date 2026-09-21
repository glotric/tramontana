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
    let mut entries = Vec::new();
    if std::fs::metadata(path)?.is_file() {
        match split_parts(path)? {
            // One file is the model.
            None => return fingerprint_file(path),
            // `…-00001-of-00003.gguf` is the first of several parts that
            // llama.cpp loads together from beside it, so all of them are the
            // model, and a change to any of them is a change to it.
            Some(parts) => {
                for part in parts {
                    let fingerprint = fingerprint_file(&part)?;
                    let name = part
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    entries.push((name, "file", fingerprint.sha256, fingerprint.bytes));
                }
            }
        }
    } else {
        collect(path, path, &mut entries)?;
    }
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

/// The parts of a split GGUF model, when `path` names one: the file and its
/// siblings, `<name>-00001-of-0000N.gguf` through `-0000N-of-0000N`.
///
/// # Errors
///
/// [`std::io::ErrorKind::NotFound`] when a part the name promises is missing.
fn split_parts(path: &Path) -> std::io::Result<Option<Vec<std::path::PathBuf>>> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let Some(stem) = name.strip_suffix(".gguf") else {
        return Ok(None);
    };
    // `<prefix>-<index>-of-<total>`, each number five digits.
    let Some((rest, total)) = stem.rsplit_once("-of-") else {
        return Ok(None);
    };
    let Some((prefix, _index)) = rest.rsplit_once('-') else {
        return Ok(None);
    };
    let digits = |text: &str| text.len() == 5 && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(total) || !digits(&rest[prefix.len() + 1..]) {
        return Ok(None);
    }
    let Ok(count) = total.parse::<u32>() else {
        return Ok(None);
    };
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut parts = Vec::new();
    for index in 1..=count {
        let part = directory.join(format!("{prefix}-{index:05}-of-{total}.gguf"));
        if !part.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "part {index} of {count} of the model is missing: {}",
                    part.display()
                ),
            ));
        }
        parts.push(part);
    }
    Ok(Some(parts))
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

    #[test]
    fn a_split_model_is_all_of_its_parts() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("qwen-00001-of-00002.gguf");
        let second = directory.path().join("qwen-00002-of-00002.gguf");
        std::fs::write(&first, b"part one").unwrap();
        std::fs::write(&second, b"part two").unwrap();
        let whole = fingerprint_model(&first).unwrap();
        assert_eq!(whole.bytes, 16, "both parts are counted");

        // A change to a part other than the one named is a change to the model.
        std::fs::write(&second, b"part 2!!").unwrap();
        assert_ne!(fingerprint_model(&first).unwrap().sha256, whole.sha256);

        // A part the name promises must be there.
        std::fs::remove_file(&second).unwrap();
        let missing = fingerprint_model(&first).unwrap_err();
        assert!(missing.to_string().contains("part 2 of 2"), "{missing}");

        // A name that only looks like it is one file.
        let single = directory.path().join("notes-1-of-2.gguf");
        std::fs::write(&single, b"x").unwrap();
        assert_eq!(fingerprint_model(&single).unwrap().bytes, 1);
    }
}
