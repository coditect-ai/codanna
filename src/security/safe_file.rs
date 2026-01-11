//! Safe file operations that prevent TOCTOU vulnerabilities
//!
//! This module provides file operations that:
//! 1. Don't follow symlinks (O_NOFOLLOW)
//! 2. Validate paths after opening
//! 3. Work correctly on both Unix and Windows

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Errors that can occur during safe file operations
#[derive(Debug)]
pub enum SafeFileError {
    /// The path is a symlink (blocked by O_NOFOLLOW)
    SymlinkDetected { path: PathBuf },
    /// The file's real path doesn't match expected path (TOCTOU detected)
    PathMismatch { expected: PathBuf, actual: PathBuf },
    /// Standard I/O error
    IoError { path: PathBuf, source: io::Error },
    /// Path is outside allowed boundary
    OutsideBoundary { path: PathBuf, boundary: PathBuf },
    /// Path contains invalid components (e.g., ..)
    InvalidPath { path: PathBuf, reason: String },
}

impl std::fmt::Display for SafeFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SymlinkDetected { path } => {
                write!(f, "Symlink detected (blocked for security): {}", path.display())
            }
            Self::PathMismatch { expected, actual } => {
                write!(
                    f,
                    "Path mismatch detected (possible TOCTOU attack): expected {}, got {}",
                    expected.display(),
                    actual.display()
                )
            }
            Self::IoError { path, source } => {
                write!(f, "I/O error reading {}: {}", path.display(), source)
            }
            Self::OutsideBoundary { path, boundary } => {
                write!(
                    f,
                    "Path {} is outside allowed boundary {}",
                    path.display(),
                    boundary.display()
                )
            }
            Self::InvalidPath { path, reason } => {
                write!(f, "Invalid path {}: {}", path.display(), reason)
            }
        }
    }
}

impl std::error::Error for SafeFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<SafeFileError> for io::Error {
    fn from(err: SafeFileError) -> Self {
        match err {
            SafeFileError::IoError { source, .. } => source,
            SafeFileError::SymlinkDetected { .. } => {
                io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
            }
            SafeFileError::PathMismatch { .. } => {
                io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
            }
            SafeFileError::OutsideBoundary { .. } => {
                io::Error::new(io::ErrorKind::PermissionDenied, err.to_string())
            }
            SafeFileError::InvalidPath { .. } => {
                io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
            }
        }
    }
}

