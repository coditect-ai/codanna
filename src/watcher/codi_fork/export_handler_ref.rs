//! Export Handler Reference (from CODI2)
//!
//! This file documents the key patterns from CODI2's export_handler.rs
//! for reference when implementing context-triggered exports.
//!
//! Original: coditect-labs-v4-archive/codi2/src/monitor/export_handler.rs (276 lines)
//!
//! # Key Patterns
//!
//! ## 1. Export Detection Pattern
//! ```rust,ignore
//! // Pattern: YYYY-MM-DD*EXPORT*.txt
//! pattern: Regex::new(r"^\d{4}-\d{2}-\d{2}.*EXPORT.*\.txt$")
//! ```
//!
//! ## 2. Content Analysis
//! Reads first 50 lines or 5KB to determine content type:
//! - "adr-review-session" if contains "adr"
//! - "agent-session" if contains session IDs
//! - "implementation-session" if contains "implement"
//! - etc.
//!
//! ## 3. Filename Generation
//! Format: `{timestamp}-{micros}-{content_hint}.txt`
//! Example: `20250910T143022-123456-adr-review-session.txt`
//!
//! ## 4. Processing Flow
//! 1. Detect export file by pattern
//! 2. Analyze content for naming
//! 3. Generate unique filename
//! 4. Move to destination directory
//! 5. Log to audit trail

use std::path::PathBuf;
use regex::Regex;

/// Export configuration (based on CODI2)
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Directory to watch for exports
    pub watch_dir: PathBuf,
    /// Directory to move exports to
    pub destination_dir: PathBuf,
    /// Pattern to match export files
    pub pattern: Regex,
    /// Whether to analyze content for naming
    pub analyze_content: bool,
}

impl Default for ExportConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let watch_dir = home.join(".coditect/context-storage/exports-pending");
        let destination_dir = home.join(".coditect/context-storage/exports-archive");

        Self {
            watch_dir,
            destination_dir,
            // Pattern: YYYY-MM-DD*EXPORT*.txt
            pattern: Regex::new(r"^\d{4}-\d{2}-\d{2}.*\.txt$").unwrap(),
            analyze_content: true,
        }
    }
}

/// Content categories for exports (from CODI2)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportCategory {
    AdrReview,
    AgentSession,
    Implementation,
    Debugging,
    Testing,
    CoditectDevelopment,
    Conversation,
}

impl ExportCategory {
    /// Detect category from content
    pub fn from_content(content: &str) -> Self {
        let lower = content.to_lowercase();

        if lower.contains("adr") {
            Self::AdrReview
        } else if lower.contains("-session") {
            Self::AgentSession
        } else if lower.contains("implement") {
            Self::Implementation
        } else if lower.contains("debug") {
            Self::Debugging
        } else if lower.contains("test") {
            Self::Testing
        } else if lower.contains("coditect") {
            Self::CoditectDevelopment
        } else {
            Self::Conversation
        }
    }

    /// Get filename suffix for this category
    pub fn suffix(&self) -> &'static str {
        match self {
            Self::AdrReview => "adr-review-session",
            Self::AgentSession => "agent-session",
            Self::Implementation => "implementation-session",
            Self::Debugging => "debugging-session",
            Self::Testing => "testing-session",
            Self::CoditectDevelopment => "coditect-development",
            Self::Conversation => "conversation",
        }
    }
}
