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

    // Resolve the casing the filesystem actually stores. On a case-insensitive
    // filesystem `realpath` returns the casing the caller *asked* for, so
    // `/work/Project` and `/work/project` resolve to two different strings for
    // one directory. Left alone, that lets a user create two root grants over
    // the same folder and revoke only one of them.
    let resolved = with_stored_case(&resolved);

    if resolved.to_str().is_none() {
        return Err(PathError::NotUtf8(resolved.display().to_string()));
    }

    Ok(CanonicalRoot(resolved))
}

/// The spelling the filesystem stores for `name` inside `parent`.
///
/// Only folds when the component **as given** actually resolves. That single
/// condition separates the two filesystems:
///
/// - case-insensitive: `PROJECT` resolves, so the directory entry is found and
///   its stored spelling (`project`) is used;
/// - case-sensitive: `PROJECT` does not resolve, so it is left alone rather
///   than being silently rewritten into a different directory that does.
///
/// Without the existence check this quietly redirected a path the caller never
/// asked for — which CI caught on Linux, where `PROJECT` and `project` are two
/// directories rather than two spellings of one.
fn stored_spelling(parent: &Path, name: &std::ffi::OsStr) -> std::ffi::OsString {
    if !parent.join(name).exists() {
        return name.to_os_string();
    }
    let Ok(entries) = std::fs::read_dir(parent) else {
        return name.to_os_string();
    };
    // An exact entry always wins. On a case-sensitive filesystem `Project`
    // and `project` can both exist, and taking whichever the listing returns
    // first would rewrite one into the other — the direction that can turn a
    // refusal in [`contains`] into an acceptance.
    let mut folded = None;
    for entry in entries.flatten().map(|entry| entry.file_name()) {
        if entry == name {
            return entry;
        }
        if folded.is_none() && entry.eq_ignore_ascii_case(name) {
            folded = Some(entry);
        }
    }
    folded.unwrap_or_else(|| name.to_os_string())
}

