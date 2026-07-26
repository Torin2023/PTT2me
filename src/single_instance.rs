use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::constants::BUNDLE_ID;

#[derive(Debug)]
pub enum LockError {
    AlreadyRunning,
    Io(io::Error),
}

impl From<io::Error> for LockError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// An exclusive lock retained for the lifetime of the process.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    pub fn acquire() -> Result<Self, LockError> {
        let effective_user_id = unsafe { libc::geteuid() };
        let path = std::env::temp_dir().join(format!("{BUNDLE_ID}.{effective_user_id}.lock"));
        Self::acquire_at(&path)
    }

    pub fn acquire_at(path: &Path) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        let descriptor = file.as_raw_fd();

        if unsafe { libc::fchmod(descriptor, 0o600) } != 0 {
            return Err(LockError::Io(io::Error::last_os_error()));
        }

        if unsafe { libc::flock(descriptor, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                Err(LockError::AlreadyRunning)
            } else {
                Err(LockError::Io(error))
            };
        }

        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::{InstanceLock, LockError};

    #[test]
    fn second_lock_is_rejected_until_first_is_dropped() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("instance.lock");

        let first = InstanceLock::acquire_at(&path).unwrap();
        assert!(matches!(
            InstanceLock::acquire_at(&path),
            Err(LockError::AlreadyRunning)
        ));

        drop(first);
        InstanceLock::acquire_at(&path).unwrap();
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}
