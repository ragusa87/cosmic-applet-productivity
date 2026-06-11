use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Atomically replace `path`'s contents with `bytes` via the write-tmp +
/// `rename(2)` dance, preserving the existing file's permission bits.
///
/// The temporary file is created with the target mode set up front via
/// `OpenOptionsExt::mode`, so a freshly-renamed file always carries the
/// same mode as the original. When the target doesn't yet exist we fall
/// back to `default_mode`.
pub fn write_preserving_mode(path: &Path, bytes: &[u8], default_mode: u32) -> io::Result<()> {
    let mode = std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(default_mode);

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
