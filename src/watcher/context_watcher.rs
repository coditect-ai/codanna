//! Context Watcher for Claude Code Sessions
//!
//! Monitors Claude Code session JSONL files for context usage and triggers
//! auto-export when the context threshold is reached.
//!
//! # Architecture
//!
//! ```text
//! ContextWatcher
//!   - Watches ~/.claude/projects/<project>/*.jsonl
//!   - Parses token usage from JSONL entries
//!   - Calculates context percentage
//!   - Triggers export at threshold (default: 75%)
//!   - Sends desktop notifications
//!   - Opens exported file in editor
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use codanna::watcher::context_watcher::{ContextWatcher, ContextConfig};
//!
//! let config = ContextConfig::default();
//! let watcher = ContextWatcher::new(config).await?;
//! watcher.run().await?;
//! ```
//!
//! # Configuration
//!
//! - `min_context_percent`: Trigger threshold (default: 75%)
//! - `max_context_percent`: Upper bound (default: 95%)
//! - `context_limit_tokens`: Total context window (default: 200,000)
//! - `cooldown_minutes`: Time between exports (default: 10)
//!
//! # CODI2 Heritage
//!
//! This module is inspired by CODI2's file_monitor.rs and export_handler.rs.
//! See `codi_fork/` for reference implementations.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Configuration for context watching
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Minimum context percentage to trigger export
    pub min_context_percent: u8,
    /// Maximum context percentage (don't trigger above this - too late)
    pub max_context_percent: u8,
    /// Total context window in tokens
    pub context_limit_tokens: u64,
    /// Cooldown between exports in minutes
    pub cooldown_minutes: u32,
    /// Path to Claude projects directory
    pub claude_projects_dir: PathBuf,
    /// Path to export destination
    pub export_destination: PathBuf,
    /// State file for persistence
    pub state_file: PathBuf,
    /// Whether to send desktop notifications
    pub notifications_enabled: bool,
    /// Editor command to open exports
    pub editor_command: Option<String>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        Self {
            min_context_percent: 75,
            max_context_percent: 95,
            context_limit_tokens: 200_000,
            cooldown_minutes: 10,
            claude_projects_dir: home.join(".claude/projects"),
            export_destination: home.join(".coditect/context-storage/exports-pending"),
            state_file: home.join(".coditect/context-storage/context-watcher-state.json"),
            notifications_enabled: true,
            editor_command: Some("code".to_string()),
        }
    }
}

/// Token usage from a Claude session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub cache_read: u64,
    pub cache_creation: u64,
    pub input: u64,
    pub output: u64,
}

impl TokenUsage {
    /// Total tokens used
    pub fn total(&self) -> u64 {
        self.cache_read + self.cache_creation + self.input + self.output
    }
}

/// Persistent state for the context watcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherState {
    /// Per-session cooldown tracking (session_id -> last export time)
    #[serde(default)]
    pub session_cooldowns: HashMap<String, DateTime<Utc>>,
    /// Legacy: last export (for backward compatibility)
    pub last_export: Option<DateTime<Utc>>,
    pub last_session_file: Option<PathBuf>,
    pub last_tokens: u64,
    pub last_context_percent: f64,
    pub exports_triggered: u32,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self {
            session_cooldowns: HashMap::new(),
            last_export: None,
            last_session_file: None,
            last_tokens: 0,
            last_context_percent: 0.0,
            exports_triggered: 0,
        }
    }
}

/// Context watcher for Claude Code sessions
pub struct ContextWatcher {
    config: ContextConfig,
    state: WatcherState,
    event_rx: mpsc::Receiver<notify::Result<Event>>,
    _watcher: notify::RecommendedWatcher,
}