/// Rewrite each component to the casing the filesystem stores for it.
///
/// A no-op on a case-sensitive filesystem, where the requested casing is
/// already the stored casing. On a case-insensitive one it collapses the
/// variants of a path onto the single spelling the directory entry actually
/// has, so two spellings of one directory compare equal.
///
/// Best effort by design: a component whose parent cannot be read is kept as
/// given rather than failing, because the caller has already established that
/// the path resolves.
fn with_stored_case(path: &Path) -> PathBuf {
    let mut rebuilt = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Normal(name) => {
                rebuilt.push(stored_spelling(&rebuilt, name));
            }
            other => rebuilt.push(other.as_os_str()),
        }
    }

    rebuilt
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
    if resolved.starts_with(root.as_path()) {
        return Ok(true);
    }

    // The fast path above answers almost every call. It can be wrong in one
    // direction only — a *false refusal* on a case-insensitive filesystem,
    // where `/work/PROJECT/x` and `/work/project/x` are the same file spelled
    // two ways. Re-checking with the stored casing costs a directory read per
    // component, which is why it happens only after a miss.
    //
    // Note the asymmetry: this can turn a refusal into an acceptance, never an
    // acceptance into a refusal. A path that genuinely lies outside the root
    // has no spelling that puts it inside.
    Ok(with_stored_case(&resolved).starts_with(root.as_path()))
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
    fn two_spellings_of_one_directory_produce_one_root() {
        // On a case-insensitive filesystem `realpath` returns the casing the
        // caller asked for, so `Project` and `project` would otherwise become
        // two root grants over one directory — and revoking one would leave
        // the other authorizing the same files.
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("project")).unwrap();

        let lower = canonicalize_root(&temp.path().join("project")).unwrap();
        let upper = canonicalize_root(&temp.path().join("PROJECT"));

        match upper {
            // Case-insensitive filesystem: both spellings must land on the
            // single spelling the directory entry actually has.
            Ok(upper) => assert_eq!(upper, lower, "one directory must yield one root"),
            // Case-sensitive filesystem: the second spelling is a different
            // directory, and it does not exist.
            Err(error) => assert!(matches!(error, PathError::NotFound(_))),
        }
    }

    #[test]
    fn a_differently_cased_candidate_is_judged_by_what_it_resolves_to() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src").join("file.txt"), "x").unwrap();
        let root = canonicalize_root(&project).unwrap();

        let variant = temp.path().join("PROJECT").join("src").join("file.txt");
        let case_insensitive = temp.path().join("PROJECT").exists();

        if case_insensitive {
            // Same file, different spelling. Refusing it would deny a user
            // access to their own project.
            assert!(contains(&root, &variant).unwrap());
        } else {
            // A genuinely different path that does not exist.
            assert!(!contains(&root, &variant).unwrap_or(false));
        }
    }

    #[test]
    fn a_nonexistent_spelling_is_not_rewritten_into_one_that_exists() {
        // Found by CI on Linux. Folding case unconditionally turned
        // `PROJECT/src` — which does not exist on a case-sensitive filesystem —
        // into `project/src`, which does. That silently answers a question
        // about one path using a different one.
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        let root = canonicalize_root(&project).unwrap();

        let variant = temp.path().join("PROJECT").join("src");
        if variant.exists() {
            // Case-insensitive: the two spellings are one directory, and the
            // folded form is the same place.
            assert!(contains(&root, &variant).unwrap());
        } else {
            // Case-sensitive: `PROJECT` is simply not there, and must not be
            // resolved into the directory that is.
            assert!(!contains(&root, &variant).unwrap_or(false));
        }
    }

    #[test]
    fn case_variance_never_admits_a_path_from_outside_the_root() {
        // The direction that matters. Whatever a filesystem does about case,
        // no spelling may put a sibling inside the root.
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("elsewhere");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();
        let root = canonicalize_root(&project).unwrap();

        for spelling in ["elsewhere", "ELSEWHERE", "Elsewhere"] {
            let candidate = temp.path().join(spelling).join("secret.txt");
            assert!(
                !contains(&root, &candidate).unwrap_or(false),
                "{spelling} must never be inside the root"
            );
        }
    }

    #[test]
    fn a_sibling_differing_only_in_case_is_not_inside_the_root() {
        // Only possible on a case-sensitive filesystem, where `Project` and
        // `project` are two directories. Folding took the first listing entry
        // that matched ignoring case, so `project/x` could be rewritten into
        // `Project/x` and accepted.
        let temp = tempfile::tempdir().unwrap();
        let approved = temp.path().join("Project");
        let sibling = temp.path().join("project");
        std::fs::create_dir_all(&approved).unwrap();
        if std::fs::create_dir(&sibling).is_err() {
            return; // Case-insensitive: one directory, nothing to confuse.
        }
        std::fs::write(sibling.join("secret.txt"), "x").unwrap();
        let root = canonicalize_root(&approved).unwrap();

        assert_eq!(root.as_path().file_name().unwrap(), "Project");
        assert!(!contains(&root, &sibling.join("secret.txt")).unwrap());
        assert!(!contains(&root, &sibling.join("not-yet.txt")).unwrap());
        assert_eq!(
            canonicalize_root(&sibling)
                .unwrap()
                .as_path()
                .file_name()
                .unwrap(),
            "project"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_forward_slash_separator_is_equivalent_on_windows() {
        // Windows accepts both separators. A tool argument written with `/`
        // must be judged the same as one written with `\`.
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src").join("file.txt"), "x").unwrap();
        let root = canonicalize_root(&project).unwrap();

        let with_forward_slashes =
            PathBuf::from(format!("{}/src/file.txt", project.display()).replace('\\', "/"));
        assert!(contains(&root, &with_forward_slashes).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_unc_root_still_contains_its_own_files() {
        // `canonicalize` on Windows returns the `\\?\` verbatim form. A root
        // stored that way must still recognize paths given in ordinary form,
        // or every containment check would fail on Windows.
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("file.txt"), "x").unwrap();

        let root = canonicalize_root(&project).unwrap();
        assert!(
            root.as_str().starts_with("\\\\?\\"),
            "expected a verbatim path, got {}",
            root.as_str()
        );
        assert!(contains(&root, &project.join("file.txt")).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_junction_escaping_the_root_is_refused() {
        // The Windows analogue of the symlink escape. Junctions can be created
        // without elevation, so this is reachable by an ordinary user or by
        // anything running as them.
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();

        let made = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &project.join("escape").to_string_lossy(),
                &outside.to_string_lossy(),
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(made, "could not create a junction to test with");

        let root = canonicalize_root(&project).unwrap();
        assert!(
            !contains(&root, &project.join("escape").join("secret.txt")).unwrap_or(false),
            "a junction must not authorize the directory it points at"
        );
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
