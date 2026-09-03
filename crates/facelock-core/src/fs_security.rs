use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub fn ensure_mode(path: &Path, mode: u32) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }

    Ok(())
}

pub fn ensure_dir(path: &Path, mode: u32) -> io::Result<()> {
    fs::create_dir_all(path)?;
    ensure_mode(path, mode)
}

pub fn ensure_private_dir(path: &Path, mode: u32) -> io::Result<()> {
    if is_shared_system_dir(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to manage shared system directory {}; configure a dedicated facelock path",
                path.display()
            ),
        ));
    }

    ensure_dir(path, mode)
}

pub fn is_shared_system_dir(path: &Path) -> bool {
    const SHARED_SYSTEM_DIRS: &[&str] = &[
        "/", "/tmp", "/var/tmp", "/var", "/etc", "/var/lib", "/var/log", "/run", "/home", "/root",
        "/usr", "/opt",
    ];

    SHARED_SYSTEM_DIRS.iter().any(|dir| path == Path::new(dir))
}

pub fn create_truncate_file(path: &Path, mode: u32) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);

    #[cfg(unix)]
    {
        options.mode(mode);
    }

    let file = options.open(path)?;
    ensure_mode(path, mode)?;
    Ok(file)
}

/// Create `path` exclusively, never following a symlink and never truncating
/// an existing file.
///
/// `create_truncate_file` is the right primitive for a file this process owns
/// outright. It is the wrong one for a secret two processes may reach for at
/// the same instant: `O_TRUNC` lets the second writer empty the first writer's
/// file, and whichever process reads it in between gets a key nobody's rows
/// were written under. `O_EXCL` makes the race resolve in the kernel — exactly
/// one caller creates the file, the rest are told `AlreadyExists` and can read
/// what the winner wrote. `O_NOFOLLOW` keeps a planted symlink from turning
/// the creation into a write primitive aimed somewhere else.
///
/// The mode is set twice on purpose: `O_CREAT` applies it through the umask,
/// so a caller running under a restrictive umask would otherwise get a file
/// *tighter* than asked for — and one running under a permissive one, on a
/// kernel where the open mode is advisory, a file looser than asked for. The
/// explicit `ensure_mode` makes the requested mode the mode on disk either
/// way, exactly as `create_truncate_file` does.
///
/// The parent directory is **not** created. This primitive writes secrets, and
/// a `create_dir_all` here would mint the directory holding them at whatever
/// the ambient umask happened to be; packaging (tmpfiles, the spec, the
/// scriptlets) owns those directories and their modes. A missing parent is an
/// error that names it.
///
/// Returns `Ok(None)` when the path already exists, which is a normal outcome
/// for a concurrent creator rather than an error.
pub fn create_new_file(path: &Path, mode: u32) -> io::Result<Option<File>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.is_dir()
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "parent directory {} does not exist; it is created by installation, \
                 not by the process writing into it",
                parent.display()
            ),
        ));
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        options.mode(mode);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }

    match options.open(path) {
        Ok(file) => {
            ensure_mode(path, mode)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

/// Open `path` for reading without traversing a symlink at the final
/// component.
///
/// `std::fs::read` follows links, so a link planted where a secret is expected
/// silently redirects the read to whatever it names. Every reader of a
/// facelock key artifact goes through here so the read path refuses exactly
/// what the create path refuses.
pub fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);

    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }

    options.open(path)
}

/// True when `path` itself is a symlink, without following it.
///
/// A separate fact from "the path exists": `Path::exists` follows links, so it
/// answers about the target. Callers that must refuse a planted link need the
/// question asked of the name.
pub fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// Move `from` onto `to` atomically, refusing to replace an existing `to`.
///
/// The raw `renameat2` syscall, for the same reason the purge engine uses it
/// raw (`crates/facelock-core/src/purge/fd.rs`): an old libc must not be able
/// to substitute a check-then-rename fallback, which is precisely the race
/// this call exists to close. `libc::SYS_renameat2` is arch-correct, and a
/// kernel without the syscall fails closed with `ENOSYS`.
///
/// An existing destination — regular file, directory, or symlink, dangling or
/// not — is `EEXIST`, and `from` is left where it is for the caller to clean
/// up. That is what makes this the placement step for a staged secret: the
/// loser of a race never overwrites the winner, and never publishes a
/// half-written file under the real name, because the name only ever appears
/// atomically over a file that is already complete.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let from_c = CString::new(from.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let to_c = CString::new(to.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: both pointers are NUL-terminated and live for the call.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from_c.as_ptr(),
                libc::AT_FDCWD,
                to_c.as_ptr(),
                libc::RENAME_NOREPLACE as libc::c_uint,
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no-replace rename is only implemented on unix",
        ))
    }
}

