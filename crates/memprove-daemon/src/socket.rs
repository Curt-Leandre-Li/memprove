//! Local Control API socket binding and cleanup.

use std::{
    fmt, fs, io,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};

use crate::instance::InstanceLock;

/// Permission bits applied to the socket file.
pub const SOCKET_MODE: u32 = 0o600;

/// Socket preparation or binding failure.
#[derive(Debug)]
pub enum SocketError {
    /// Something other than a socket occupies the socket path.
    PathOccupied(PathBuf),
    /// Inspecting, removing, binding, or restricting the socket failed.
    Io {
        /// Socket path.
        path: PathBuf,
        /// Failed step.
        action: &'static str,
        /// Underlying error.
        source: io::Error,
    },
}

impl fmt::Display for SocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathOccupied(path) => write!(
                formatter,
                "{} exists and is not a socket; refusing to replace it",
                path.display()
            ),
            Self::Io {
                path,
                action,
                source,
            } => write!(formatter, "cannot {action} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SocketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PathOccupied(_) => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// A bound socket whose file is removed when the guard is dropped.
#[derive(Debug)]
pub struct BoundSocket {
    listener: Option<UnixListener>,
    path: PathBuf,
    /// Whether a leftover socket from an unclean exit was removed.
    pub removed_stale: bool,
}

impl BoundSocket {
    /// Takes the listener for serving; the guard keeps owning the path.
    ///
    /// # Panics
    ///
    /// Panics if the listener was already taken.
    pub fn take_listener(&mut self) -> UnixListener {
        self.listener.take().expect("listener already taken")
    }

    /// Socket path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        drop(self.listener.take());
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "memproved: cannot remove socket {}: {error}",
                self.path.display()
            ),
        }
    }
}

/// Binds the socket at `path` with mode 0600.
///
/// A leftover socket file can only exist after an unclean exit, because a live
/// daemon holds the instance lock. Requiring `_lock` makes removal impossible
/// without first owning that lock. Non-socket files are never removed.
///
/// # Errors
///
/// Returns [`SocketError`] when the path is occupied by a non-socket or when
/// removal, binding, or permission changes fail.
pub fn bind(_lock: &InstanceLock, path: &Path) -> Result<BoundSocket, SocketError> {
    let io_error = |action, source| SocketError::Io {
        path: path.to_path_buf(),
        action,
        source,
    };
    let removed_stale = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path).map_err(|source| io_error("remove stale socket", source))?;
            true
        }
        Ok(_) => return Err(SocketError::PathOccupied(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(source) => return Err(io_error("inspect", source)),
    };
    let listener = UnixListener::bind(path).map_err(|source| io_error("bind", source))?;
    // From here on the guard owns the path, so every failure removes it.
    let bound = BoundSocket {
        listener: Some(listener),
        path: path.to_path_buf(),
        removed_stale,
    };
    // The data directory is already 0700, so no other user can reach the
    // socket in the window between bind and chmod.
    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
        .map_err(|source| io_error("restrict permissions of", source))?;
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn binds_with_owner_only_mode_and_removes_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(root.path()).unwrap();
        let path = root.path().join("s.sock");
        let bound = bind(&lock, &path).unwrap();
        assert!(!bound.removed_stale);
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, SOCKET_MODE);
        UnixStream::connect(&path).unwrap();
        drop(bound);
        assert!(!path.exists());
    }

    #[test]
    fn stale_socket_is_replaced_under_lock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s.sock");
        // A bound-then-dropped std listener leaves an unconnectable socket file,
        // exactly what SIGKILL leaves behind.
        drop(UnixListener::bind(&path).unwrap());
        // Other tests in this process spawn `git` concurrently. A child holds
        // copies of every parent fd between fork and exec, the listener's
        // among them, so the socket can stay connectable briefly after
        // `drop`. Wait a bounded time for it to stop accepting.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while UnixStream::connect(&path).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "the dropped listener still accepts connections"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let lock = InstanceLock::acquire(root.path()).unwrap();
        let bound = bind(&lock, &path).unwrap();
        assert!(bound.removed_stale);
        UnixStream::connect(&path).unwrap();
    }

    #[test]
    fn non_socket_path_is_never_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s.sock");
        fs::write(&path, b"user data").unwrap();
        let lock = InstanceLock::acquire(root.path()).unwrap();
        let error = bind(&lock, &path).unwrap_err();
        assert!(matches!(error, SocketError::PathOccupied(_)));
        assert_eq!(fs::read(&path).unwrap(), b"user data");
    }
}
