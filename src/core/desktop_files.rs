//! Client-owned desktop files. Callers must establish local source ownership
//! before entering this boundary; destination paths remain owned by `Host`.
use std::{io, path::Path};
use tty7_core::host::Host;

pub enum CopyError {
    Io(io::Error),
    TooDeep,
    TooLarge,
}

impl From<io::Error> for CopyError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn source_exists(path: &Path) -> bool {
    path.exists()
}

/// Copy a desktop-owned source to its explicitly selected target Host.
/// Replacement staging/commit is owned by the caller; this only copies bytes.
pub fn copy_to_host(
    host: &dyn Host,
    source: &Path,
    destination: &Path,
    max_depth: usize,
    remote_file_max: u64,
) -> Result<(), CopyError> {
    copy_tree(host, source, destination, 0, max_depth, remote_file_max)
}

fn copy_tree(
    host: &dyn Host,
    source: &Path,
    destination: &Path,
    depth: usize,
    max_depth: usize,
    remote_file_max: u64,
) -> Result<(), CopyError> {
    if depth > max_depth {
        return Err(CopyError::TooDeep);
    }
    let metadata = std::fs::metadata(source)?;
    if metadata.is_dir() {
        host.create_dir(destination, true)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            let child = if host.id().is_local() {
                destination.join(&name)
            } else {
                host.join(destination, &name.to_string_lossy())
            };
            copy_tree(
                host,
                &entry.path(),
                &child,
                depth + 1,
                max_depth,
                remote_file_max,
            )?;
        }
    } else if host.id().is_local() {
        // Keep executable bits and non-UTF8 local paths intact.
        std::fs::copy(source, destination)?;
    } else {
        if metadata.len() > remote_file_max {
            return Err(CopyError::TooLarge);
        }
        let bytes = std::fs::read(source)?;
        host.write_file(destination, &bytes)?;
    }
    Ok(())
}

/// Whether a desktop association would execute this local file rather than
/// display it. This does not authorize opening paths from another Host.
pub fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "exe"
                | "com"
                | "bat"
                | "cmd"
                | "scr"
                | "pif"
                | "msi"
                | "ps1"
                | "vbs"
                | "js"
                | "jse"
                | "wsf"
                | "wsh"
                | "cpl"
                | "msc"
                | "hta"
                | "reg"
                | "lnk"
        )
    }
}
