//! U1 (E4, ADR 0015): offline `memprove backup` and `memprove restore`.
//!
//! Both run while no daemon runs on the data directory: `backup` holds the
//! daemon's instance lock for as long as it reads, so a daemon cannot start
//! in the middle, and refuses when the lock is held. A backup is a directory
//! holding a copy of every file and directory of the data directory (the
//! registry, each Project's state database, and whatever else is there,
//! content-addressed storage included) except the socket, the lock, and
//! `tmp/`; `continuity-backup.txt`, which records the mode of each entry;
//! and `SHA256SUMS`, in the format `shasum -a 256 -c` reads, covering every
//! other file. `restore` checks `SHA256SUMS` against every file before it
//! writes anything, refuses a target that is not empty, and gives each
//! entry the recorded mode. Into another directory than the backup's
//! source, `restore` also binds the registry's state-database paths to the
//! new directory (U1 U-F, SF-2), and leaves `memproved.lock` and an empty
//! `tmp/` there, as the daemon would. Every `restore` holds the target's
//! instance lock from the empty-target check to its end (U2 S3), and a
//! failed one removes only what it wrote (U1 U-F, SF-2R; U2 S3). Nothing
//! here talks to the daemon.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs::{self, DirBuilder, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use memprove_domain::UtcTimestamp;
use sha2::{Digest, Sha256};

use crate::{SOCKET_FILE_NAME, VERSION_LINE};

/// The daemon's single-instance lock file in the data directory.
pub const LOCK_FILE_NAME: &str = "memproved.lock";
/// The checksum file of a backup.
pub const SUMS_FILE: &str = "SHA256SUMS";
/// The file of a backup that records the mode of each entry.
pub const MANIFEST_FILE: &str = "continuity-backup.txt";
/// The first line of [`MANIFEST_FILE`].
const MANIFEST_HEADER: &str = "memprove-backup 1";
/// The entries of the data directory's top level a backup leaves out.
const EXCLUDED: [&str; 3] = [SOCKET_FILE_NAME, LOCK_FILE_NAME, "tmp"];

/// What a backup or a restore copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// Files copied.
    pub files: usize,
    /// Directories created, the top one included.
    pub directories: usize,
    /// Bytes copied.
    pub bytes: u64,
}

/// One entry of [`MANIFEST_FILE`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    directory: bool,
    mode: u32,
    path: String,
}

