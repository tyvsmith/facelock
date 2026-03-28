use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

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
}
