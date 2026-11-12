use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::{CString, OsString},
    fs::File,
    io,
    os::unix::fs::{FileExt, MetadataExt, PermissionsExt as _},
    path::{Component, Path},
    ptr::NonNull,
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(test)]
use std::sync::{Barrier, atomic::AtomicBool};

use cap_std::{
    ambient_authority,
    fs::{Dir, DirBuilder, DirBuilderExt as _, OpenOptions, OpenOptionsExt as _},
};
use rusqlite::{Connection, OpenFlags};
use sqlite_plugin::{
    flags::{AccessFlags, CreateMode, LockLevel, OpenMode, OpenOpts, ShmLockMode},
    vars,
    vfs::{RegisterOpts, Vfs, VfsHandle, VfsResult},
};

use crate::StoreError;

#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0000_0100;
#[cfg(target_os = "macos")]
const O_DIRECTORY: i32 = 0x0010_0000;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0x0002_0000;
#[cfg(target_os = "linux")]
const O_DIRECTORY: i32 = 0x0001_0000;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("the memprove capability VFS currently supports macOS and Linux");

static NEXT_HANDLE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_VFS_ID: AtomicU64 = AtomicU64::new(1);
static REGISTERED_ROOTS: OnceLock<Mutex<HashMap<RootIdentity, RegisteredVfs>>> = OnceLock::new();
static APPROVED_ROOT_BINDINGS: OnceLock<Mutex<HashMap<RootLocator, RootIdentity>>> =
    OnceLock::new();

#[cfg(test)]
const ROOT_IDENTITY_VERIFIED_HOOK: &str = "<approved-root-identity-verified>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RootLocator {
    parent: RootIdentity,
    basename: OsString,
}

#[derive(Clone)]
struct RegisteredVfs {
    name: String,
    vfs: CapabilityVfs,
}

#[derive(Clone)]
pub(crate) struct CapabilityRoot {
    data_dir: Arc<Path>,
    parent: Arc<Dir>,
    basename: Arc<OsString>,
    identity: RootIdentity,
    registered: RegisteredVfs,
}

#[derive(Default)]
struct FileLockState {
    levels: HashMap<u64, LockLevel>,
}

#[derive(Default)]
struct ShmByteLock {
    shared: HashSet<u64>,
    exclusive: Option<u64>,
}

#[derive(Default)]
struct ShmState {
    regions: Mutex<Vec<Box<[u8]>>>,
    locks: Mutex<Vec<ShmByteLock>>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TraceEvent {
    pub(crate) operation: &'static str,
    pub(crate) path: String,
}

#[cfg(test)]
struct RaceHook {
    target: String,
    reached: Arc<Barrier>,
    resume: Arc<Barrier>,
    fired: AtomicBool,
}

#[derive(Clone)]
struct CapabilityVfs {
    root: Arc<Dir>,
    file_locks: Arc<Mutex<HashMap<String, FileLockState>>>,
    shm: Arc<Mutex<HashMap<String, Arc<ShmState>>>>,
    #[cfg(test)]
    trace: Arc<Mutex<Vec<TraceEvent>>>,
    #[cfg(test)]
    race_hook: Arc<Mutex<Option<Arc<RaceHook>>>>,
    #[cfg(test)]
    fail_before_open: Arc<Mutex<Option<String>>>,
}

struct CapabilityHandle {
    id: u64,
    path: String,
    file: File,
    readonly: bool,
    delete_on_close: bool,
    lock_level: LockLevel,
    shm: Arc<ShmState>,
}

impl VfsHandle for CapabilityHandle {
    fn readonly(&self) -> bool {
        self.readonly
    }

