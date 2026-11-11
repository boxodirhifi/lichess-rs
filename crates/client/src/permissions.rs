//! Owner-only permissions for the files this client reads and writes.
//!
//! `~/.config/lichess-rs/token` is a Board API secret and `config.toml` may
//! hold private preferences, so both are created with mode 0600 on Unix and the
//! containing directory with 0700. Existing files are never chmod'ed behind the
//! user's back: startup and config hot-reload only warn when a file is
//! group- or world-readable (`mode & 0o044`).
//!
//! Non-Unix platforms use ACLs rather than POSIX modes, so every operation is a
//! no-op there with a debug log.

use std::io;
use std::path::Path;

/// Owner read/write only.
const FILE_MODE: u32 = 0o600;
/// Owner read/write/execute only.
const DIR_MODE: u32 = 0o700;
/// Group- or world-readable bits (the check mirrors the task's `0o044` mask).
#[cfg(unix)]
const SHARED_READ_BITS: u32 = 0o044;

/// Create `dir` (and its parents) when needed and restrict the leaf to 0700.
/// A directory that already exists is left exactly as the user set it.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    set_mode(dir, DIR_MODE, "directory")
}

/// Create `path` with `contents`, owner read/write only (0600).
///
/// On Unix the file is opened with mode 0600 and the mode is then re-applied
/// with `set_permissions`: the open mode is filtered through the umask, and a
/// pre-existing file keeps its old mode. Only call this for files the client is
/// creating or intentionally rewriting.
pub fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(FILE_MODE)
            .open(path)?;
        file.write_all(contents.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)?;
    }
    set_mode(path, FILE_MODE, "file")
}

/// Create an empty owner-only file when `path` is missing. Returns `true` when
/// it created the file and `false` when it already existed (never rewrites it).
pub fn ensure_private_empty_file(path: &Path) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    write_private_file(path, "")?;
    Ok(true)
}

/// Whether other users can read `path` (`mode & 0o044 != 0` on Unix). A missing
/// file is reported as an `io::Error` with `ErrorKind::NotFound`.
#[cfg(unix)]
pub fn is_shared_readable(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)?.permissions().mode();
    Ok(mode & SHARED_READ_BITS != 0)
}

#[cfg(not(unix))]
pub fn is_shared_readable(path: &Path) -> io::Result<bool> {
    log::debug!(
        "Skipping the permission check on {}; this platform uses ACLs",
        path.display()
    );
    Ok(false)
}

/// Warn when `label` (`path`) is readable by the group or anyone else. This is
/// deliberately advisory: the user may have set the mode on purpose, so the
/// client never repairs it automatically.
pub fn warn_if_shared_readable(path: &Path, label: &str) {
    match is_shared_readable(path) {
        Ok(true) => log::warn!(
            "{label} {} is readable by the group or other users; run `chmod 600 {}`",
            path.display(),
            path.display()
        ),
        Ok(false) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log::debug!("Could not check the permissions of {}: {e}", path.display()),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32, kind: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    log::debug!("Set {kind} mode of {} to {mode:o}", path.display());
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32, kind: &str) -> io::Result<()> {
    log::debug!(
        "Skipping POSIX mode {mode:o} for {kind} {}; this platform uses ACLs",
        path.display()
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lichess-rs-permissions-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creating_a_file_sets_owner_only_permissions() {
        let dir = temp_dir("file");
        create_private_dir(&dir).unwrap();
        let path = dir.join("token");
        write_private_file(&path, "secret").unwrap();

        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creating_a_directory_sets_owner_only_permissions() {
        let dir = temp_dir("dir");
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        // An existing directory is the user's choice; never chmod it.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o750);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shared_readable_detects_group_and_world_bits() {
        let dir = temp_dir("warn");
        create_private_dir(&dir).unwrap();
        let path = dir.join("token");
        write_private_file(&path, "secret").unwrap();
        assert!(!is_shared_readable(&path).unwrap());

        for shared in [0o644, 0o604, 0o640, 0o444] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(shared)).unwrap();
            assert!(is_shared_readable(&path).unwrap(), "mode {shared:o}");
        }
        for private in [0o600, 0o400, 0o700] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(private)).unwrap();
            assert!(!is_shared_readable(&path).unwrap(), "mode {private:o}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensuring_a_file_never_rewrites_an_existing_one() {
        let dir = temp_dir("ensure");
        create_private_dir(&dir).unwrap();
        let path = dir.join("token");

        assert!(ensure_private_empty_file(&path).unwrap());
        assert_eq!(mode(&path), 0o600);
        std::fs::write(&path, "existing").unwrap();
        assert!(!ensure_private_empty_file(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing");
        std::fs::remove_dir_all(&dir).ok();
    }
}
