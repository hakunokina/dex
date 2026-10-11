//! Owner-only application storage on Unix; existing Windows filesystem behavior is preserved.
use std::{fs::{self, File, OpenOptions}, io, path::Path};

pub fn create_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Repair retained log generations as well as the active file.
pub fn log_dir(path: &Path) -> io::Result<()> {
    create_dir(path)?;
    #[cfg(unix)]
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() { private_file(&entry.path())?; }
    }
    Ok(())
}

pub fn append(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) { create_dir(parent)?; }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// Precreate before SQLite writes anything; new WAL/SHM inherit the DB's mode.
/// Existing sidecars must also be repaired before SQLite can read/recover them.
pub fn sqlite(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if path != Path::new(":memory:") && !path.as_os_str().is_empty() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            match private_file(Path::new(&sidecar)) {
                Ok(()) => {},
                Err(e) if e.kind() == io::ErrorKind::NotFound => {},
                Err(e) => return Err(e),
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
