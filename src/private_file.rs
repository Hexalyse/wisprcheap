//! Atomic writes of configuration and backups that may contain API keys or sync credentials.

use std::io::Write;
use std::path::Path;

use anyhow::Result;

pub fn protect(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    // NamedTempFile starts with mode 0600 on Unix, before any secret is written.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(contents.as_ref())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_writes_replace_existing_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        write(&path, "old long secret").unwrap();
        write(&path, "new").unwrap();
        append(&path).unwrap().write_all(b" entry").unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "new entry");
    }

    #[cfg(unix)]
    #[test]
    fn private_writes_tighten_permissions_and_replace_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "public").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, "secret").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(append(&path).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let link = dir.path().join("link");
        symlink(&path, &link).unwrap();
        write(&link, "replacement").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        assert!(!std::fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }
}
