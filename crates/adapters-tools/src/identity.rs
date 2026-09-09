use std::fs::{File, Metadata};
use std::path::Path;

/// A filesystem object's identity, independent of the name used to reach it.
///
/// Two paths naming the same identity are the same object; two identities that
/// differ are different objects even if the paths are spelled identically.
/// That is what makes it possible to notice a swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    volume: u64,
    file: u64,
}

impl FileIdentity {
    /// Read the identity of an already-open handle.
    ///
    /// The handle is the point: its identity cannot change afterwards, however
    /// the path is manipulated.
    pub(crate) fn of_handle(file: &File) -> Option<Self> {
        file.metadata()
            .ok()
            .and_then(|metadata| Self::of(&metadata))
    }

    /// Read the identity a path currently resolves to.
    pub(crate) fn of_path(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().and_then(|m| Self::of(&m))
    }

    #[cfg(unix)]
    fn of(metadata: &Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            volume: metadata.dev(),
            file: metadata.ino(),
        })
    }

    #[cfg(windows)]
    fn of(metadata: &Metadata) -> Option<Self> {
        use std::os::windows::fs::MetadataExt;
        Some(Self {
            volume: u64::from(metadata.volume_serial_number()?),
            file: metadata.file_index()?,
        })
    }

    #[cfg(not(any(unix, windows)))]
    fn of(_metadata: &Metadata) -> Option<Self> {
        // No identity available. Callers treat `None` as "cannot prove the
        // file did not change", which refuses rather than proceeding.
        None
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
    fn one_file_reached_two_ways_has_one_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        std::fs::write(&path, "x").unwrap();

        let direct = FileIdentity::of_path(&path).unwrap();
        let indirect = FileIdentity::of_path(&temp.path().join(".").join("file.txt")).unwrap();
        assert_eq!(direct, indirect);
    }

    #[test]
    fn two_files_have_different_identities() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "x").unwrap();
        std::fs::write(temp.path().join("b.txt"), "x").unwrap();

        assert_ne!(
            FileIdentity::of_path(&temp.path().join("a.txt")).unwrap(),
            FileIdentity::of_path(&temp.path().join("b.txt")).unwrap()
        );
    }

    #[test]
    fn an_open_handle_reports_the_identity_of_what_it_holds() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        std::fs::write(&path, "x").unwrap();

        let handle = File::open(&path).unwrap();
        assert_eq!(
            FileIdentity::of_handle(&handle).unwrap(),
            FileIdentity::of_path(&path).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_handle_keeps_its_identity_after_the_name_is_repointed() {
        // The property the whole check rests on: replacing the *name* does not
        // change what an already-open handle refers to, so comparing the two
        // reveals that a swap happened.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        std::fs::write(temp.path().join("other.txt"), "swapped").unwrap();

        let handle = File::open(&path).unwrap();
        let held = FileIdentity::of_handle(&handle).unwrap();

        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(temp.path().join("other.txt"), &path).unwrap();

        let now = FileIdentity::of_path(&path).unwrap();
        assert_ne!(held, now, "the path now names a different object");
    }
}