    fn in_memory(&self) -> bool {
        false
    }
}

impl CapabilityRoot {
    pub(crate) fn open(data_dir: &Path) -> Result<Self, StoreError> {
        if !data_dir.is_absolute() {
            return Err(StoreError::CapabilityVfs(
                "approved data root must be an absolute path".to_owned(),
            ));
        }
        let parent_path = data_dir.parent().ok_or_else(|| {
            StoreError::CapabilityVfs("approved data root must have a parent directory".to_owned())
        })?;
        let basename = data_dir.file_name().ok_or_else(|| {
            StoreError::CapabilityVfs("approved data root must have a basename".to_owned())
        })?;
        std::fs::create_dir_all(parent_path)?;
        let parent = Dir::open_ambient_dir(parent_path, ambient_authority())?;
        let locator = RootLocator {
            parent: root_identity(&parent)?,
            basename: basename.to_owned(),
        };
        let bindings = APPROVED_ROOT_BINDINGS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut bindings = bindings.lock().map_err(|_| {
            StoreError::CapabilityVfs("approved-root binding lock was poisoned".to_owned())
        })?;
        let expected_identity = bindings.get(&locator).copied();
        let opened_root = match CapabilityVfs::open_directory_component(&parent, basename) {
            Ok(root) => root,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && expected_identity.is_none() =>
            {
                match parent.create_dir(basename) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                CapabilityVfs::open_directory_component(&parent, basename)?
            }
            Err(error) => return Err(root_entry_error(&error)),
        };
        let identity = root_identity(&opened_root)?;
        if expected_identity.is_some_and(|expected| expected != identity) {
            return Err(root_identity_changed(expected_identity, Some(identity)));
        }
        let candidate = CapabilityVfs::from_root(opened_root)?;
        let roots = REGISTERED_ROOTS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut roots = roots
            .lock()
            .map_err(|_| StoreError::CapabilityVfs("VFS registry lock was poisoned".to_owned()))?;
        let registered = if let Some(existing) = roots.get(&identity) {
            existing.clone()
        } else {
            let name = format!(
                "memprove_capability_{}_{}",
                std::process::id(),
                NEXT_VFS_ID.fetch_add(1, Ordering::Relaxed)
            );
            sqlite_plugin::vfs::register_static(
                CString::new(name.as_str()).map_err(|error| {
                    StoreError::CapabilityVfs(format!("invalid VFS name: {error}"))
                })?,
                candidate.clone(),
                RegisterOpts {
                    make_default: false,
                },
            )
            .map_err(|code| {
                StoreError::CapabilityVfs(format!("SQLite VFS registration failed: {code}"))
            })?;
            let registered = RegisteredVfs {
                name,
                vfs: candidate,
            };
            roots.insert(identity, registered.clone());
            registered
        };
        let root = Self {
            data_dir: Arc::from(data_dir),
            parent: Arc::new(parent),
            basename: Arc::new(basename.to_owned()),
            identity,
            registered,
        };
        root.verify_identity()?;
        match bindings.get(&locator) {
            Some(expected) if *expected != identity => {
                return Err(root_identity_changed(Some(*expected), Some(identity)));
            }
            Some(_) => {}
            None => {
                bindings.insert(locator, identity);
            }
        }
        Ok(root)
    }

