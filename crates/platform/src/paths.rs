use std::path::{Component, Path, PathBuf};

/// Why a path could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PathError {
    /// The path does not exist.
    #[error("path does not exist: {0}")]
    NotFound(String),
    /// The path exists but is not a directory.
    #[error("path is not a directory: {0}")]
    NotADirectory(String),
    /// The path could not be resolved.
    #[error("could not resolve path: {0}")]
    Unresolvable(String),
    /// The resolved path is not valid UTF-8.
    ///
    /// Tramontana stores and exports paths as text. A path that cannot be
    /// represented as UTF-8 is rejected rather than lossily converted, because
    /// a lossy root would authorize a directory nobody named.
    #[error("path is not valid UTF-8: {0}")]
    NotUtf8(String),
}

/// A directory that has been resolved and approved as a project root.
///
/// The type is the point: a plain `PathBuf` says nothing about whether anyone
/// checked it. Only [`canonicalize_root`] can produce one, so a function taking
/// a `CanonicalRoot` knows symlinks were followed and the result is a real
/// directory. Storing a *canonical* root is a security property, not tidiness:
/// a grant recorded as a symlink would authorize wherever that link happened to
/// point (`SECURITY_AND_PERMISSIONS.md` §5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalRoot(PathBuf);

impl CanonicalRoot {
    /// The resolved path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The resolved path as text, for storage and display.
    ///
    /// Always valid UTF-8: [`canonicalize_root`] rejects anything else.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }
}

impl std::fmt::Display for CanonicalRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolve a user-chosen directory into an approved root.
///
/// Follows symlinks, resolves `..`, and on Windows produces the verbatim form
/// the operating system reports.
///
/// # Errors
///
/// [`PathError::NotFound`] when nothing is there, [`PathError::NotADirectory`]
/// when it is a file, [`PathError::NotUtf8`] when the resolved path cannot be
/// represented as text, and [`PathError::Unresolvable`] otherwise.
pub fn canonicalize_root(path: &Path) -> Result<CanonicalRoot, PathError> {
    let resolved = std::fs::canonicalize(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => PathError::NotFound(path.display().to_string()),
        _ => PathError::Unresolvable(format!("{}: {error}", path.display())),
    })?;

    let metadata = std::fs::metadata(&resolved)
        .map_err(|error| PathError::Unresolvable(format!("{}: {error}", resolved.display())))?;
    if !metadata.is_dir() {
        return Err(PathError::NotADirectory(resolved.display().to_string()));
    }

    if resolved.to_str().is_none() {
        return Err(PathError::NotUtf8(resolved.display().to_string()));
    }

    Ok(CanonicalRoot(resolved))
}

/// Whether `candidate` resolves to somewhere inside `root`.
///
/// Both sides are canonicalized first, so this is not string prefix matching:
/// a symlink inside the project pointing at `~/.ssh` resolves outside the root
/// and is refused. Comparison is component-wise, so `/project-secrets` is not
/// treated as inside `/project`.
///
/// A candidate that does not exist is resolved through its nearest existing
/// ancestor, which is what makes this usable for a file about to be created.
///
/// # Errors
///
/// [`PathError`] when neither the candidate nor any ancestor can be resolved.
pub fn contains(root: &CanonicalRoot, candidate: &Path) -> Result<bool, PathError> {
    let resolved = resolve_through_existing_ancestor(candidate)?;
    Ok(resolved.starts_with(root.as_path()))
}

