use std::fs::File;
use std::path::Path;

/// A filesystem object's identity, independent of the name used to reach it.
///
/// Two paths naming the same identity are the same object; two identities that
/// differ are different objects even if the paths are spelled identically.
/// That is what makes it possible to notice a swap.
///
/// Backed by `same-file`, which reads device and inode on Unix and calls
/// `GetFileInformationByHandle` on Windows. The equivalent standard-library
/// accessors on Windows — `volume_serial_number` and `file_index` — are still
/// behind the unstable `windows_by_handle` feature, which CI proved by
/// refusing to compile them.
#[derive(Debug)]
pub(crate) struct FileIdentity(same_file::Handle);

impl PartialEq for FileIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for FileIdentity {}

impl FileIdentity {
    /// Take ownership of an open handle and read its identity.
    ///
    /// The handle is the point: its identity is fixed at open and cannot
    /// change afterwards, however the path is manipulated. Read through
    /// [`Self::file_mut`] so the bytes come from the object that was verified.
    pub(crate) fn of_handle(file: File) -> Option<Self> {
        same_file::Handle::from_file(file).ok().map(Self)
    }

    /// Read the identity a path currently resolves to.
    pub(crate) fn of_path(path: &Path) -> Option<Self> {
        same_file::Handle::from_path(path).ok().map(Self)
    }

    /// The underlying file, for reading.
    pub(crate) fn file_mut(&mut self) -> &mut File {
        self.0.as_file_mut()
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

        let held = FileIdentity::of_handle(File::open(&path).unwrap()).unwrap();
        assert_eq!(held, FileIdentity::of_path(&path).unwrap());
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

        let held = FileIdentity::of_handle(File::open(&path).unwrap()).unwrap();

        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(temp.path().join("other.txt"), &path).unwrap();

        assert_ne!(
            held,
            FileIdentity::of_path(&path).unwrap(),
            "the path now names a different object"
        );
    }
}