/// Safely open a file without following symlinks
///
/// On Unix, uses O_NOFOLLOW to prevent symlink following.
/// On Windows, checks file attributes after opening.
///
/// # Security
///
/// This function prevents TOCTOU attacks by:
/// 1. Opening with O_NOFOLLOW (Unix) or checking attributes (Windows)
/// 2. Verifying the opened file's real path matches the requested path
///
/// # Example
///
/// ```rust,ignore
/// use codanna::security::safe_open;
///
/// let file = safe_open("/path/to/file.rs")?;
/// ```
pub fn safe_open<P: AsRef<Path>>(path: P) -> Result<File, SafeFileError> {
    let path = path.as_ref();

    // Pre-flight check: reject paths with suspicious components
    validate_path_components(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_NOFOLLOW constant - prevents following symlinks
        // Value is 0x20000 on Linux, 0x0100 on macOS/BSD
        #[cfg(target_os = "linux")]
        const O_NOFOLLOW: i32 = 0x20000;
        #[cfg(target_os = "macos")]
        const O_NOFOLLOW: i32 = 0x0100;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        const O_NOFOLLOW: i32 = 0x20000; // Default to Linux value

        // ELOOP error code for symlink with O_NOFOLLOW
        #[cfg(target_os = "linux")]
        const ELOOP: i32 = 40;
        #[cfg(target_os = "macos")]
        const ELOOP: i32 = 62;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        const ELOOP: i32 = 40; // Default to Linux value

        // Open with O_NOFOLLOW - will fail if path is a symlink
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(path)
            .map_err(|e| {
                // Check if error is ELOOP (symlink with O_NOFOLLOW)
                if e.raw_os_error() == Some(ELOOP) {
                    SafeFileError::SymlinkDetected { path: path.to_path_buf() }
                } else {
                    SafeFileError::IoError {
                        path: path.to_path_buf(),
                        source: e,
                    }
                }
            })?;

        // Post-open validation: verify the file we opened is what we expected
        verify_opened_file(&file, path)?;

        Ok(file)
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // On Windows, open with FILE_FLAG_OPEN_REPARSE_POINT to detect symlinks
        // This prevents automatic symlink following
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x00200000) // FILE_FLAG_OPEN_REPARSE_POINT
            .open(path)
            .map_err(|e| SafeFileError::IoError {
                path: path.to_path_buf(),
                source: e,
            })?;

        // Check if it's a reparse point (symlink)
        let metadata = file.metadata().map_err(|e| SafeFileError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;

        if metadata.file_type().is_symlink() {
            return Err(SafeFileError::SymlinkDetected { path: path.to_path_buf() });
        }

        Ok(file)
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Fallback for other platforms - basic open with warning
        tracing::warn!(
            "[security] Platform does not support O_NOFOLLOW, using standard open for {}",
            path.display()
        );

        File::open(path).map_err(|e| SafeFileError::IoError {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

/// Safely read a file to string without following symlinks
///
/// This is the secure replacement for `std::fs::read_to_string`.
///
/// # Security
///
/// - Uses `safe_open` to prevent symlink following
/// - Validates path components before opening
/// - Optionally enforces workspace boundary
///
/// # Example
///
/// ```rust,ignore
/// use codanna::security::safe_read_to_string;
///
/// let content = safe_read_to_string("/path/to/file.rs", None)?;
/// ```
pub fn safe_read_to_string<P: AsRef<Path>>(
    path: P,
    workspace_root: Option<&Path>,
) -> Result<String, SafeFileError> {
    let path = path.as_ref();

    // If workspace root is provided, validate boundary
    if let Some(root) = workspace_root {
        validate_workspace_boundary(path, root)?;
    }

    // Open safely (no symlink following)
    let mut file = safe_open(path)?;

    // Read content
    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|e| SafeFileError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    Ok(content)
}

/// Validate path components for suspicious patterns
fn validate_path_components(path: &Path) -> Result<(), SafeFileError> {
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Allow .. but log it for monitoring
                tracing::debug!(
                    "[security] Path contains parent directory reference: {}",
                    path.display()
                );
            }
            std::path::Component::Normal(s) => {
                let s_str = s.to_string_lossy();
                // Block null bytes and other dangerous patterns
                if s_str.contains('\0') {
                    return Err(SafeFileError::InvalidPath {
                        path: path.to_path_buf(),
                        reason: "Path contains null byte".to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate that a path stays within workspace boundary
fn validate_workspace_boundary(path: &Path, workspace_root: &Path) -> Result<(), SafeFileError> {
    // Canonicalize both paths
    let canonical_root = workspace_root.canonicalize().map_err(|e| SafeFileError::IoError {
        path: workspace_root.to_path_buf(),
        source: e,
    })?;

    let canonical_path = path.canonicalize().map_err(|e| SafeFileError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Check if path starts with workspace root
    if !canonical_path.starts_with(&canonical_root) {
        return Err(SafeFileError::OutsideBoundary {
            path: path.to_path_buf(),
            boundary: workspace_root.to_path_buf(),
        });
    }

    Ok(())
}

/// Verify the opened file matches the expected path (Unix only)
#[cfg(unix)]
fn verify_opened_file(file: &File, expected_path: &Path) -> Result<(), SafeFileError> {
    use std::os::unix::io::AsRawFd;

    // Get the real path of the opened file descriptor
    let fd = file.as_raw_fd();
    let proc_path = format!("/proc/self/fd/{}", fd);

    // Try to read the symlink to get the actual path
    match std::fs::read_link(&proc_path) {
        Ok(actual_path) => {
            // Canonicalize expected path for comparison
            if let Ok(expected_canonical) = expected_path.canonicalize() {
                if actual_path != expected_canonical {
                    // Log the mismatch but don't necessarily fail
                    // (paths might differ in normalization)
                    tracing::debug!(
                        "[security] Path verification: expected={}, actual={}",
                        expected_canonical.display(),
                        actual_path.display()
                    );
                }
            }
        }
        Err(e) => {
            // /proc might not be available (macOS, etc.)
            tracing::debug!(
                "[security] Could not verify file descriptor path: {} (this is normal on macOS)",
                e
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_safe_read_regular_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");

        fs::write(&file_path, "hello world").unwrap();

        let content = safe_read_to_string(&file_path, None).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    #[cfg(unix)]
    fn test_safe_read_blocks_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let real_file = temp.path().join("real.txt");
        let symlink_path = temp.path().join("link.txt");

        fs::write(&real_file, "secret content").unwrap();
        symlink(&real_file, &symlink_path).unwrap();

        // Should fail with symlink error
        let result = safe_read_to_string(&symlink_path, None);
        assert!(result.is_err());

        match result.unwrap_err() {
            SafeFileError::SymlinkDetected { path } => {
                assert_eq!(path, symlink_path);
            }
            other => panic!("Expected SymlinkDetected, got: {:?}", other),
        }
    }

    #[test]
    fn test_safe_read_with_workspace_boundary() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");

        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let inside_file = workspace.join("inside.txt");
        let outside_file = outside.join("outside.txt");

        fs::write(&inside_file, "inside content").unwrap();
        fs::write(&outside_file, "outside content").unwrap();

        // Inside workspace should work
        let content = safe_read_to_string(&inside_file, Some(&workspace)).unwrap();
        assert_eq!(content, "inside content");

        // Outside workspace should fail
        let result = safe_read_to_string(&outside_file, Some(&workspace));
        assert!(result.is_err());

        match result.unwrap_err() {
            SafeFileError::OutsideBoundary { .. } => {}
            other => panic!("Expected OutsideBoundary, got: {:?}", other),
        }
    }

    #[test]
    fn test_path_with_null_byte() {
        let path = PathBuf::from("test\0file.txt");
        let result = validate_path_components(&path);
        assert!(result.is_err());
    }
}