/// Canonicalize the nearest existing ancestor and re-apply the rest.
///
/// A file that does not exist yet cannot be canonicalized, but the directory it
/// would live in usually can. Resolving that far still defeats a symlinked
/// parent, which is the escape that matters.
fn resolve_through_existing_ancestor(candidate: &Path) -> Result<PathBuf, PathError> {
    let mut remainder: Vec<Component<'_>> = Vec::new();
    let mut current = candidate;

    loop {
        match std::fs::canonicalize(current) {
            Ok(resolved) => {
                let mut full = resolved;
                for component in remainder.iter().rev() {
                    match component {
                        // `..` after a canonicalized ancestor must still be
                        // applied, or a candidate could climb out unchecked.
                        Component::ParentDir => {
                            full.pop();
                        }
                        Component::CurDir => {}
                        other => full.push(other.as_os_str()),
                    }
                }
                return Ok(full);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(parent) = current.parent() else {
                    return Err(PathError::NotFound(candidate.display().to_string()));
                };
                let Some(last) = current.components().next_back() else {
                    return Err(PathError::NotFound(candidate.display().to_string()));
                };
                remainder.push(last);
                current = parent;
            }
            Err(error) => {
                return Err(PathError::Unresolvable(format!(
                    "{}: {error}",
                    current.display()
                )));
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_canonicalizes_to_an_approved_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = canonicalize_root(temp.path()).unwrap();
        assert!(root.as_path().is_absolute());
        assert!(!root.as_str().is_empty());
    }

    #[test]
    fn a_missing_path_is_reported_not_invented() {
        let temp = tempfile::tempdir().unwrap();
        let error = canonicalize_root(&temp.path().join("nope")).unwrap_err();
        assert!(matches!(error, PathError::NotFound(_)));
    }

    #[test]
    fn a_file_is_not_a_root() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        let error = canonicalize_root(&file).unwrap_err();
        assert!(matches!(error, PathError::NotADirectory(_)));
    }

    #[test]
    fn dot_dot_is_resolved_before_the_root_is_stored() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let climbed = canonicalize_root(&nested.join("..")).unwrap();
        let direct = canonicalize_root(&temp.path().join("a")).unwrap();
        assert_eq!(climbed, direct);
    }

    #[test]
    fn a_path_inside_the_root_is_contained() {
        let temp = tempfile::tempdir().unwrap();
        let root = canonicalize_root(temp.path()).unwrap();
        std::fs::write(temp.path().join("inside.txt"), "x").unwrap();
        assert!(contains(&root, &temp.path().join("inside.txt")).unwrap());
    }

    #[test]
    fn a_file_that_does_not_exist_yet_is_still_checkable() {
        // Writing a new file must be checkable before it exists, or the
        // permission boundary could only ever authorize overwrites.
        let temp = tempfile::tempdir().unwrap();
        let root = canonicalize_root(temp.path()).unwrap();
        assert!(contains(&root, &temp.path().join("not-yet.txt")).unwrap());
    }

    #[test]
    fn traversal_out_of_the_root_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let inner = temp.path().join("project");
        std::fs::create_dir_all(&inner).unwrap();
        let root = canonicalize_root(&inner).unwrap();

        assert!(!contains(&root, &inner.join("..").join("outside.txt")).unwrap());
        assert!(!contains(&root, &inner.join("..").join("..")).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_escaping_the_root_is_refused() {
        // The escape that matters: a link *inside* the project pointing out of
        // it. String prefix matching would accept this; canonicalizing does not.
        let temp = tempfile::tempdir().unwrap();
        let inner = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();

        std::os::unix::fs::symlink(&outside, inner.join("escape")).unwrap();

        let root = canonicalize_root(&inner).unwrap();
        assert!(!contains(&root, &inner.join("escape").join("secret.txt")).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_staying_inside_the_root_is_allowed() {
        let temp = tempfile::tempdir().unwrap();
        let inner = temp.path().join("project");
        std::fs::create_dir_all(inner.join("src")).unwrap();
        std::fs::write(inner.join("src").join("main.rs"), "x").unwrap();
        std::os::unix::fs::symlink(inner.join("src"), inner.join("link")).unwrap();

        let root = canonicalize_root(&inner).unwrap();
        assert!(contains(&root, &inner.join("link").join("main.rs")).unwrap());
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_inside_the_root() {
        // `/tmp/x/project-secrets` must not count as inside `/tmp/x/project`.
        // A naive string prefix check gets this wrong.
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let sibling = temp.path().join("project-secrets");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("key.txt"), "x").unwrap();

        let root = canonicalize_root(&project).unwrap();
        assert!(!contains(&root, &sibling.join("key.txt")).unwrap());
    }

    #[test]
    fn unicode_and_spaces_survive_canonicalization() {
        let temp = tempfile::tempdir().unwrap();
        let awkward = temp.path().join("pro ject — ünïcode");
        std::fs::create_dir_all(&awkward).unwrap();

        let root = canonicalize_root(&awkward).unwrap();
        assert!(root.as_str().contains("ünïcode"));
        assert!(contains(&root, &awkward.join("file.txt")).unwrap());
    }
}