/// The instance lock of `data_dir`, held for as long as the value lives.
struct Offline(#[allow(dead_code)] File);

/// Takes the daemon's instance lock of `data_dir`, without writing to it.
fn take_lock(data_dir: &Path, command: &str) -> Result<Offline, String> {
    let path = data_dir.join(LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("cannot open the instance lock {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Offline(file)),
        Err(TryLockError::WouldBlock) => Err(format!(
            "memproved is running on {} (it holds the instance lock {}); {command} works \
             only while the daemon is stopped: stop it (SIGTERM), wait until it has exited, \
             and run {command} again",
            data_dir.display(),
            path.display()
        )),
        Err(TryLockError::Error(error)) => Err(format!(
            "cannot lock the instance lock {}: {error}",
            path.display()
        )),
    }
}

/// The relative path of `path` below `root` as text, with `/` between the
/// parts; refused when a part is not UTF-8 or holds a character the
/// checksum file cannot carry.
fn relative_text(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is not inside {}", path.display(), root.display()))?;
    let text = relative
        .to_str()
        .ok_or_else(|| format!("{} has a name that is not UTF-8", path.display()))?;
    if text.chars().any(|ch| ch.is_control() || ch == '\\') {
        return Err(format!(
            "{} has a name with a control character or a backslash",
            path.display()
        ));
    }
    Ok(text.to_owned())
}

/// Every entry below `directory`, depth first in name order, without the
/// top-level entries a backup leaves out.
fn walk(root: &Path, directory: &Path, entries: &mut Vec<(PathBuf, Entry)>) -> Result<(), String> {
    let mut children = fs::read_dir(directory)
        .map_err(|error| format!("cannot list {}: {error}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot list {}: {error}", directory.display()))?;
    children.sort();
    for path in children {
        if directory == root
            && path
                .file_name()
                .is_some_and(|name| EXCLUDED.iter().any(|excluded| name == *excluded))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        let text = relative_text(root, &path)?;
        if metadata.is_dir() {
            entries.push((
                path.clone(),
                Entry {
                    directory: true,
                    mode,
                    path: text,
                },
            ));
            walk(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.push((
                path,
                Entry {
                    directory: false,
                    mode,
                    path: text,
                },
            ));
        } else {
            return Err(format!(
                "{} is neither a regular file nor a directory (a symbolic link, socket, or \
                 device); a data directory holds none, so nothing was backed up",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Creates `path` with mode 0700 (and any missing parent), refusing one
/// that exists and is not an empty directory.
fn empty_directory(path: &Path, what: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let mut entries = fs::read_dir(path)
                .map_err(|error| format!("cannot list {}: {error}", path.display()))?;
            if entries.next().is_some() {
                return Err(format!(
                    "{what} {} is not empty; choose a new or empty directory",
                    path.display()
                ));
            }
            Ok(false)
        }
        Ok(_) => Err(format!("{what} {} is not a directory", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
            Ok(true)
        }
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// Copies `from` to the new file `to`, mode `mode`, synced; the SHA-256 of
/// what was copied, in lowercase hex, and the byte count.
fn copy_file(from: &Path, to: &Path, mode: u32) -> Result<(String, u64), String> {
    let mut source =
        File::open(from).map_err(|error| format!("cannot read {}: {error}", from.display()))?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(to)
        .map_err(|error| format!("cannot create {}: {error}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", from.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        target
            .write_all(&buffer[..read])
            .map_err(|error| format!("cannot write {}: {error}", to.display()))?;
        bytes += read as u64;
    }
    target
        .sync_all()
        .map_err(|error| format!("cannot write {}: {error}", to.display()))?;
    fs::set_permissions(to, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot set the mode of {}: {error}", to.display()))?;
    Ok((hex(&hasher.finalize()), bytes))
}

/// The SHA-256 of the file `path`, in lowercase hex.
fn file_digest(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
        text
    })
}

/// Writes `contents` to the new file `path`, mode 0600, synced.
fn write_new(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// `memprove backup --out <out>` of the data directory `data_dir`.
///
/// # Errors
///
/// Why nothing (or, when copying failed part way, an incomplete `out`) was
/// written: the daemon runs, `out` is not new or empty or lies inside the
/// data directory, or a file cannot be read or written.
pub fn backup(data_dir: &Path, out: &Path) -> Result<Copied, String> {
    let data_dir = fs::canonicalize(data_dir).map_err(|error| {
        format!(
            "cannot use the data directory {}: {error}",
            data_dir.display()
        )
    })?;
    if !data_dir.is_dir() {
        return Err(format!("{} is not a directory", data_dir.display()));
    }
    if !data_dir.join("registry.sqlite").is_file() {
        return Err(format!(
            "{} holds no registry.sqlite; it is not a memprove data directory",
            data_dir.display()
        ));
    }
    let absolute_out = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot resolve the current directory: {error}"))?
            .join(out)
    };
    let nearest = absolute_out
        .ancestors()
        .find_map(|ancestor| fs::canonicalize(ancestor).ok())
        .unwrap_or_default();
    if nearest.starts_with(&data_dir) {
        return Err(format!(
            "{} is inside the data directory {}; put the backup elsewhere",
            out.display(),
            data_dir.display()
        ));
    }
    let _lock = take_lock(&data_dir, "backup")?;
    let mut entries = Vec::new();
    walk(&data_dir, &data_dir, &mut entries)?;
    empty_directory(&absolute_out, "the backup directory")?;
    let out = fs::canonicalize(&absolute_out)
        .map_err(|error| format!("cannot use {}: {error}", absolute_out.display()))?;
    let root_mode = fs::metadata(&data_dir)
        .map_err(|error| format!("cannot read {}: {error}", data_dir.display()))?
        .permissions()
        .mode()
        & 0o777;
    let mut manifest = format!(
        "{MANIFEST_HEADER}\ncreated_at {}\nsource {}\nbuild {VERSION_LINE}\ndir {root_mode:o} .\n",
        UtcTimestamp::now().as_str(),
        data_dir.display()
    );
    let mut sums = Vec::new();
    let mut copied = Copied {
        files: 0,
        directories: 1,
        bytes: 0,
    };
    for (source, entry) in &entries {
        let target = out.join(&entry.path);
        if entry.directory {
            DirBuilder::new()
                .mode(0o700)
                .create(&target)
                .map_err(|error| format!("cannot create {}: {error}", target.display()))?;
            copied.directories += 1;
        } else {
            let (digest, bytes) = copy_file(source, &target, entry.mode)?;
            sums.push((entry.path.clone(), digest));
            copied.files += 1;
            copied.bytes += bytes;
        }
        writeln!(
            manifest,
            "{} {:o} {}",
            if entry.directory { "dir" } else { "file" },
            entry.mode,
            entry.path
        )
        .expect("writing to a String cannot fail");
    }
    // Directory modes last, so a read-only directory does not stop the
    // copies into it.
    for (_, entry) in entries.iter().rev().filter(|(_, entry)| entry.directory) {
        let target = out.join(&entry.path);
        fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode))
            .map_err(|error| format!("cannot set the mode of {}: {error}", target.display()))?;
    }
    write_new(&out.join(MANIFEST_FILE), &manifest)?;
    sums.push((
        MANIFEST_FILE.to_owned(),
        hex(&Sha256::digest(manifest.as_bytes())),
    ));
    sums.sort();
    let text = sums.iter().fold(String::new(), |mut text, (path, digest)| {
        writeln!(text, "{digest}  {path}").expect("writing to a String cannot fail");
        text
    });
    write_new(&out.join(SUMS_FILE), &text)?;
    File::open(&out)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync {}: {error}", out.display()))?;
    Ok(copied)
}

/// A relative path of a backup: non-empty parts, none of them `.` or `..`.
fn safe_relative(path: &str) -> bool {
    path.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The lines of `SHA256SUMS`: path and digest, each path once.
fn read_sums(from: &Path) -> Result<Vec<(String, String)>, String> {
    let path = from.join(SUMS_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut seen = BTreeSet::new();
    let mut sums = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let bad = || {
            format!(
                "{} line {} is not `<sha256>  <path>`",
                path.display(),
                number + 1
            )
        };
        let (digest, name) = line.split_once("  ").ok_or_else(bad)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !safe_relative(name)
            || name == SUMS_FILE
        {
            return Err(bad());
        }
        if !seen.insert(name.to_owned()) {
            return Err(format!("{} lists {name} twice", path.display()));
        }
        sums.push((name.to_owned(), digest.to_owned()));
    }
    Ok(sums)
}

/// The entries of `continuity-backup.txt`: the top directory's mode, the
/// data directory the backup was taken of (its `source` line), and every
/// other entry.
fn read_manifest(from: &Path) -> Result<(u32, Option<PathBuf>, Vec<Entry>), String> {
    let path = from.join(MANIFEST_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut lines = text.lines();
    if lines.next() != Some(MANIFEST_HEADER) {
        return Err(format!(
            "{} does not start with `{MANIFEST_HEADER}`",
            path.display()
        ));
    }
    let mut root_mode = None;
    let mut source = None;
    let mut entries = Vec::new();
    for line in lines {
        if let Some(path) = line.strip_prefix("source ") {
            source = Some(PathBuf::from(path));
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let (kind, mode, name) = (parts.next(), parts.next(), parts.next());
        let directory = match kind {
            Some("dir") => true,
            Some("file") => false,
            _ => continue,
        };
        let bad = || format!("{} has a malformed entry: {line:?}", path.display());
        let mode = mode
            .and_then(|mode| u32::from_str_radix(mode, 8).ok())
            .filter(|mode| *mode <= 0o777)
            .ok_or_else(bad)?;
        let name = name.ok_or_else(bad)?;
        if name == "." && directory {
            root_mode = Some(mode);
            continue;
        }
        if !safe_relative(name) || name == SUMS_FILE || name == MANIFEST_FILE {
            return Err(bad());
        }
        entries.push(Entry {
            directory,
            mode,
            path: name.to_owned(),
        });
    }
    let root_mode =
        root_mode.ok_or_else(|| format!("{} records no top directory", path.display()))?;
    Ok((root_mode, source, entries))
}

/// Every file below `directory`, as relative text.
fn files_below(root: &Path, directory: &Path, files: &mut BTreeSet<String>) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("cannot list {}: {error}", directory.display()))?
    {
        let path = entry
            .map_err(|error| format!("cannot list {}: {error}", directory.display()))?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if metadata.is_dir() {
            files_below(root, &path, files)?;
        } else {
            files.insert(relative_text(root, &path)?);
        }
    }
    Ok(())
}

/// A backup that [`verify`] checked.
struct Verified {
    /// The data directory's own mode.
    root_mode: u32,
    /// The data directory the backup was taken of.
    source: Option<PathBuf>,
    /// Every other entry, parents before children.
    entries: Vec<Entry>,
    /// Each file's SHA-256, by relative path.
    sums: Vec<(String, String)>,
}

/// Checks the backup `from` without writing anything: every file listed
/// in `SHA256SUMS` has its digest, no other file is there, and
/// `continuity-backup.txt` lists exactly the same files.
fn verify(from: &Path) -> Result<Verified, String> {
    let sums = read_sums(from)?;
    let (root_mode, source, entries) = read_manifest(from)?;
    let mut present = BTreeSet::new();
    files_below(from, from, &mut present)?;
    present.remove(SUMS_FILE);
    let listed: BTreeSet<String> = sums.iter().map(|(name, _)| name.clone()).collect();
    if let Some(extra) = present.difference(&listed).next() {
        return Err(format!(
            "{extra} is in the backup but not in {SUMS_FILE}; nothing was restored"
        ));
    }
    if let Some(missing) = listed.difference(&present).next() {
        return Err(format!(
            "{missing} is listed in {SUMS_FILE} but missing from the backup; nothing was restored"
        ));
    }
    let manifest_files: BTreeSet<String> = entries
        .iter()
        .filter(|entry| !entry.directory)
        .map(|entry| entry.path.clone())
        .chain([MANIFEST_FILE.to_owned()])
        .collect();
    if manifest_files != listed {
        return Err(format!(
            "{MANIFEST_FILE} and {SUMS_FILE} do not list the same files; nothing was restored"
        ));
    }
    for (name, digest) in &sums {
        if file_digest(&from.join(name))? != *digest {
            return Err(format!(
                "{name} does not match its SHA-256 in {SUMS_FILE}; the backup was changed or \
                 damaged, and nothing was restored"
            ));
        }
    }
    Ok(Verified {
        root_mode,
        source,
        entries,
        sums,
    })
}

/// `memprove restore --from <from> --data-dir <data_dir>`.
///
/// # Errors
///
/// Why nothing was restored: the backup does not verify, or the target is
/// not new or empty (a daemon runs there, say), or a file cannot be
/// written or the registry cannot be bound; the target is then left as it
/// was (U1 U-F, SF-2R).
pub fn restore(from: &Path, data_dir: &Path) -> Result<Copied, String> {
    restore_with(from, data_dir, &mut |_| {})
}

/// [`restore`], calling `checked` with the target right after the check
/// that it is new or empty, before anything else happens there (a test
/// starts a daemon, or writes a file of its own, at that moment).
fn restore_with(
    from: &Path,
    data_dir: &Path,
    checked: &mut dyn FnMut(&Path),
) -> Result<Copied, String> {
    let from = fs::canonicalize(from)
        .map_err(|error| format!("cannot use the backup {}: {error}", from.display()))?;
    let Verified {
        root_mode,
        source,
        entries,
        sums,
    } = verify(&from)?;
    if data_dir.join(LOCK_FILE_NAME).exists() {
        take_lock(data_dir, "restore")?;
    }
    // U1 U-F (SF-2R): what a failure must put back: the directories this
    // restore creates (the target and missing parents), or the mode of the
    // empty target that was there.
    let missing = missing_directories(data_dir);
    let created = empty_directory(data_dir, "the data directory")?;
    let target = fs::canonicalize(data_dir)
        .map_err(|error| format!("cannot use {}: {error}", data_dir.display()))?;
    let before = if created {
        Before::Created(missing)
    } else {
        match fs::symlink_metadata(&target) {
            Ok(metadata) => Before::Empty(metadata.permissions().mode() & 0o7777),
            Err(error) => return Err(format!("cannot read {}: {error}", target.display())),
        }
    };
    checked(&target);
    // U2 (S3, N-13, Owner O6): into any directory, the backup's source
    // included, the target's instance lock is taken right after the
    // empty-target check and held until the restore ends, so no daemon can
    // start there in between; a held lock refuses the restore. The lock
    // file stays, empty, as the daemon would leave it.
    let mut written = Written::default();
    let lock_path = target.join(LOCK_FILE_NAME);
    let lock_existed = fs::symlink_metadata(&lock_path).is_ok();
    let taken = take_lock(&target, "restore");
    if !lock_existed && fs::symlink_metadata(&lock_path).is_ok() {
        written.files.push(lock_path);
    }
    let lock = match taken {
        Ok(lock) => lock,
        Err(error) => {
            undo(&target, &before, &written);
            return Err(error);
        }
    };
    // U1 U-F (SF-2): into another directory than the backup's source, the
    // registry is bound to the new place before anything else uses it.
    // Into the same directory the registry is not written.
    let moved = source.filter(|source| *source != target);
    let restored =
        restore_into(&from, &target, root_mode, &entries, &sums, &mut written).and_then(|copied| {
            match &moved {
                Some(source) => {
                    let result = rebind(&target, source);
                    written.store_files(&target);
                    result.map(|()| copied)
                }
                None => Ok(copied),
            }
        });
    match restored {
        Ok(copied) => Ok(copied),
        Err(error) => {
            undo(&target, &before, &written);
            drop(lock);
            Err(format!("{error}; what was restored has been removed again"))
        }
    }
}

/// U2 (S3, N-13): what this restore wrote below the target, so that a
/// failure removes exactly that and nothing another writer put there.
#[derive(Default)]
struct Written {
    /// Files it created, in order (the lock file, the backup's files).
    files: Vec<PathBuf>,
    /// Directories it created, in order.
    directories: Vec<PathBuf>,
    /// What the store created while binding the registry (`tmp/`, a
    /// journal), removed with everything below it.
    store: Vec<PathBuf>,
}

impl Written {
    /// The store's files that the registry binding created: `tmp/` and the
    /// registry's journal files, when they are not the restore's own.
    fn store_files(&mut self, target: &Path) {
        for name in [
            "tmp",
            "registry.sqlite-journal",
            "registry.sqlite-wal",
            "registry.sqlite-shm",
        ] {
            let path = target.join(name);
            if fs::symlink_metadata(&path).is_ok() && !self.files.contains(&path) {
                self.store.push(path);
            }
        }
    }
}

/// U1 U-F (SF-2): binds the restored registry of `target` to it: each
/// Project's `state_db_path` that named its state database below `source`
/// names it below `target`; nothing else changes.
fn rebind(target: &Path, source: &Path) -> Result<(), String> {
    memprove_store_sqlite::rebind_restored_state_paths(target, source)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "cannot bind the restored registry {} to {}: {error}",
                target.join("registry.sqlite").display(),
                target.display()
            )
        })
}

fn restore_into(
    from: &Path,
    target: &Path,
    root_mode: u32,
    entries: &[Entry],
    sums: &[(String, String)],
    written: &mut Written,
) -> Result<Copied, String> {
    let mut copied = Copied {
        files: 0,
        directories: 1,
        bytes: 0,
    };
    for entry in entries {
        let path = target.join(&entry.path);
        if entry.directory {
            DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
            written.directories.push(path);
            copied.directories += 1;
        } else {
            let expected = sums
                .iter()
                .find(|(name, _)| *name == entry.path)
                .map(|(_, digest)| digest.as_str());
            let existed = fs::symlink_metadata(&path).is_ok();
            let result = copy_file(&from.join(&entry.path), &path, entry.mode);
            if !existed && fs::symlink_metadata(&path).is_ok() {
                written.files.push(path.clone());
            }
            let (digest, bytes) = result?;
            if expected != Some(digest.as_str()) {
                return Err(format!("{} changed while it was restored", entry.path));
            }
            copied.files += 1;
            copied.bytes += bytes;
        }
    }
    for entry in entries.iter().rev().filter(|entry| entry.directory) {
        let path = target.join(&entry.path);
        fs::set_permissions(&path, fs::Permissions::from_mode(entry.mode))
            .map_err(|error| format!("cannot set the mode of {}: {error}", path.display()))?;
    }
    fs::set_permissions(target, fs::Permissions::from_mode(root_mode))
        .map_err(|error| format!("cannot set the mode of {}: {error}", target.display()))?;
    File::open(target)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync {}: {error}", target.display()))?;
    Ok(copied)
}

/// What the target of a restore was before it began (U1 U-F, SF-2R).
enum Before {
    /// It did not exist: the directories this restore created, deepest
    /// first (the target, then each missing parent).
    Created(Vec<PathBuf>),
    /// It was an empty directory with this mode.
    Empty(u32),
}

/// `path` and each of its ancestors that does not exist, deepest first.
fn missing_directories(path: &Path) -> Vec<PathBuf> {
    path.ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .take_while(|ancestor| {
            matches!(fs::symlink_metadata(ancestor),
                Err(error) if error.kind() == io::ErrorKind::NotFound)
        })
        .map(Path::to_path_buf)
        .collect()
}

/// Puts `target` back as it was before the restore (U1 U-F, SF-2R; U2
/// S3): it removes what this restore wrote, and only that, not following
/// links: the files it created, what the store created while binding the
/// registry (`tmp/`, a journal), and the directories it created when they
/// are empty again. Then a target this restore created is removed with
/// each missing parent it created, when nothing else is in it, or the
/// empty target that was there gets its mode back.
fn undo(target: &Path, before: &Before, written: &Written) {
    // A restored directory can have a mode without the owner's write bit.
    let writable = fs::Permissions::from_mode(0o700);
    let _ = fs::set_permissions(target, writable.clone());
    for directory in &written.directories {
        let _ = fs::set_permissions(directory, writable.clone());
    }
    for path in &written.store {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                clear(path);
                let _ = fs::remove_dir(path);
            }
            Ok(_) => {
                let _ = fs::remove_file(path);
            }
            Err(_) => {}
        }
    }
    for file in written.files.iter().rev() {
        let _ = fs::remove_file(file);
    }
    for directory in written.directories.iter().rev() {
        let _ = fs::remove_dir(directory);
    }
    match before {
        Before::Created(directories) => {
            for directory in directories {
                let _ = fs::remove_dir(directory);
            }
            if target.exists() {
                let _ = fs::set_permissions(target, fs::Permissions::from_mode(0o700));
            }
        }
        Before::Empty(mode) => {
            let _ = fs::set_permissions(target, fs::Permissions::from_mode(*mode));
        }
    }
}

/// Removes everything below `directory`, deepest first, without following
/// a symbolic link.
fn clear(directory: &Path) {
    let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                clear(&path);
                let _ = fs::remove_dir(&path);
            }
            Ok(_) => {
                let _ = fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data directory `<root>/d` with a registry that is not a database
    /// (a restore elsewhere cannot bind it) and one Project file, backed up
    /// into `<root>/b`.
    fn backed_up(root: &Path) -> (PathBuf, PathBuf) {
        let data = root.join("d");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(data.join("projects/p"))
            .unwrap();
        fs::write(data.join("registry.sqlite"), "not a database\n").unwrap();
        fs::write(data.join("projects/p/state.sqlite"), "state\n").unwrap();
        let out = root.join("b");
        backup(&data, &out).unwrap();
        (data, out)
    }

    /// The names below `directory`, sorted.
    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// U2 (S3, N-13, Owner O6): a restore into the backup's own source
    /// takes the target's instance lock too: a daemon that starts there
    /// after the empty-target check (it holds the lock) makes the restore
    /// refuse, and the target is left as the daemon has it.
    #[test]
    fn u2_s3_a_same_path_restore_refuses_while_the_lock_is_held() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        let (data, out) = backed_up(&root);
        fs::rename(&data, root.join("moved")).unwrap();
        DirBuilder::new().mode(0o700).create(&data).unwrap();
        let mut daemon = None;
        let refused = restore_with(&out, &data, &mut |target| {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(target.join(LOCK_FILE_NAME))
                .unwrap();
            lock.try_lock().unwrap();
            daemon = Some(lock);
        });
        let error = refused.unwrap_err();
        assert!(error.contains("memproved is running"), "{error}");
        assert_eq!(names(&data), [LOCK_FILE_NAME], "the target was written");
        assert!(daemon.is_some());
        drop(daemon);
        // Without a daemon the same restore works; its lock file stays,
        // empty, as a stopped daemon's would.
        fs::remove_file(data.join(LOCK_FILE_NAME)).unwrap();
        restore_with(&out, &data, &mut |_| {}).unwrap();
        assert_eq!(
            names(&data),
            [LOCK_FILE_NAME, "projects", "registry.sqlite"]
        );
    }

    /// U2 (S3): a restore that fails removes only what it wrote: a file
    /// another writer put into the target after the empty-target check is
    /// kept.
    #[test]
    fn u2_s3_a_failed_restore_removes_only_what_it_wrote() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        let (_, out) = backed_up(&root);
        for target in [root.join("t"), root.join("e")] {
            if target.ends_with("e") {
                DirBuilder::new().mode(0o750).create(&target).unwrap();
            }
            let failed = restore_with(&out, &target, &mut |target| {
                fs::write(target.join("foreign"), "not the restore's\n").unwrap();
            });
            let error = failed.unwrap_err();
            assert!(
                error.contains("cannot bind the restored registry"),
                "{error}"
            );
            assert_eq!(names(&target), ["foreign"], "{}", target.display());
            assert_eq!(
                fs::read_to_string(target.join("foreign")).unwrap(),
                "not the restore's\n"
            );
        }
        assert_eq!(
            fs::symlink_metadata(root.join("e"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }

    #[test]
    fn relative_paths_of_a_backup_stay_inside_it() {
        for good in ["registry.sqlite", "projects/a/state.sqlite"] {
            assert!(safe_relative(good), "{good}");
        }
        for bad in ["", "/etc/passwd", "../x", "a/../b", "./a", "a/", "a//b/."] {
            assert!(!safe_relative(bad), "{bad}");
        }
    }
}
