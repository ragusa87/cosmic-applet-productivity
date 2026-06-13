use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Atomically replace `path`'s contents with `bytes` via the write-tmp +
/// `rename(2)` dance, preserving the existing file's permission bits.
///
/// The temporary file is created with the target mode set up front via
/// `OpenOptionsExt::mode`, so it never even briefly carries the wider
/// umask-default mode. When the target doesn't yet exist we fall back to
/// `default_mode` — for credentials files that callers pass `0o600`.
pub fn write_preserving_mode(path: &Path, bytes: &[u8], default_mode: u32) -> io::Result<()> {
    let mode = std::fs::metadata(path).map_or(default_mode, |m| m.permissions().mode() & 0o777);

    let mut tmp_os: OsString = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn preserves_existing_0600_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write_preserving_mode(&path, b"{\"new\":true}", 0o644).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode regressed to {mode:o}");
        assert_eq!(fs::read(&path).unwrap(), b"{\"new\":true}");
    }

    #[test]
    fn uses_default_mode_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.json");

        write_preserving_mode(&path, b"x", 0o600).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
