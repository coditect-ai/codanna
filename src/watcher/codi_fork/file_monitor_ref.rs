//! File Monitor Reference (from CODI2)
//!
//! This file documents the key patterns from CODI2's file_monitor.rs
//! for reference when implementing context watching.
//!
//! Original: coditect-labs-v4-archive/codi2/src/monitor/file_monitor.rs (1131 lines)
//!
//! # Key Patterns
//!
//! ## 1. File Event Types
//! ```rust,ignore
//! pub enum FileOperation {
//!     Created,
//!     Modified,
//!     Deleted,
//!     Renamed { from: PathBuf, to: PathBuf, confidence: Option<f32> },
//!     PermissionChanged { old_mode: u32, new_mode: u32 },
//! }
//! ```
//!
//! ## 2. Actor Attribution
//! ```rust,ignore
//! pub enum Actor {
//!     Human(String),
//!     AI { tool: String, session: String },
//!     System,
//! }
//! ```
//!
//! ## 3. Event Processing Loop
//! ```rust,ignore
//! loop {
//!     match receiver.recv() {
//!         Ok(Ok(event)) => {
//!             if should_process(&event) {
//!                 process_event(event).await;
//!             }
//!         }
//!         // error handling...
//!     }
//! }
//! ```
//!
//! ## 4. Move Correlation
//! Detects file moves by correlating delete events with subsequent create events
//! using file identity (size, mtime, hash).
//!
//! ## 5. Export Detection
//! Special handling for Claude Code export files matching pattern:
//! `^\d{4}-\d{2}-\d{2}.*EXPORT.*\.txt$`

use std::path::PathBuf;

/// Actor who performed a file operation (from CODI2)
#[derive(Debug, Clone)]
pub enum Actor {
    /// Human developer
    Human(String),
    /// AI assistant (Claude, etc.)
    AI { tool: String, session: String },
    /// System/automated process
    System,
}

/// File operation types (from CODI2)
#[derive(Debug, Clone)]
pub enum FileOperation {
    /// File or directory was created
    Created,
    /// File content was modified
    Modified,
    /// File or directory was deleted
    Deleted,
    /// File was renamed or moved
    Renamed {
        from: PathBuf,
        to: PathBuf,
        confidence: Option<f32>,
    },
}

/// File event with attribution (from CODI2)
#[derive(Debug, Clone)]
pub struct FileEvent {
    /// Path affected by the operation
    pub path: PathBuf,
    /// Type of file operation
    pub operation: FileOperation,
    /// Actor who performed the operation
    pub actor: Actor,
    /// When the event occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