/// Flush `path`'s directory entry so a file created in it survives a crash.
///
/// `sync_all` on the file itself only promises the data; the name that reaches
/// it lives in the parent directory and needs its own fsync.
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let Some(parent) = parent else {
        return Ok(());
    };
    File::open(parent)?.sync_all()
}

pub fn open_append_file(path: &Path, mode: u32) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true).write(true);

    #[cfg(unix)]
    {
        options.mode(mode);
    }

    let file = options.open(path)?;
    ensure_mode(path, mode)?;
    Ok(file)
}

pub fn write_file(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let mut file = create_truncate_file(path, mode)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "facelock-fs-security-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn ensure_dir_creates_directory() {
        let path = temp_path("dir");
        let _ = fs::remove_dir_all(&path);
        ensure_dir(&path, 0o750).unwrap();
        assert!(path.is_dir());
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn write_file_creates_file() {
        let path = temp_path("file");
        let _ = fs::remove_file(&path);
        write_file(&path, b"test", 0o640).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"test");
        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn write_file_sets_requested_mode() {
        let path = temp_path("mode");
        let _ = fs::remove_file(&path);
        write_file(&path, b"test", 0o640).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn ensure_private_dir_rejects_shared_system_dir() {
        let err = ensure_private_dir(Path::new("/tmp"), 0o750).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // --- The exclusive-create / no-replace-rename pair (#231) ---------------

    #[cfg(unix)]
    #[test]
    fn create_new_file_applies_the_requested_mode_over_the_umask() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");

        // 0666 not because anything should ever be that mode, but because
        // every ordinary umask clips at least one of its bits: a mode that
        // survives intact proves the explicit `ensure_mode` ran rather than
        // the file inheriting whatever `O_CREAT` was allowed to give it. The
        // key file's own 0600 is asserted where the key is created.
        create_new_file(&path, 0o666).unwrap().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o666, "the requested mode must survive the umask");
    }

    #[test]
    fn create_new_file_reports_an_existing_path_instead_of_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        fs::write(&path, b"the winner's bytes").unwrap();

        assert!(
            create_new_file(&path, 0o600).unwrap().is_none(),
            "an existing file is a losing race, not an error"
        );
        assert_eq!(fs::read(&path).unwrap(), b"the winner's bytes");
    }

    #[cfg(unix)]
    #[test]
    fn create_new_file_refuses_a_symlink_standing_at_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        let path = dir.path().join("secret");
        std::os::unix::fs::symlink(&target, &path).unwrap();

        // A dangling link is the dangerous shape: `O_CREAT|O_EXCL` without
        // `O_NOFOLLOW` would create the *target*, turning the link into a
        // write primitive aimed wherever it points.
        assert!(create_new_file(&path, 0o600).unwrap().is_none());
        assert!(!target.exists(), "the link target must not be created");
    }

    #[test]
    fn create_new_file_names_a_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("not-installed");
        let path = parent.join("secret");

        let err = create_new_file(&path, 0o600).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            err.to_string().contains("not-installed"),
            "the error must name the directory that is missing: {err}"
        );
        assert!(
            !parent.exists(),
            "a secret writer must not mint its own parent directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rename_noreplace_refuses_every_shape_of_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let occupied = dir.path().join("occupied");
        let dangling = dir.path().join("dangling");
        fs::write(&staged, b"new").unwrap();
        fs::write(&occupied, b"old").unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), &dangling).unwrap();

        for destination in [&occupied, &dangling] {
            let err = rename_noreplace(&staged, destination).unwrap_err();
            assert_eq!(
                err.raw_os_error(),
                Some(libc::EEXIST),
                "{} must not be replaced",
                destination.display()
            );
        }
        assert_eq!(fs::read(&occupied).unwrap(), b"old");
        assert!(
            staged.exists(),
            "the loser keeps its staging file to clean up"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rename_noreplace_publishes_onto_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let published = dir.path().join("published");
        fs::write(&staged, b"complete").unwrap();

        rename_noreplace(&staged, &published).unwrap();
        assert_eq!(fs::read(&published).unwrap(), b"complete");
        assert!(!staged.exists());
    }

    #[cfg(unix)]
    #[test]
    fn open_no_follow_refuses_a_symlink_and_is_symlink_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        let link = dir.path().join("link");
        fs::write(&target, b"payload").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(is_symlink(&link));
        assert!(!is_symlink(&target));
        assert!(
            link.exists(),
            "`exists` follows the link — that is the trap"
        );
        assert!(open_no_follow(&link).is_err());
        assert_eq!(
            std::io::Read::bytes(open_no_follow(&target).unwrap())
                .collect::<io::Result<Vec<u8>>>()
                .unwrap(),
            b"payload"
        );
    }

    #[test]
    fn sync_parent_dir_flushes_an_existing_parent_and_tolerates_a_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"x").unwrap();
        sync_parent_dir(&path).unwrap();
        sync_parent_dir(Path::new("bare-name")).unwrap();
    }
}