impl ContextWatcher {
    /// Create a new context watcher
    pub fn new(config: ContextConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Create export destination if it doesn't exist
        fs::create_dir_all(&config.export_destination)?;
        fs::create_dir_all(config.state_file.parent().unwrap_or(Path::new(".")))?;

        // Load existing state
        let state = Self::load_state(&config.state_file).unwrap_or_default();

        // Create channel for events
        let (tx, rx) = mpsc::channel(100);

        // Create the notify watcher
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let _ = tx.blocking_send(res);
        })?;

        Ok(Self {
            config,
            state,
            event_rx: rx,
            _watcher: watcher,
        })
    }

    /// Load state from disk
    fn load_state(path: &Path) -> Option<WatcherState> {
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save state to disk
    fn save_state(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let content = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.config.state_file, content)?;
        Ok(())
    }

    /// Find the primary session file (largest recently modified)
    pub fn find_primary_session(&self, project_dir: &Path) -> Option<PathBuf> {
        let now = SystemTime::now();
        let sixty_minutes = Duration::from_secs(60 * 60);

        // Find JSONL files modified in last 60 minutes
        let mut candidates: Vec<(PathBuf, u64, SystemTime)> = fs::read_dir(project_dir)
            .ok()?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();

                if path.extension()?.to_str()? != "jsonl" {
                    return None;
                }

                let metadata = fs::metadata(&path).ok()?;
                let modified = metadata.modified().ok()?;

                // Only consider files modified in last 60 minutes
                if now.duration_since(modified).ok()? > sixty_minutes {
                    return None;
                }

                Some((path, metadata.len(), modified))
            })
            .collect();

        // Sort by size (largest first)
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        candidates.first().map(|(path, _, _)| path.clone())
    }

    /// Find ALL active session files (modified in last 60 minutes)
    pub fn find_all_active_sessions(&self, project_dir: &Path) -> Vec<PathBuf> {
        let now = SystemTime::now();
        let sixty_minutes = Duration::from_secs(60 * 60);

        fs::read_dir(project_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|entry| {
                        let entry = entry.ok()?;
                        let path = entry.path();

                        if path.extension()?.to_str()? != "jsonl" {
                            return None;
                        }

                        let metadata = fs::metadata(&path).ok()?;
                        let modified = metadata.modified().ok()?;

                        // Only consider files modified in last 60 minutes
                        if now.duration_since(modified).ok()? > sixty_minutes {
                            return None;
                        }

                        Some(path)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Extract session ID from path (filename without extension)
    fn session_id_from_path(path: &Path) -> String {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    /// Check if a specific session is in cooldown
    fn is_session_in_cooldown(&self, session_id: &str) -> bool {
        if let Some(last_export) = self.state.session_cooldowns.get(session_id) {
            let cooldown = chrono::Duration::minutes(self.config.cooldown_minutes as i64);
            let now = Utc::now();
            now - *last_export < cooldown
        } else {
            false
        }
    }

    /// Parse token usage from a session JSONL file
    ///
    /// Reads the last ~100KB of the file and finds the most recent usage entry.
    /// This matches the Python implementation behavior - we want the LATEST
    /// context usage, not cumulative tokens across the entire session.
    pub fn parse_session_tokens(&self, path: &Path) -> Result<TokenUsage, Box<dyn std::error::Error + Send + Sync>> {
        let mut file = File::open(path)?;

        // Get file size
        let file_size = file.metadata()?.len();

        // Read last 100KB (or entire file if smaller)
        const READ_SIZE: u64 = 100_000;
        let read_start = file_size.saturating_sub(READ_SIZE);
        file.seek(SeekFrom::Start(read_start))?;

        // Read as bytes and convert with lossy UTF-8 (like Python's errors='ignore')
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        let content = String::from_utf8_lossy(&buffer);

        // Split into lines and process from END (most recent first)
        let lines: Vec<&str> = content.lines().collect();

        for line in lines.iter().rev() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with('{') {
                continue;
            }

            // Parse JSONL line
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                // Check for message.usage pattern (most common in Claude Code)
                if let Some(message) = entry.get("message") {
                    if let Some(usage) = message.get("usage") {
                        if let Some(token_usage) = Self::extract_usage(usage) {
                            return Ok(token_usage);
                        }
                    }
                }

                // Also check for direct usage block
                if let Some(usage) = entry.get("usage") {
                    if let Some(token_usage) = Self::extract_usage(usage) {
                        return Ok(token_usage);
                    }
                }
            }
        }

        // No usage found - return empty
        Ok(TokenUsage::default())
    }

    /// Extract TokenUsage from a usage JSON object
    fn extract_usage(usage: &serde_json::Value) -> Option<TokenUsage> {
        // Check if this looks like a valid usage object
        if !usage.is_object() {
            return None;
        }

        let cache_read = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_creation = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

        // Only return if we found at least some token data
        if cache_read > 0 || cache_creation > 0 || input > 0 || output > 0 {
            Some(TokenUsage {
                cache_read,
                cache_creation,
                input,
                output,
            })
        } else {
            None
        }
    }

    /// Calculate context percentage
    pub fn calculate_context_percent(&self, usage: &TokenUsage) -> f64 {
        let total = usage.total() as f64;
        let limit = self.config.context_limit_tokens as f64;
        (total / limit) * 100.0
    }

    /// Send desktop notification (macOS)
    fn notify(&self, title: &str, message: &str) {
        if !self.config.notifications_enabled {
            return;
        }

        #[cfg(target_os = "macos")]
        {
            let script = format!(
                r#"display notification "{}" with title "{}" sound name "Glass""#,
                message.replace('"', r#"\""#),
                title.replace('"', r#"\""#)
            );
            let _ = Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output();
        }

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("notify-send")
                .arg(title)
                .arg(message)
                .output();
        }
    }

    /// Open file in editor
    fn open_in_editor(&self, path: &Path) {
        if let Some(ref editor) = self.config.editor_command {
            let _ = Command::new(editor)
                .arg(path)
                .spawn();
        }
    }

    /// Trigger export for a session
    pub fn trigger_export(&mut self, session_path: &Path, context_pct: f64) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        let session_id = Self::session_id_from_path(session_path);
        let timestamp = Utc::now().format("%Y-%m-%d-%H%M%S").to_string();
        // Include session ID prefix (first 8 chars) in filename for clarity
        let session_prefix = &session_id[..session_id.len().min(8)];
        let filename = format!("{}-{}-CONTEXT-{:.0}pct-EXPORT.txt", timestamp, session_prefix, context_pct);
        let export_path = self.config.export_destination.join(&filename);

        // Copy session file to export destination
        fs::copy(session_path, &export_path)?;

        // Update state with per-session cooldown
        let now = Utc::now();
        self.state.session_cooldowns.insert(session_id.clone(), now);
        self.state.last_export = Some(now);
        self.state.exports_triggered += 1;
        self.save_state()?;

        // Notify user
        self.notify(
            "CODITECT Context Export",
            &format!("Context at {:.1}%\nExported: {}\nRun /cx to process", context_pct, filename)
        );

        // Open in editor
        self.open_in_editor(&export_path);

        tracing::info!(
            "[context-watcher] exported {} at {:.1}% context",
            export_path.display(),
            context_pct
        );

        Ok(export_path)
    }

    /// Check a single session and export if needed
    fn check_single_session(&mut self, session_file: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
        let session_id = Self::session_id_from_path(session_file);

        // Parse tokens
        let usage = self.parse_session_tokens(session_file)?;
        let context_pct = self.calculate_context_percent(&usage);

        tracing::debug!(
            "[context-watcher] {} at {:.1}% ({} tokens)",
            session_id,
            context_pct,
            usage.total()
        );

        // Check if we should export (per-session cooldown)
        if context_pct >= self.config.min_context_percent as f64
            && context_pct <= self.config.max_context_percent as f64
            && !self.is_session_in_cooldown(&session_id)
        {
            tracing::info!(
                "[context-watcher] session {} at {:.1}% - triggering export",
                &session_id[..session_id.len().min(8)],
                context_pct
            );
            let export_path = self.trigger_export(session_file, context_pct)?;
            return Ok(Some(export_path));
        }

        Ok(None)
    }

    /// Check ALL active sessions and export any above threshold
    pub fn check_and_export(&mut self, project_dir: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
        // Find ALL active sessions
        let sessions = self.find_all_active_sessions(project_dir);

        if sessions.is_empty() {
            return Ok(None);
        }

        let mut last_export = None;

        // Check each session independently
        for session_file in sessions {
            // Update state with most recent session info
            if let Ok(usage) = self.parse_session_tokens(&session_file) {
                let context_pct = self.calculate_context_percent(&usage);
                self.state.last_session_file = Some(session_file.clone());
                self.state.last_tokens = usage.total();
                self.state.last_context_percent = context_pct;
            }

            // Check and potentially export this session
            match self.check_single_session(&session_file) {
                Ok(Some(path)) => {
                    last_export = Some(path);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!(
                        "[context-watcher] error checking {}: {}",
                        session_file.display(),
                        e
                    );
                }
            }
        }

        // Save state after checking all sessions
        self.save_state()?;

        Ok(last_export)
    }

    /// Run the context watcher (event-driven)
    pub async fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("[context-watcher] starting");
        tracing::info!("[context-watcher] watching: {}", self.config.claude_projects_dir.display());
        tracing::info!("[context-watcher] threshold: {}%", self.config.min_context_percent);

        // Watch the Claude projects directory
        if !self.config.claude_projects_dir.exists() {
            tracing::warn!("[context-watcher] Claude projects directory does not exist: {}",
                self.config.claude_projects_dir.display());
        }

        // Watch for changes
        // Note: We need to watch parent directory since project dirs are dynamic
        self._watcher.watch(&self.config.claude_projects_dir, RecursiveMode::Recursive)?;

        loop {
            // Wait for events with timeout for periodic checks
            let timeout = tokio::time::sleep(Duration::from_secs(10));
            tokio::pin!(timeout);

            tokio::select! {
                Some(res) = self.event_rx.recv() => {
                    match res {
                        Ok(event) => {
                            // Only process modify events on JSONL files
                            if matches!(event.kind, EventKind::Modify(_)) {
                                for path in &event.paths {
                                    if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                                        if let Some(project_dir) = path.parent() {
                                            if let Err(e) = self.check_and_export(project_dir) {
                                                tracing::error!("[context-watcher] check error: {e}");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("[context-watcher] watch error: {e}");
                        }
                    }
                }
                // Periodic check (fallback if events are missed)
                _ = &mut timeout => {
                    // Check all project directories
                    if let Ok(entries) = fs::read_dir(&self.config.claude_projects_dir) {
                        for entry in entries.filter_map(|e| e.ok()) {
                            let path = entry.path();
                            if path.is_dir() {
                                if let Err(e) = self.check_and_export(&path) {
                                    tracing::debug!("[context-watcher] periodic check error: {e}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Get current state
    pub fn state(&self) -> &WatcherState {
        &self.state
    }

    /// Get current config
    pub fn config(&self) -> &ContextConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_token_usage() {
        let usage = TokenUsage {
            cache_read: 1000,
            cache_creation: 500,
            input: 2000,
            output: 1500,
        };
        assert_eq!(usage.total(), 5000);
    }

    #[test]
    fn test_context_percent() {
        let config = ContextConfig {
            context_limit_tokens: 200_000,
            ..Default::default()
        };
        let watcher = ContextWatcher::new(config).unwrap();

        let usage = TokenUsage {
            cache_read: 100_000,
            cache_creation: 0,
            input: 50_000,
            output: 0,
        };

        let percent = watcher.calculate_context_percent(&usage);
        assert!((percent - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_state_serialization() {
        let state = WatcherState {
            last_export: Some(Utc::now()),
            last_session_file: Some(PathBuf::from("/test/session.jsonl")),
            last_tokens: 150_000,
            last_context_percent: 75.0,
            exports_triggered: 5,
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: WatcherState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.last_tokens, 150_000);
        assert_eq!(restored.exports_triggered, 5);
    }
}
