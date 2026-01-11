//! Workspace boundary enforcement
//!
//! Ensures all file operations stay within the designated workspace root.
//! This prevents path traversal attacks and accidental access to files
//! outside the project.

use std::path::{Path, PathBuf};

/// Errors related to workspace boundary violations
#[derive(Debug, Clone)]
pub enum BoundaryError {
    /// Path escapes the workspace boundary
    EscapeAttempt {
        path: PathBuf,
        workspace: PathBuf,
        reason: String,
    },
    /// Path could not be validated
    ValidationFailed {
        path: PathBuf,
        reason: String,
    },
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EscapeAttempt { path, workspace, reason } => {
                write!(
                    f,
                    "Path {} attempts to escape workspace {}: {}",
                    path.display(),
                    workspace.display(),
                    reason
                )
            }
            Self::ValidationFailed { path, reason } => {
                write!(f, "Path validation failed for {}: {}", path.display(), reason)
            }
        }
    }
}

impl std::error::Error for BoundaryError {}

/// Workspace boundary configuration and enforcement
#[derive(Debug, Clone)]
pub struct WorkspaceBoundary {
    /// The canonical root path of the workspace
    root: PathBuf,
    /// Whether to allow symlinks within the workspace
    allow_internal_symlinks: bool,
}

impl WorkspaceBoundary {
    /// Create a new workspace boundary
    ///
    /// # Arguments
    ///
    /// * `root` - The root directory of the workspace
    ///
    /// # Errors
    ///
    /// Returns error if the root path cannot be canonicalized
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Self, BoundaryError> {
        let root = root.as_ref();
        let canonical = root.canonicalize().map_err(|e| BoundaryError::ValidationFailed {
            path: root.to_path_buf(),
            reason: format!("Cannot canonicalize workspace root: {}", e),
        })?;

        Ok(Self {
            root: canonical,
            allow_internal_symlinks: false,
        })
    }

    /// Allow symlinks that stay within the workspace
    #[must_use]
    pub fn with_internal_symlinks(mut self, allow: bool) -> Self {
        self.allow_internal_symlinks = allow;
        self
    }

    /// Get the workspace root
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Check if a path is within the workspace boundary
    ///
    /// # Arguments
    ///
    /// * `path` - The path to validate
    ///
    /// # Returns
    ///
    /// `Ok(canonical_path)` if the path is within bounds, `Err` otherwise
    pub fn validate<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf, BoundaryError> {
        let path = path.as_ref();

        // First, try to canonicalize
        let canonical = path.canonicalize().map_err(|e| {
            BoundaryError::ValidationFailed {
                path: path.to_path_buf(),
                reason: format!("Cannot canonicalize path: {}", e),
            }
        })?;

        // Check if it starts with our root
        if !canonical.starts_with(&self.root) {
            return Err(BoundaryError::EscapeAttempt {
                path: path.to_path_buf(),
                workspace: self.root.clone(),
                reason: "Path resolves outside workspace".to_string(),
            });
        }

        // If symlinks not allowed, check the original path for symlink components
        if !self.allow_internal_symlinks {
            self.check_no_symlinks(path)?;
        }

        Ok(canonical)
    }

    /// Check if a relative path would escape the workspace
    ///
    /// This is useful for validating user-provided paths before any file operations.
    pub fn validate_relative<P: AsRef<Path>>(&self, relative_path: P) -> Result<PathBuf, BoundaryError> {
        let relative = relative_path.as_ref();
        let full_path = self.root.join(relative);
        self.validate(&full_path)
    }