    pub(crate) fn verify_identity(&self) -> Result<(), StoreError> {
        let opened =
            CapabilityVfs::open_directory_component(&self.parent, self.basename.as_os_str())
                .map_err(|error| root_entry_error(&error))?;
        let actual = root_identity(&opened)?;
        if actual != self.identity {
            return Err(root_identity_changed(Some(self.identity), Some(actual)));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn after_identity_verification_for_test(&self) {
        self.registered
            .vfs
            .fire_race_hook(ROOT_IDENTITY_VERIFIED_HOOK);
    }

    pub(crate) fn ensure_dir_all(&self, relative: &str) -> Result<(), StoreError> {
        self.registered
            .vfs
            .ensure_directory_path(relative)
            .map_err(sqlite_vfs_error)
    }

    pub(crate) fn open_database(&self, relative: &str) -> Result<Connection, StoreError> {
        CapabilityVfs::validated_components(relative).map_err(sqlite_vfs_error)?;
        let flags = OpenFlags::from_bits_truncate(
            OpenFlags::SQLITE_OPEN_READ_WRITE.bits()
                | OpenFlags::SQLITE_OPEN_CREATE.bits()
                | rusqlite::ffi::SQLITE_OPEN_NOFOLLOW,
        );
        Connection::open_with_flags_and_vfs(relative, flags, &self.registered.name)
            .map_err(StoreError::from)
    }

    /// T21 (V1-R-2): [`Self::open_database`] without `SQLITE_OPEN_CREATE`: a
    /// database that does not exist is not created.
    pub(crate) fn open_existing_database(&self, relative: &str) -> Result<Connection, StoreError> {
        CapabilityVfs::validated_components(relative).map_err(sqlite_vfs_error)?;
        let flags = OpenFlags::from_bits_truncate(
            OpenFlags::SQLITE_OPEN_READ_WRITE.bits() | rusqlite::ffi::SQLITE_OPEN_NOFOLLOW,
        );
        Connection::open_with_flags_and_vfs(relative, flags, &self.registered.name)
            .map_err(StoreError::from)
    }

    pub(crate) fn exists(&self, relative: &str) -> Result<bool, StoreError> {
        self.registered
            .vfs
            .path_exists(relative)
            .map_err(sqlite_vfs_error)
    }

    pub(crate) fn cleanup_new_database(&self, relative: &str) -> Result<(), StoreError> {
        let components = CapabilityVfs::validated_components(relative).map_err(sqlite_vfs_error)?;
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let candidate = format!("{relative}{suffix}");
            if self
                .registered
                .vfs
                .path_exists(&candidate)
                .map_err(sqlite_vfs_error)?
            {
                self.registered
                    .vfs
                    .delete(&candidate)
                    .map_err(sqlite_vfs_error)?;
            }
        }
        if components.len() > 1 {
            let parent = components[..components.len() - 1].join("/");
            self.registered
                .vfs
                .remove_empty_directory_path(&parent)
                .map_err(sqlite_vfs_error)?;
        }
        Ok(())
    }

    pub(crate) fn absolute_path(&self, relative: &str) -> Result<std::path::PathBuf, StoreError> {
        CapabilityVfs::validated_components(relative).map_err(sqlite_vfs_error)?;
        Ok(self.data_dir.join(relative))
    }

    pub(crate) fn display_path(&self) -> &Path {
        &self.data_dir
    }

    #[cfg(test)]
    pub(crate) fn install_race_hook(
        &self,
        target: &str,
        reached: Arc<Barrier>,
        resume: Arc<Barrier>,
    ) {
        self.registered
            .vfs
            .install_race_hook(target, reached, resume);
    }

    #[cfg(test)]
    pub(crate) fn install_root_identity_race_hook(
        &self,
        reached: Arc<Barrier>,
        resume: Arc<Barrier>,
    ) {
        self.install_race_hook(ROOT_IDENTITY_VERIFIED_HOOK, reached, resume);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_open(&self, target: &str) {
        self.registered.vfs.fail_next_open(target);
    }

    #[cfg(test)]
    pub(crate) fn trace(&self) -> Vec<TraceEvent> {
        self.registered.vfs.trace()
    }
}

fn root_identity(dir: &Dir) -> io::Result<RootIdentity> {
    let metadata = dir.try_clone()?.into_std_file().metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "approved data root is not a directory",
        ));
    }
    Ok(RootIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn root_entry_error(error: &io::Error) -> StoreError {
    StoreError::CapabilityVfs(format!(
        "approved data root entry is missing, replaced, or not a directory: {error}"
    ))
}

fn root_identity_changed(
    expected: Option<RootIdentity>,
    actual: Option<RootIdentity>,
) -> StoreError {
    StoreError::CapabilityVfs(format!(
        "approved data root identity changed: expected {expected:?}, found {actual:?}"
    ))
}

fn sqlite_vfs_error(code: i32) -> StoreError {
    StoreError::CapabilityVfs(format!("SQLite VFS operation failed: {code}"))
}

impl CapabilityVfs {
    fn from_root(root: Dir) -> io::Result<Self> {
        let vfs = Self {
            root: Arc::new(root),
            file_locks: Arc::default(),
            shm: Arc::default(),
            #[cfg(test)]
            trace: Arc::default(),
            #[cfg(test)]
            race_hook: Arc::default(),
            #[cfg(test)]
            fail_before_open: Arc::default(),
        };
        vfs.ensure_directory_path("tmp").map_err(|code| {
            io::Error::other(format!("cannot create VFS temp directory: {code}"))
        })?;
        Ok(vfs)
    }

    fn validated_components(path: &str) -> VfsResult<Vec<String>> {
        let parsed = Path::new(path);
        if path.is_empty()
            || parsed.is_absolute()
            || path.contains('\\')
            || path
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(vars::SQLITE_CANTOPEN);
        }
        let mut components = Vec::new();
        for component in parsed.components() {
            match component {
                Component::Normal(value) => {
                    let value = value.to_str().ok_or(vars::SQLITE_CANTOPEN)?;
                    if value.is_empty() {
                        return Err(vars::SQLITE_CANTOPEN);
                    }
                    components.push(value.to_owned());
                }
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => return Err(vars::SQLITE_CANTOPEN),
            }
        }
        if components.is_empty() {
            return Err(vars::SQLITE_CANTOPEN);
        }
        Ok(components)
    }

    fn open_directory_component(parent: &Dir, component: impl AsRef<Path>) -> io::Result<Dir> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(O_NOFOLLOW | O_DIRECTORY);
        let opened = parent.open_with(component.as_ref(), &options)?;
        if !opened.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "capability path component is not a directory",
            ));
        }
        Ok(Dir::from_std_file(opened.into_std()))
    }

    /// U2 (S2, N-5, original #131): a directory this VFS creates
    /// (`projects/`, `projects/<PROJECT_ID>/`, `tmp/`) gets mode 0700 through
    /// its descriptor, whatever the umask took from the creation mode.
    fn owner_only(directory: Dir) -> io::Result<Dir> {
        let file = directory.into_std_file();
        file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        Ok(Dir::from_std_file(file))
    }

    fn ensure_directory_path(&self, path: &str) -> VfsResult<()> {
        let components = Self::validated_components(path)?;
        let mut current = self.root.try_clone().map_err(|_| vars::SQLITE_CANTOPEN)?;
        for component in components {
            current = match Self::open_directory_component(&current, &component) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let mut builder = DirBuilder::new();
                    builder.mode(0o700);
                    current
                        .create_dir_with(&component, &builder)
                        .map_err(|_| vars::SQLITE_CANTOPEN)?;
                    let created = Self::open_directory_component(&current, &component)
                        .map_err(|_| vars::SQLITE_CANTOPEN)?;
                    Self::owner_only(created).map_err(|_| vars::SQLITE_CANTOPEN)?
                }
                Err(_) => return Err(vars::SQLITE_CANTOPEN),
            };
        }
        Ok(())
    }

    fn remove_empty_directory_path(&self, path: &str) -> VfsResult<()> {
        let components = Self::validated_components(path)?;
        let (parent, name) = self.resolve_parent(&components)?;
        match parent.remove_dir(name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(vars::SQLITE_IOERR_DELETE),
        }
    }

    fn resolve_parent(&self, components: &[String]) -> VfsResult<(Dir, String)> {
        let (name, parents) = components.split_last().ok_or(vars::SQLITE_CANTOPEN)?;
        let mut current = self.root.try_clone().map_err(|_| vars::SQLITE_CANTOPEN)?;
        for component in parents {
            current = Self::open_directory_component(&current, component)
                .map_err(|_| vars::SQLITE_CANTOPEN)?;
        }
        Ok((current, name.clone()))
    }

    fn path_exists(&self, path: &str) -> VfsResult<bool> {
        let components = Self::validated_components(path)?;
        let (name, parents) = components.split_last().ok_or(vars::SQLITE_CANTOPEN)?;
        let mut current = self.root.try_clone().map_err(|_| vars::SQLITE_CANTOPEN)?;
        for component in parents {
            current = match Self::open_directory_component(&current, component) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(vars::SQLITE_CANTOPEN),
            };
        }
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(O_NOFOLLOW);
        match current.open_with(name, &options) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(vars::SQLITE_CANTOPEN),
        }
    }

    fn directory_identity(dir: &Dir) -> VfsResult<RootIdentity> {
        root_identity(dir).map_err(|_| vars::SQLITE_CANTOPEN)
    }

    fn stable_parent(&self, path: &str) -> VfsResult<(Dir, String)> {
        let components = Self::validated_components(path)?;
        let (first_parent, name) = self.resolve_parent(&components)?;

        #[cfg(test)]
        self.fire_race_hook(path);

        let (second_parent, second_name) = self.resolve_parent(&components)?;
        if name != second_name
            || Self::directory_identity(&first_parent)? != Self::directory_identity(&second_parent)?
        {
            return Err(vars::SQLITE_CANTOPEN);
        }
        Ok((second_parent, name))
    }

    fn named_or_temp_path(path: Option<&str>) -> VfsResult<String> {
        if let Some(path) = path {
            Self::validated_components(path)?;
            return Ok(path.to_owned());
        }
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        Ok(format!("tmp/sqlite-temp-{}-{id}", std::process::id()))
    }

    fn open_capability_file(&self, path: &str, opts: OpenOpts) -> VfsResult<File> {
        let (parent, name) = self.stable_parent(path)?;
        #[cfg(test)]
        {
            let mut failure = self
                .fail_before_open
                .lock()
                .map_err(|_| vars::SQLITE_IOERR)?;
            if failure.as_deref() == Some(path) {
                failure.take();
                return Err(vars::SQLITE_CANTOPEN);
            }
        }

        let mut options = OpenOptions::new();
        options.read(true).custom_flags(O_NOFOLLOW).mode(0o600);
        match opts.mode() {
            OpenMode::ReadOnly => {}
            OpenMode::ReadWrite { create } => {
                options.write(true);
                match create {
                    CreateMode::None => {}
                    CreateMode::Create => {
                        options.create(true);
                    }
                    CreateMode::MustCreate => {
                        options.create_new(true);
                    }
                }
            }
        }
        parent
            .open_with(name, &options)
            .map(cap_std::fs::File::into_std)
            .map_err(|_| vars::SQLITE_CANTOPEN)
    }

    fn shm_for(&self, path: &str) -> VfsResult<Arc<ShmState>> {
        let mut states = self.shm.lock().map_err(|_| vars::SQLITE_IOERR)?;
        Ok(Arc::clone(states.entry(path.to_owned()).or_default()))
    }

    fn release_file_lock(&self, path: &str, handle_id: u64) -> VfsResult<()> {
        let mut locks = self.file_locks.lock().map_err(|_| vars::SQLITE_IOERR)?;
        if let Some(state) = locks.get_mut(path) {
            state.levels.remove(&handle_id);
            if state.levels.is_empty() {
                locks.remove(path);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn record(&self, operation: &'static str, path: &str) {
        if let Ok(mut trace) = self.trace.lock() {
            trace.push(TraceEvent {
                operation,
                path: path.to_owned(),
            });
        }
    }

    #[cfg(test)]
    fn trace(&self) -> Vec<TraceEvent> {
        self.trace
            .lock()
            .map_or_else(|_| Vec::new(), |trace| trace.clone())
    }

    #[cfg(test)]
    fn install_race_hook(&self, target: &str, reached: Arc<Barrier>, resume: Arc<Barrier>) {
        if let Ok(mut hook) = self.race_hook.lock() {
            *hook = Some(Arc::new(RaceHook {
                target: target.to_owned(),
                reached,
                resume,
                fired: AtomicBool::new(false),
            }));
        }
    }

    #[cfg(test)]
    fn fire_race_hook(&self, target: &str) {
        let hook = self.race_hook.lock().ok().and_then(|hook| hook.clone());
        if let Some(hook) = hook
            && hook.target == target
            && !hook.fired.swap(true, Ordering::SeqCst)
        {
            hook.reached.wait();
            hook.resume.wait();
        }
    }

    #[cfg(test)]
    fn fail_next_open(&self, target: &str) {
        if let Ok(mut failure) = self.fail_before_open.lock() {
            *failure = Some(target.to_owned());
        }
    }
}

impl Vfs for CapabilityVfs {
    type Handle = CapabilityHandle;

    fn canonical_path<'a>(&self, path: Cow<'a, str>) -> VfsResult<Cow<'a, str>> {
        Self::validated_components(path.as_ref())?;
        #[cfg(test)]
        self.record("canonical_path", path.as_ref());
        Ok(path)
    }

    fn open(&self, path: Option<&str>, opts: OpenOpts) -> VfsResult<Self::Handle> {
        let path = Self::named_or_temp_path(path)?;
        #[cfg(test)]
        self.record("open", &path);
        let file = self.open_capability_file(&path, opts)?;
        let readonly = opts.mode().is_readonly();
        Ok(CapabilityHandle {
            id: NEXT_HANDLE_ID.fetch_add(1, Ordering::Relaxed),
            shm: self.shm_for(&path)?,
            path,
            file,
            readonly,
            delete_on_close: opts.delete_on_close(),
            lock_level: LockLevel::Unlocked,
        })
    }

    fn delete(&self, path: &str) -> VfsResult<()> {
        #[cfg(test)]
        self.record("delete", path);
        let (parent, name) = self.stable_parent(path)?;
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(O_NOFOLLOW);
        match parent.open_with(&name, &options) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(vars::SQLITE_IOERR_DELETE_NOENT);
            }
            Err(_) => return Err(vars::SQLITE_IOERR_DELETE),
        }
        parent.remove_file(name).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                vars::SQLITE_IOERR_DELETE_NOENT
            } else {
                vars::SQLITE_IOERR_DELETE
            }
        })
    }

    fn access(&self, path: &str, flags: AccessFlags) -> VfsResult<bool> {
        #[cfg(test)]
        self.record("access", path);
        let (parent, name) = self.stable_parent(path)?;
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(O_NOFOLLOW);
        match parent.open_with(name, &options) {
            Ok(file) => match flags {
                AccessFlags::Exists | AccessFlags::Read => Ok(true),
                AccessFlags::ReadWrite => Ok(!file
                    .metadata()
                    .map_err(|_| vars::SQLITE_IOERR_ACCESS)?
                    .permissions()
                    .readonly()),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(vars::SQLITE_IOERR_ACCESS),
        }
    }

    fn file_size(&self, handle: &mut Self::Handle) -> VfsResult<usize> {
        let size = handle
            .file
            .metadata()
            .map_err(|_| vars::SQLITE_IOERR_FSTAT)?
            .len();
        usize::try_from(size).map_err(|_| vars::SQLITE_IOERR_FSTAT)
    }

    fn truncate(&self, handle: &mut Self::Handle, size: usize) -> VfsResult<()> {
        handle
            .file
            .set_len(size as u64)
            .map_err(|_| vars::SQLITE_IOERR_TRUNCATE)
    }

    fn write(&self, handle: &mut Self::Handle, offset: usize, data: &[u8]) -> VfsResult<usize> {
        handle
            .file
            .write_at(data, offset as u64)
            .map_err(|_| vars::SQLITE_IOERR_WRITE)
    }

    fn read(&self, handle: &mut Self::Handle, offset: usize, data: &mut [u8]) -> VfsResult<usize> {
        handle
            .file
            .read_at(data, offset as u64)
            .map_err(|_| vars::SQLITE_IOERR_READ)
    }

    fn lock(&self, handle: &mut Self::Handle, level: LockLevel) -> VfsResult<()> {
        let mut locks = self
            .file_locks
            .lock()
            .map_err(|_| vars::SQLITE_IOERR_LOCK)?;
        let state = locks.entry(handle.path.clone()).or_default();
        let conflict = state.levels.iter().any(|(id, held)| {
            if *id == handle.id {
                return false;
            }
            match level {
                LockLevel::Unlocked => false,
                LockLevel::Shared => *held >= LockLevel::Pending,
                LockLevel::Reserved => *held >= LockLevel::Reserved,
                LockLevel::Pending | LockLevel::Exclusive => *held > LockLevel::Unlocked,
            }
        });
        if conflict {
            return Err(vars::SQLITE_BUSY);
        }
        state.levels.insert(handle.id, level);
        handle.lock_level = level;
        #[cfg(test)]
        self.record("lock", &handle.path);
        Ok(())
    }

    fn unlock(&self, handle: &mut Self::Handle, level: LockLevel) -> VfsResult<()> {
        let mut locks = self
            .file_locks
            .lock()
            .map_err(|_| vars::SQLITE_IOERR_UNLOCK)?;
        if level == LockLevel::Unlocked {
            if let Some(state) = locks.get_mut(&handle.path) {
                state.levels.remove(&handle.id);
            }
        } else {
            locks
                .entry(handle.path.clone())
                .or_default()
                .levels
                .insert(handle.id, level);
        }
        handle.lock_level = level;
        #[cfg(test)]
        self.record("unlock", &handle.path);
        Ok(())
    }

    fn check_reserved_lock(&self, handle: &mut Self::Handle) -> VfsResult<bool> {
        let locked = self
            .file_locks
            .lock()
            .map_err(|_| vars::SQLITE_IOERR_CHECKRESERVEDLOCK)?
            .get(&handle.path)
            .is_some_and(|state| {
                state
                    .levels
                    .iter()
                    .any(|(id, level)| *id != handle.id && *level >= LockLevel::Reserved)
            });
        #[cfg(test)]
        self.record("check_reserved_lock", &handle.path);
        Ok(locked)
    }

    fn sync(&self, handle: &mut Self::Handle) -> VfsResult<()> {
        handle.file.sync_all().map_err(|_| vars::SQLITE_IOERR_FSYNC)
    }

    fn close(&self, handle: Self::Handle) -> VfsResult<()> {
        self.release_file_lock(&handle.path, handle.id)?;
        let path = handle.path.clone();
        let delete_on_close = handle.delete_on_close;
        drop(handle);
        #[cfg(test)]
        self.record("close", &path);
        if delete_on_close {
            match self.delete(&path) {
                Ok(()) | Err(vars::SQLITE_IOERR_DELETE_NOENT) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn sector_size(&self, _handle: &mut Self::Handle) -> VfsResult<i32> {
        Ok(4096)
    }

    fn device_characteristics(&self, _handle: &mut Self::Handle) -> VfsResult<i32> {
        Ok(0)
    }

    fn shm_map(
        &self,
        handle: &mut Self::Handle,
        region_idx: usize,
        region_size: usize,
        extend: bool,
    ) -> VfsResult<Option<NonNull<u8>>> {
        let mut regions = handle
            .shm
            .regions
            .lock()
            .map_err(|_| vars::SQLITE_IOERR_SHMMAP)?;
        if region_idx >= regions.len() {
            if !extend {
                return Ok(None);
            }
            while regions.len() <= region_idx {
                regions.push(vec![0_u8; region_size].into_boxed_slice());
            }
        }
        if regions[region_idx].len() != region_size {
            return Err(vars::SQLITE_IOERR_SHMMAP);
        }
        let pointer = NonNull::new(regions[region_idx].as_mut_ptr());
        drop(regions);
        #[cfg(test)]
        self.record("shm_map", &handle.path);
        Ok(pointer)
    }

    fn shm_lock(
        &self,
        handle: &mut Self::Handle,
        offset: u32,
        count: u32,
        mode: ShmLockMode,
    ) -> VfsResult<()> {
        let start = offset as usize;
        let end = start
            .checked_add(count as usize)
            .ok_or(vars::SQLITE_IOERR_SHMLOCK)?;
        let mut locks = handle
            .shm
            .locks
            .lock()
            .map_err(|_| vars::SQLITE_IOERR_SHMLOCK)?;
        if locks.len() < end {
            locks.resize_with(end, ShmByteLock::default);
        }
        let range = &mut locks[start..end];
        match mode {
            ShmLockMode::LockShared => {
                if range
                    .iter()
                    .any(|lock| lock.exclusive.is_some_and(|owner| owner != handle.id))
                {
                    return Err(vars::SQLITE_BUSY);
                }
                for lock in range {
                    lock.shared.insert(handle.id);
                }
            }
            ShmLockMode::LockExclusive => {
                if range.iter().any(|lock| {
                    lock.exclusive.is_some_and(|owner| owner != handle.id)
                        || lock.shared.iter().any(|owner| *owner != handle.id)
                }) {
                    return Err(vars::SQLITE_BUSY);
                }
                for lock in range {
                    lock.shared.remove(&handle.id);
                    lock.exclusive = Some(handle.id);
                }
            }
            ShmLockMode::UnlockShared => {
                for lock in range {
                    lock.shared.remove(&handle.id);
                }
            }
            ShmLockMode::UnlockExclusive => {
                for lock in range {
                    if lock.exclusive == Some(handle.id) {
                        lock.exclusive = None;
                    }
                }
            }
        }
        drop(locks);
        #[cfg(test)]
        self.record("shm_lock", &handle.path);
        Ok(())
    }

    fn shm_barrier(&self, handle: &mut Self::Handle) {
        std::sync::atomic::fence(Ordering::SeqCst);
        #[cfg(test)]
        self.record("shm_barrier", &handle.path);
        #[cfg(not(test))]
        let _ = handle;
    }

    fn shm_unmap(&self, handle: &mut Self::Handle, _delete: bool) -> VfsResult<()> {
        let mut locks = handle
            .shm
            .locks
            .lock()
            .map_err(|_| vars::SQLITE_IOERR_SHMLOCK)?;
        for lock in locks.iter_mut() {
            lock.shared.remove(&handle.id);
            if lock.exclusive == Some(handle.id) {
                lock.exclusive = None;
            }
        }
        drop(locks);
        #[cfg(test)]
        self.record("shm_unmap", &handle.path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink, thread};

    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, CapabilityRoot) {
        let directory = TempDir::new().expect("approved data root");
        let root = CapabilityRoot::open(directory.path()).expect("capability root");
        (directory, root)
    }

    #[test]
    fn capability_paths_reject_ambient_traversal_and_static_symlinks() {
        let (directory, root) = setup();
        assert!(root.open_database("/tmp/outside.sqlite").is_err());
        assert!(root.open_database("../outside.sqlite").is_err());
        assert!(root.open_database("projects/./outside.sqlite").is_err());

        let outside = TempDir::new().expect("outside directory");
        symlink(outside.path(), directory.path().join("projects")).expect("intermediate symlink");
        assert!(root.ensure_dir_all("projects/alpha").is_err());
        assert!(
            fs::read_dir(outside.path())
                .expect("outside contents")
                .next()
                .is_none()
        );

        fs::remove_file(directory.path().join("projects")).expect("remove intermediate symlink");
        root.ensure_dir_all("projects/alpha")
            .expect("real project directory");
        let outside_file = outside.path().join("outside.sqlite");
        fs::write(&outside_file, b"outside sentinel").expect("outside sentinel");
        symlink(
            &outside_file,
            directory.path().join("projects/alpha/state.sqlite"),
        )
        .expect("final symlink");
        assert!(root.open_database("projects/alpha/state.sqlite").is_err());
        assert_eq!(
            fs::read(outside_file).expect("outside sentinel after open"),
            b"outside sentinel"
        );
    }

    #[test]
    fn deterministic_component_and_final_symlink_swaps_fail_closed() {
        let (directory, root) = setup();
        root.ensure_dir_all("projects/alpha")
            .expect("project directory");
        let outside = TempDir::new().expect("outside directory");
        let reached = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        root.install_race_hook(
            "projects/alpha/state.sqlite",
            Arc::clone(&reached),
            Arc::clone(&resume),
        );
        let opener = {
            let root = root.clone();
            thread::spawn(move || root.open_database("projects/alpha/state.sqlite"))
        };
        reached.wait();
        fs::rename(
            directory.path().join("projects"),
            directory.path().join("projects-original"),
        )
        .expect("move real component");
        symlink(outside.path(), directory.path().join("projects")).expect("swap component");
        resume.wait();
        assert!(opener.join().expect("component race thread").is_err());
        assert!(
            fs::read_dir(outside.path())
                .expect("outside after component race")
                .next()
                .is_none()
        );

        let (directory, root) = setup();
        root.ensure_dir_all("projects/alpha")
            .expect("project directory");
        let outside = TempDir::new().expect("outside directory");
        let outside_file = outside.path().join("outside.sqlite");
        fs::write(&outside_file, b"outside sentinel").expect("outside sentinel");
        let reached = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        root.install_race_hook(
            "projects/alpha/state.sqlite",
            Arc::clone(&reached),
            Arc::clone(&resume),
        );
        let opener = {
            let root = root.clone();
            thread::spawn(move || root.open_database("projects/alpha/state.sqlite"))
        };
        reached.wait();
        symlink(
            &outside_file,
            directory.path().join("projects/alpha/state.sqlite"),
        )
        .expect("swap final component");
        resume.wait();
        assert!(opener.join().expect("final race thread").is_err());
        assert_eq!(
            fs::read(outside_file).expect("outside after final race"),
            b"outside sentinel"
        );
    }

    #[test]
    fn rusqlite_transactions_journals_wal_shm_and_reopen_use_registered_vfs() {
        let (_directory, root) = setup();
        root.ensure_dir_all("projects/alpha")
            .expect("project directory");
        let connection = root
            .open_database("projects/alpha/state.sqlite")
            .expect("open main through registered VFS");
        connection
            .execute_batch(
                "CREATE TABLE durable(value INTEGER NOT NULL);
                 INSERT INTO durable VALUES (1);
                 PRAGMA journal_mode=WAL;
                 INSERT INTO durable VALUES (2);",
            )
            .expect("rollback journal and WAL writes");
        let mut concurrent = root
            .open_database("projects/alpha/state.sqlite")
            .expect("second connection through same VFS");
        let transaction = concurrent.transaction().expect("concurrent transaction");
        transaction
            .execute("INSERT INTO durable VALUES (3)", [])
            .expect("concurrent insert");
        transaction.commit().expect("concurrent commit");
        drop(concurrent);
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
            .expect("checkpoint and restore rollback journal");
        drop(connection);

        let reopened = root
            .open_database("projects/alpha/state.sqlite")
            .expect("reopen through registered VFS");
        assert_eq!(
            reopened
                .query_row("SELECT SUM(value) FROM durable", [], |row| row
                    .get::<_, i64>(0))
                .expect("durable sum"),
            6
        );
        drop(reopened);

        let trace = root.trace();
        assert!(trace.iter().any(|event| {
            event.operation == "open" && event.path == "projects/alpha/state.sqlite"
        }));
        assert!(trace.iter().any(|event| event.path.ends_with("-journal")));
        assert!(trace.iter().any(|event| event.path.ends_with("-wal")));
        assert!(trace.iter().any(|event| event.operation == "shm_map"));
        assert!(trace.iter().any(|event| event.operation == "access"));
        assert!(trace.iter().any(|event| event.operation == "delete"));
        assert!(trace.iter().all(|event| {
            !event.path.starts_with('/')
                && !event.path.split('/').any(|component| component == "..")
        }));
    }
}
