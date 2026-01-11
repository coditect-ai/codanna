//! Security module for codanna
//!
//! Provides secure file operations to prevent TOCTOU (time-of-check to time-of-use)
//! vulnerabilities and symlink attacks.
//!
//! # Security Features
//!
//! - **O_NOFOLLOW**: Prevents following symlinks during file operations
//! - **Path Canonicalization**: Validates paths stay within workspace boundaries
//! - **Workspace Boundary Enforcement**: Rejects paths that escape the workspace
//!
//! # CODITECT Integration
//!
//! This module was added as part of ADR-065 (Codanna Code Intelligence Integration)
//! to address P1 security requirement: "Fix symlink race condition (O_NOFOLLOW, path validation)"

mod safe_file;
mod workspace_boundary;

pub use safe_file::{safe_read_to_string, safe_open, SafeFileError};
pub use workspace_boundary::{validate_path_boundary, WorkspaceBoundary, BoundaryError};
