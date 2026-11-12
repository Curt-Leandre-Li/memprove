//! Data-directory preparation and the single-instance lock.

use std::{
    fmt,
    fs::{self, DirBuilder, File, OpenOptions, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::config::{ConfigError, DaemonConfig, LOCK_FILE_NAME};

/// Data-directory or lock failure.
#[derive(Debug)]
pub enum InstanceError {
    /// The data directory could not be created or inspected.
    DataDir {
        /// Directory path.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// The data-directory path exists but is not a directory.
    NotADirectory(PathBuf),
    /// The data directory grants access to group or other users.
    InsecurePermissions {
        /// Directory path.
        path: PathBuf,
        /// Observed permission bits.
        mode: u32,
    },
    /// The data directory is owned by another user.
    ForeignOwner {
        /// Directory path.
        path: PathBuf,
        /// Directory owner.
        owner: u32,
        /// Effective user of this process.
        user: u32,
    },
    /// The canonical data directory is not usable as configured.
    Config(ConfigError),
    /// The lock file could not be opened, locked, or written.
    Lock {
        /// Lock file path.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// Another process holds the lock.
    AlreadyRunning {
        /// Data directory path.
        data_dir: PathBuf,
        /// Process identifier recorded by the holder, when readable.
        holder_pid: Option<u32>,
    },
}

impl fmt::Display for InstanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DataDir { path, source } => write!(
                formatter,
                "cannot prepare data directory {}: {source}",
                path.display()
            ),
            Self::NotADirectory(path) => {
                write!(
                    formatter,
                    "data directory {} is not a directory",
                    path.display()
                )
            }
            Self::InsecurePermissions { path, mode } => write!(
                formatter,
                "data directory {} has mode {mode:04o}; it must not grant group or other \
                 access (expected 0700)",
                path.display()
            ),
            Self::ForeignOwner { path, owner, user } => write!(
                formatter,
                "data directory {} is owned by uid {owner}, not the current user (uid {user})",
                path.display()
            ),
            Self::Config(error) => error.fmt(formatter),
            Self::Lock { path, source } => {
                write!(formatter, "cannot lock {}: {source}", path.display())
            }
            Self::AlreadyRunning {
                data_dir,
                holder_pid,
            } => {
                write!(
                    formatter,
                    "another memproved instance is already running for data directory {}",
                    data_dir.display()
                )?;
                match holder_pid {
                    Some(pid) => write!(formatter, " (lock held by pid {pid})"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl std::error::Error for InstanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DataDir { source, .. } | Self::Lock { source, .. } => Some(source),
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

/// Creates the data directory with mode 0700 when absent, then checks that it
/// is a directory without group/other access, and returns the canonical
/// configuration.
///
/// # Errors
///
/// Returns [`InstanceError`] when the directory cannot be created, is not a
/// directory, is too permissive, or canonicalises to an unusable path.
pub fn prepare_data_dir(config: &DaemonConfig) -> Result<DaemonConfig, InstanceError> {
    let path = config.data_dir();
    let data_dir_error = |source| InstanceError::DataDir {
        path: path.to_path_buf(),
        source,
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(data_dir_error)?;
    let metadata = fs::metadata(path).map_err(data_dir_error)?;
    if !metadata.is_dir() {
        return Err(InstanceError::NotADirectory(path.to_path_buf()));
    }
    let mode = metadata.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(InstanceError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    let canonical = fs::canonicalize(path).map_err(data_dir_error)?;
    DaemonConfig::new(canonical).map_err(InstanceError::Config)
}

/// Exclusive advisory lock proving this process is the only daemon for a data
/// directory. The lock is released when the value is dropped or the process
/// exits, including on `SIGKILL`.
#[derive(Debug)]
pub struct InstanceLock {
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Acquires the lock without blocking and records this process id in it.
    ///
    /// # Errors
    ///
    /// Returns [`InstanceError::AlreadyRunning`] when another process holds the
    /// lock, [`InstanceError::ForeignOwner`] when the data directory belongs to
    /// another user, or [`InstanceError::Lock`] for I/O failures.
    pub fn acquire(data_dir: &Path) -> Result<Self, InstanceError> {
        let path = data_dir.join(LOCK_FILE_NAME);
        let lock_error = |source| InstanceError::Lock {
            path: path.clone(),
            source,
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(lock_error)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(InstanceError::AlreadyRunning {
                    data_dir: data_dir.to_path_buf(),
                    holder_pid: read_pid(&mut file),
                });
            }
            Err(TryLockError::Error(source)) => return Err(lock_error(source)),
        }
        // A file this process opened with O_CREAT is owned by the effective
        // user, so a mismatch means the directory or lock belongs to someone
        // else.
        let user = file.metadata().map_err(lock_error)?.uid();
        let owner = fs::metadata(data_dir).map_err(lock_error)?.uid();
        if owner != user {
            return Err(InstanceError::ForeignOwner {
                path: data_dir.to_path_buf(),
                owner,
                user,
            });
        }
        file.set_len(0).map_err(lock_error)?;
        file.seek(SeekFrom::Start(0)).map_err(lock_error)?;
        writeln!(file, "{}", std::process::id()).map_err(lock_error)?;
        file.sync_all().map_err(lock_error)?;
        Ok(Self { file, path })
    }

    /// Lock file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Process id recorded in the lock file.
    #[must_use]
    pub fn recorded_pid(&mut self) -> Option<u32> {
        read_pid(&mut self.file)
    }
}

fn read_pid(file: &mut File) -> Option<u32> {
    let mut contents = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut contents).ok()?;
    contents.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_for(path: &Path) -> DaemonConfig {
        DaemonConfig::new(path.to_path_buf()).unwrap()
    }

    #[test]
    fn creates_missing_data_dir_with_owner_only_mode() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("nested").join("data");
        let prepared = prepare_data_dir(&config_for(&data)).unwrap();
        let mode = fs::metadata(prepared.data_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert!(prepared.data_dir().is_absolute());
    }

    #[test]
    fn rejects_permissive_data_dir() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let error = prepare_data_dir(&config_for(root.path())).unwrap_err();
        assert!(matches!(
            error,
            InstanceError::InsecurePermissions { mode: 0o755, .. }
        ));
        assert!(error.to_string().contains("0755"));
    }

    #[test]
    fn rejects_file_as_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("plain");
        fs::write(&file, b"x").unwrap();
        assert!(prepare_data_dir(&config_for(&file)).is_err());
    }

    #[test]
    fn second_lock_fails_with_holder_pid_until_first_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let mut first = InstanceLock::acquire(root.path()).unwrap();
        assert_eq!(first.recorded_pid(), Some(std::process::id()));
        let mode = fs::metadata(first.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let error = InstanceLock::acquire(root.path()).unwrap_err();
        match &error {
            InstanceError::AlreadyRunning { holder_pid, .. } => {
                assert_eq!(*holder_pid, Some(std::process::id()));
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
        assert!(error.to_string().contains("already running"));

        drop(first);
        // Other tests in this process spawn `git` concurrently. A child holds
        // copies of every parent fd between fork and exec, the lock file's
        // among them, so the flock can outlive `drop` for that short window.
        // Retry for a bounded time instead of asserting at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match InstanceLock::acquire(root.path()) {
                Ok(_) => break,
                Err(InstanceError::AlreadyRunning { .. })
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("the lock was not released after drop: {error:?}"),
            }
        }
    }

    #[test]
    fn stale_lock_file_contents_are_replaced() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(LOCK_FILE_NAME), b"999999999\nleftover\n").unwrap();
        let mut lock = InstanceLock::acquire(root.path()).unwrap();
        assert_eq!(lock.recorded_pid(), Some(std::process::id()));
    }
}
