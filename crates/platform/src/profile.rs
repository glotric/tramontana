use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// Exclusive ownership of runtime execution for one local profile.
///
/// The OS releases the lock after process death. Keep the sidecar file: deleting
/// it while another process has it open could create two independent locks.
/// SQLite uses its own locking; this lease governs recovery and worker ownership.
#[derive(Debug)]
pub struct ProfileLease {
    _file: File,
}

impl ProfileLease {
    /// Acquire execution rights, or return `None` if another runtime owns them.
    /// The database must already exist. Aliases through symlinks resolve to the
    /// same sidecar. Profiles are local files, not network-shared databases.
    pub fn try_acquire(database: &Path) -> io::Result<Option<Self>> {
        let mut lock_path = database.canonicalize()?.into_os_string();
        lock_path.push(".runtime.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn only_one_runtime_owns_a_profile_and_drop_releases_it() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("profile.sqlite");
        File::create(&database).unwrap();
        let owner = ProfileLease::try_acquire(&database).unwrap().unwrap();
        assert!(ProfileLease::try_acquire(&database).unwrap().is_none());
        drop(owner);
        assert!(ProfileLease::try_acquire(&database).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_does_not_bypass_the_runtime_lock() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("profile.sqlite");
        let alias = directory.path().join("alias.sqlite");
        File::create(&database).unwrap();
        std::os::unix::fs::symlink(&database, &alias).unwrap();
        let _owner = ProfileLease::try_acquire(&database).unwrap().unwrap();
        assert!(ProfileLease::try_acquire(&alias).unwrap().is_none());
    }
}
