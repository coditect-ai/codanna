//! Context Handler for Claude Code Sessions
//!
//! Handles file events for Claude Code session JSONL files,
//! integrating with the UnifiedWatcher system.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::super::error::WatchError;
use super::super::handler::{WatchAction, WatchHandler};
use super::super::context_watcher::{ContextConfig, TokenUsage};

/// Handler for Claude Code session files
pub struct ContextHandler {
    /// Configuration
    config: ContextConfig,
    /// Tracked session files
    tracked_paths: Arc<RwLock<Vec<PathBuf>>>,
    /// Last known token counts per session
    token_cache: Arc<RwLock<std::collections::HashMap<PathBuf, u64>>>,
}

impl ContextHandler {
    /// Create a new context handler
    pub fn new(config: ContextConfig) -> Self {
        Self {
            config,
            tracked_paths: Arc::new(RwLock::new(Vec::new())),
            token_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Parse token usage from a session file
    fn parse_tokens(&self, path: &Path) -> Option<TokenUsage> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let file = File::open(path).ok()?;
        let reader = BufReader::new(file);

        let mut usage = TokenUsage::default();

        for line in reader.lines().filter_map(|l| l.ok()) {
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) {
                // Check for usage in message or top level
                let usage_val = entry.get("usage")
                    .or_else(|| entry.get("message").and_then(|m| m.get("usage")));

                if let Some(u) = usage_val {
                    if let Some(v) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                        usage.cache_read += v;
                    }
                    if let Some(v) = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()) {
                        usage.cache_creation += v;
                    }
                    if let Some(v) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                        usage.input += v;
                    }
                    if let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                        usage.output += v;
                    }
                }
            }
        }

        Some(usage)
    }

    /// Calculate context percentage
    fn context_percent(&self, usage: &TokenUsage) -> f64 {
        let total = usage.total() as f64;
        let limit = self.config.context_limit_tokens as f64;
        (total / limit) * 100.0
    }
}

#[async_trait]
impl WatchHandler for ContextHandler {
    fn name(&self) -> &str {
        "context"
    }

    fn matches(&self, path: &Path) -> bool {
        // Match JSONL files in Claude projects directory
        if let Some(ext) = path.extension() {
            if ext == "jsonl" {
                // Check if it's in the Claude projects directory
                if let Some(parent) = path.parent() {
                    let claude_dir = &self.config.claude_projects_dir;
                    return parent.starts_with(claude_dir) ||
                           path.to_string_lossy().contains("/.claude/projects/");
                }
            }
        }
        false
    }

    async fn on_modify(&self, path: &Path) -> Result<WatchAction, WatchError> {
        // Parse tokens and check threshold
        if let Some(usage) = self.parse_tokens(path) {
            let percent = self.context_percent(&usage);
            let total = usage.total();

            // Update cache
            {
                let mut cache = self.token_cache.write();
                cache.insert(path.to_path_buf(), total);
            }

            tracing::debug!(
                "[context] {} at {:.1}% ({} tokens)",
                path.display(),
                percent,
                total
            );

            // Check if we should trigger export
            if percent >= self.config.min_context_percent as f64
                && percent <= self.config.max_context_percent as f64
            {
                tracing::info!(
                    "[context] threshold reached: {:.1}% - export recommended",
                    percent
                );

                // Return a custom action that the watcher can handle
                // For now, just log - actual export is handled by ContextWatcher
            }
        }

        Ok(WatchAction::None)
    }

    async fn on_delete(&self, path: &Path) -> Result<WatchAction, WatchError> {
        // Remove from cache
        {
            let mut cache = self.token_cache.write();
            cache.remove(path);
        }

        tracing::debug!("[context] session deleted: {}", path.display());
        Ok(WatchAction::None)
    }

    async fn refresh_paths(&self) -> Result<(), WatchError> {
        let mut paths = self.tracked_paths.write();
        paths.clear();

        // Scan Claude projects directory
        if self.config.claude_projects_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.config.claude_projects_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        // Scan for JSONL files
                        if let Ok(files) = std::fs::read_dir(&path) {
                            for file in files.filter_map(|f| f.ok()) {
                                let file_path = file.path();
                                if file_path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                                    paths.push(file_path);
                                }
                            }
                        }
                    }
                }
            }
        }

        tracing::debug!("[context] tracking {} session files", paths.len());
        Ok(())
    }

    async fn tracked_paths(&self) -> Vec<PathBuf> {
        self.tracked_paths.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_jsonl() {
        let config = ContextConfig::default();
        let handler = ContextHandler::new(config);

        // Should match JSONL in Claude projects
        let path = dirs::home_dir().unwrap()
            .join(".claude/projects/test-project/session.jsonl");
        assert!(handler.matches(&path));

        // Should not match non-JSONL
        let path = dirs::home_dir().unwrap()
            .join(".claude/projects/test-project/session.txt");
        assert!(!handler.matches(&path));
    }
}