    /// Check that path doesn't contain symlinks (unless allowed)
    #[cfg(unix)]
    fn check_no_symlinks(&self, path: &Path) -> Result<(), BoundaryError> {
        use std::fs;

        let mut current = PathBuf::new();

        for component in path.components() {
            current.push(component);

            if current.exists() {
                let metadata = fs::symlink_metadata(&current).map_err(|e| {
                    BoundaryError::ValidationFailed {
                        path: current.clone(),
                        reason: format!("Cannot read metadata: {}", e),
                    }
                })?;

                if metadata.file_type().is_symlink() {
                    // Check if symlink target is within workspace
                    let target = fs::read_link(&current).map_err(|e| {
                        BoundaryError::ValidationFailed {
                            path: current.clone(),
                            reason: format!("Cannot read symlink target: {}", e),
                        }
                    })?;

                    let resolved = if target.is_absolute() {
                        target.clone()
                    } else {
                        current.parent().unwrap_or(Path::new("/")).join(&target)
                    };

                    if let Ok(canonical_target) = resolved.canonicalize() {
                        if !canonical_target.starts_with(&self.root) {
                            return Err(BoundaryError::EscapeAttempt {
                                path: path.to_path_buf(),
                                workspace: self.root.clone(),
                                reason: format!(
                                    "Symlink {} points outside workspace to {}",
                                    current.display(),
                                    canonical_target.display()
                                ),
                            });
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[cfg(not(unix))]
    fn check_no_symlinks(&self, _path: &Path) -> Result<(), BoundaryError> {
        // On non-Unix, we rely on canonicalization
        Ok(())
    }
}

/// Convenience function to validate a path against a workspace root
///
/// # Example
///
/// ```rust,ignore
/// use codanna::security::validate_path_boundary;
///
/// let workspace = "/home/user/project";
/// let file = "/home/user/project/src/main.rs";
///
/// validate_path_boundary(file, workspace)?; // Ok
///
/// let outside = "/etc/passwd";
/// validate_path_boundary(outside, workspace)?; // Error
/// ```
pub fn validate_path_boundary<P: AsRef<Path>, R: AsRef<Path>>(
    path: P,
    workspace_root: R,
) -> Result<PathBuf, BoundaryError> {
    let boundary = WorkspaceBoundary::new(workspace_root)?;
    boundary.validate(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_valid_path_within_boundary() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        let file = workspace.join("src/main.rs");

        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(&file, "fn main() {}").unwrap();

        let boundary = WorkspaceBoundary::new(workspace).unwrap();
        let result = boundary.validate(&file);

        assert!(result.is_ok());
    }

    #[test]
    fn test_path_escape_blocked() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");

        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();

        let boundary = WorkspaceBoundary::new(&workspace).unwrap();
        let result = boundary.validate(outside.join("secret.txt"));

        assert!(result.is_err());
        match result.unwrap_err() {
            BoundaryError::EscapeAttempt { .. } => {}
            other => panic!("Expected EscapeAttempt, got: {:?}", other),
        }
    }

    #[test]
    fn test_parent_directory_escape_blocked() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");

        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();

        let boundary = WorkspaceBoundary::new(&workspace).unwrap();

        // Try to escape with ../
        let escape_path = workspace.join("../outside/secret.txt");
        let result = boundary.validate(&escape_path);

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_relative_path() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        let file = workspace.join("src/main.rs");

        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(&file, "fn main() {}").unwrap();

        let boundary = WorkspaceBoundary::new(workspace).unwrap();
        let result = boundary.validate_relative("src/main.rs");

        assert!(result.is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_symlink_escape_blocked() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");

        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();

        // Create symlink inside workspace pointing outside
        symlink(outside.join("secret.txt"), workspace.join("link.txt")).unwrap();

        let boundary = WorkspaceBoundary::new(&workspace).unwrap();
        let result = boundary.validate(workspace.join("link.txt"));

        // Should fail because symlink points outside
        assert!(result.is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_internal_symlink_allowed_when_configured() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();

        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/real.txt"), "content").unwrap();

        // Create symlink within workspace
        symlink(
            workspace.join("src/real.txt"),
            workspace.join("src/link.txt"),
        ).unwrap();

        let boundary = WorkspaceBoundary::new(workspace)
            .unwrap()
            .with_internal_symlinks(true);

        let result = boundary.validate(workspace.join("src/link.txt"));
        assert!(result.is_ok());
    }
}
