//! Context Watcher for Claude Code Sessions
//!
//! Monitors Claude Code session JSONL files for context usage and triggers
//! auto-export when the context threshold is reached. Includes automatic
//! processing of pending exports via the CxProcessor.
//!
//! # Architecture
//!
//! ```text
//! ContextWatcher
//!   - Watches ~/.claude/projects/<project>/*.jsonl
//!   - Parses token usage from JSONL entries
//!   - Calculates context percentage
//!   - Triggers export at threshold (default: 75%)
//!   - Auto-processes exports via CxProcessor
//!   - Sends desktop notifications
//!   - Opens exported file in editor
//!
//! CxProcessor (integrated)
//!   - Scans ~/.coditect/context-storage/exports-pending/
//!   - Calls unified-message-extractor.py for each file
//!   - Moves processed files to exports-archive/
//!   - Generates processing reports in cx-processing-reports/
//!   - Updates session log with processing results
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
//! - `cx_processing_interval_secs`: Auto /cx interval (default: 60)
//!
//! # CODI2 Heritage
//!
//! This module is inspired by CODI2's file_monitor.rs and export_handler.rs.
//! See `codi_fork/` for reference implementations.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

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
    /// Interval in seconds for Claude process detection
    pub process_check_interval_secs: u32,
    /// Path to Claude projects directory
    pub claude_projects_dir: PathBuf,
    /// Path to export destination
    pub export_destination: PathBuf,
    /// Path to export archive
    pub export_archive: PathBuf,
    /// State file for persistence
    pub state_file: PathBuf,
    /// Whether to send desktop notifications
    pub notifications_enabled: bool,
    /// Editor command to open exports
    pub editor_command: Option<String>,
    /// Interval in seconds for cx processing checks
    pub cx_processing_interval_secs: u64,
    /// Path to Python message extractor script
    pub python_extractor_path: PathBuf,
    /// Path to cx processing reports directory
    pub cx_reports_dir: PathBuf,
    /// Path to session logs directory
    pub session_logs_dir: PathBuf,
    /// Path to machine-id.json
    pub machine_id_path: PathBuf,
}

impl Default for ContextConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let coditect_dir = home.join(".coditect");

        Self {
            min_context_percent: 75,
            max_context_percent: 95,
            context_limit_tokens: 200_000,
            cooldown_minutes: 10,
            process_check_interval_secs: 30,
            claude_projects_dir: home.join(".claude/projects"),
            export_destination: coditect_dir.join("context-storage/exports-pending"),
            export_archive: coditect_dir.join("context-storage/exports-archive"),
            state_file: coditect_dir.join("context-storage/context-watcher-state.json"),
            notifications_enabled: true,
            editor_command: Some("code".to_string()),
            cx_processing_interval_secs: 60,
            python_extractor_path: coditect_dir.join("scripts/unified-message-extractor.py"),
            cx_reports_dir: coditect_dir.join("context-storage/cx-processing-reports"),
            session_logs_dir: coditect_dir.join("session-logs"),
            machine_id_path: coditect_dir.join("machine-id.json"),
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

/// Result of processing a single export file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CxFileResult {
    /// Filename that was processed
    pub filename: String,
    /// Number of new messages extracted
    pub messages_new: u64,
    /// Number of duplicate messages filtered
    pub messages_duplicate: u64,
    /// Whether processing succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
}

/// Cumulative result of cx processing run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CxProcessingReport {
    /// ISO 8601 timestamp of processing run
    pub timestamp: String,
    /// Unique run identifier
    pub run_id: String,
    /// Number of files processed
    pub files_processed: u32,
    /// Total new messages extracted
    pub messages_new: u64,
    /// Total duplicate messages filtered
    pub messages_duplicate: u64,
    /// Number of errors encountered
    pub errors: u32,
    /// Processing duration in milliseconds
    pub duration_ms: u64,
    /// Per-file results
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub file_results: Vec<CxFileResult>,
}

/// Information about a running Claude process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeProcess {
    /// Process ID
    pub pid: u32,
    /// Working directory of the process
    pub cwd: PathBuf,
    /// Mapped session folder in ~/.claude/projects/
    pub session_folder: Option<PathBuf>,
}

impl ClaudeProcess {
    /// Convert a working directory path to Claude's session folder format
    /// e.g., /Users/hal/PROJECTS/foo → ~/.claude/projects/-Users-hal-PROJECTS-foo/
    pub fn cwd_to_session_folder(cwd: &Path, projects_dir: &Path) -> Option<PathBuf> {
        let cwd_str = cwd.to_string_lossy();
        // Replace / with - and prepend -
        let folder_name = format!("-{}", cwd_str.replace('/', "-").trim_start_matches('-'));
        let session_folder = projects_dir.join(&folder_name);

        if session_folder.exists() {
            Some(session_folder)
        } else {
            // Try partial match for nested paths
            if let Ok(entries) = fs::read_dir(projects_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.contains(&folder_name.trim_start_matches('-')) {
                        return Some(entry.path());
                    }
                }
            }
            None
        }
    }
}

/// Process detector for finding running Claude instances
pub struct ProcessDetector;

impl ProcessDetector {
    /// Find all running Claude processes with their working directories
    #[cfg(target_os = "macos")]
    pub fn find_claude_processes(projects_dir: &Path) -> Vec<ClaudeProcess> {
        let mut processes = Vec::new();

        // Get Claude process PIDs using pgrep
        let pgrep_output = Command::new("pgrep")
            .arg("-x")
            .arg("claude")
            .output();

        let pids: Vec<u32> = match pgrep_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect()
            }
            _ => return processes,
        };

        if pids.is_empty() {
            return processes;
        }

        // Get working directories using lsof
        let pid_args: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
        let lsof_output = Command::new("lsof")
            .arg("-p")
            .arg(pid_args.join(","))
            .output();

        if let Ok(output) = lsof_output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                // Parse lsof output: claude PID user cwd DIR ... path
                if line.contains("cwd") && line.starts_with("claude") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 9 {
                        if let Ok(pid) = parts[1].parse::<u32>() {
                            // Last part is the path
                            let cwd = PathBuf::from(parts.last().unwrap_or(&""));
                            if cwd.exists() {
                                let session_folder = ClaudeProcess::cwd_to_session_folder(&cwd, projects_dir);
                                processes.push(ClaudeProcess {
                                    pid,
                                    cwd,
                                    session_folder,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Deduplicate by PID
        processes.sort_by_key(|p| p.pid);
        processes.dedup_by_key(|p| p.pid);

        tracing::debug!(
            "[context-watcher] found {} Claude process(es): {:?}",
            processes.len(),
            processes.iter().map(|p| p.pid).collect::<Vec<_>>()
        );

        processes
    }

    #[cfg(target_os = "linux")]
    pub fn find_claude_processes(projects_dir: &Path) -> Vec<ClaudeProcess> {
        let mut processes = Vec::new();

        // Get Claude process PIDs using pgrep
        let pgrep_output = Command::new("pgrep")
            .arg("-x")
            .arg("claude")
            .output();

        let pids: Vec<u32> = match pgrep_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect()
            }
            _ => return processes,
        };

        // Get working directories from /proc
        for pid in pids {
            let cwd_link = PathBuf::from(format!("/proc/{}/cwd", pid));
            if let Ok(cwd) = fs::read_link(&cwd_link) {
                let session_folder = ClaudeProcess::cwd_to_session_folder(&cwd, projects_dir);
                processes.push(ClaudeProcess {
                    pid,
                    cwd,
                    session_folder,
                });
            }
        }

        tracing::debug!(
            "[context-watcher] found {} Claude process(es)",
            processes.len()
        );

        processes
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn find_claude_processes(_projects_dir: &Path) -> Vec<ClaudeProcess> {
        // Windows and other platforms: not yet implemented
        Vec::new()
    }

    /// Check if any Claude process is using a specific session folder
    pub fn is_session_active(session_folder: &Path, processes: &[ClaudeProcess]) -> bool {
        processes.iter().any(|p| {
            p.session_folder.as_ref().map_or(false, |sf| sf == session_folder)
        })
    }

    /// Get the session folders that have active Claude processes
    pub fn get_active_session_folders(processes: &[ClaudeProcess]) -> Vec<PathBuf> {
        processes
            .iter()
            .filter_map(|p| p.session_folder.clone())
            .collect()
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
    /// Last cx processing run
    #[serde(default)]
    pub last_cx_processing: Option<DateTime<Utc>>,
    /// Total cx processing runs
    #[serde(default)]
    pub cx_runs_total: u32,
    /// Currently detected running Claude processes
    #[serde(default)]
    pub active_processes: Vec<ClaudeProcess>,
    /// Count of active Claude processes (for quick access)
    #[serde(default)]
    pub active_process_count: u32,
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
            last_cx_processing: None,
            cx_runs_total: 0,
            active_processes: Vec::new(),
            active_process_count: 0,
        }
    }
}

/// Context watcher for Claude Code sessions
pub struct ContextWatcher {
    config: ContextConfig,
    state: WatcherState,
    event_rx: mpsc::Receiver<notify::Result<Event>>,
    _watcher: notify::RecommendedWatcher,
    /// Last time we checked for pending exports
    last_cx_check: Instant,
    /// Cached machine ID for session log entries
    machine_id: Option<String>,
    /// Last time we checked for Claude processes
    last_process_check: Instant,
    /// Interval between process checks (30 seconds)
    process_check_interval: Duration,
}

impl ContextWatcher {
    /// Create a new context watcher
    pub fn new(config: ContextConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Create export destination if it doesn't exist
        fs::create_dir_all(&config.export_destination)?;
        fs::create_dir_all(&config.export_archive)?;
        fs::create_dir_all(&config.cx_reports_dir)?;
        fs::create_dir_all(config.state_file.parent().unwrap_or(Path::new(".")))?;

        // Load existing state
        let state = Self::load_state(&config.state_file).unwrap_or_default();

        // Load machine ID for session log entries
        let machine_id = Self::load_machine_id(&config.machine_id_path);

        // Extract process check interval before moving config
        let process_check_interval = Duration::from_secs(config.process_check_interval_secs as u64);

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
            last_cx_check: Instant::now(),
            machine_id,
            last_process_check: Instant::now(),
            process_check_interval,
        })
    }

    /// Load machine ID from machine-id.json
    fn load_machine_id(path: &Path) -> Option<String> {
        let content = fs::read_to_string(path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;
        json.get("machine_uuid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
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
        // Use .jsonl extension for consistency with unified message format
        let filename = format!("{}-{}-CONTEXT-{:.0}pct-EXPORT.jsonl", timestamp, session_prefix, context_pct);
        let export_path = self.config.export_destination.join(&filename);

        // Copy session file to export destination
        fs::copy(session_path, &export_path)?;

        // Update state with per-session cooldown
        let now = Utc::now();
        self.state.session_cooldowns.insert(session_id.clone(), now);
        self.state.last_export = Some(now);
        self.state.exports_triggered += 1;
        self.save_state()?;

        // Notify user - indicate auto-processing is enabled
        self.notify(
            "CODITECT Auto-Export Complete",
            &format!("Context at {:.1}%\nExported: {}\nAuto-processing enabled", context_pct, filename)
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

    // =========================================================================
    // CxProcessor Methods - Auto /cx processing
    // =========================================================================

    /// Scan exports-pending/ directory for files to process
    fn find_pending_exports(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.config.export_destination) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() {
                    // Process both .jsonl and .txt files (backward compatibility)
                    if let Some(ext) = path.extension() {
                        let ext_str = ext.to_string_lossy().to_lowercase();
                        if ext_str == "jsonl" || ext_str == "txt" {
                            files.push(path);
                        }
                    }
                }
            }
        }

        // Sort by modification time (oldest first)
        files.sort_by(|a, b| {
            let a_time = fs::metadata(a).and_then(|m| m.modified()).ok();
            let b_time = fs::metadata(b).and_then(|m| m.modified()).ok();
            a_time.cmp(&b_time)
        });

        files
    }

    /// Call Python extractor script for a single file
    fn call_python_extractor(&self, file: &Path) -> Result<CxFileResult, Box<dyn std::error::Error + Send + Sync>> {
        let filename = file.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Check if extractor script exists
        if !self.config.python_extractor_path.exists() {
            return Ok(CxFileResult {
                filename,
                messages_new: 0,
                messages_duplicate: 0,
                success: false,
                error: Some("Python extractor script not found".to_string()),
            });
        }

        // Determine file type flag based on extension
        let file_type_flag = if file.extension().map(|e| e == "jsonl").unwrap_or(false) {
            "--jsonl"
        } else {
            "--export"
        };

        // Run the Python extractor
        let output = Command::new("python3")
            .arg(&self.config.python_extractor_path)
            .arg(file_type_flag)
            .arg(file)
            .arg("--no-archive")  // We handle archiving ourselves
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Ok(CxFileResult {
                filename,
                messages_new: 0,
                messages_duplicate: 0,
                success: false,
                error: Some(format!("Extractor failed: {}", stderr.trim())),
            });
        }

        // Parse output to extract message counts
        // Look for patterns like "→ 123 new / 456 total"
        let mut messages_new = 0u64;
        let mut messages_duplicate = 0u64;

        for line in stdout.lines() {
            if line.contains("new /") {
                // Parse "→ 123 new / 456 total"
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if *part == "new" && i > 0 {
                        if let Ok(n) = parts[i - 1].trim_start_matches('→').trim().parse::<u64>() {
                            messages_new = n;
                        }
                    }
                    if *part == "total" && i > 0 {
                        if let Ok(n) = parts[i - 1].trim_start_matches('/').trim().parse::<u64>() {
                            messages_duplicate = n.saturating_sub(messages_new);
                        }
                    }
                }
            }
        }

        Ok(CxFileResult {
            filename,
            messages_new,
            messages_duplicate,
            success: true,
            error: None,
        })
    }

    /// Move processed file to archive directory
    fn move_to_archive(&self, file: &Path) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        let filename = file.file_name().ok_or("No filename")?;
        let archive_path = self.config.export_archive.join(filename);

        // Handle name collision
        let final_path = if archive_path.exists() {
            let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
            let ext = file.extension().and_then(|s| s.to_str()).unwrap_or("jsonl");
            let timestamp = Utc::now().format("%H%M%S").to_string();
            self.config.export_archive.join(format!("{}-{}.{}", stem, timestamp, ext))
        } else {
            archive_path
        };

        fs::rename(file, &final_path)?;

        Ok(final_path)
    }

    /// Generate processing report and write to reports directory
    fn generate_report(&self, report: &CxProcessingReport) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        let report_filename = format!("{}.jsonl", report.run_id);
        let report_path = self.config.cx_reports_dir.join(&report_filename);

        let json = serde_json::to_string(report)?;
        let mut file = File::create(&report_path)?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;

        Ok(report_path)
    }

    /// Update session log with processing results
    fn update_session_log(&self, report: &CxProcessingReport) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Get today's date for session log filename
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let session_log_path = self.config.session_logs_dir.join(format!("SESSION-LOG-{}.md", today));

        // Create session log entry as JSONL
        let entry = serde_json::json!({
            "event": "cx_processing",
            "timestamp": report.timestamp,
            "files": report.files_processed,
            "messages": report.messages_new,
            "run_id": report.run_id,
            "machine_id": self.machine_id,
        });

        // Append to session log (create if doesn't exist)
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session_log_path)?;

        // If file is new or empty, add header
        let metadata = file.metadata()?;
        if metadata.len() == 0 {
            let header = format!(
                "---\ntitle: \"Session Log - {}\"\nmachine_uuid: \"{}\"\ndate: \"{}\"\n---\n\n# Session Log - {}\n\n## Auto-CX Processing Events\n\n",
                today,
                self.machine_id.as_deref().unwrap_or("unknown"),
                today,
                today
            );
            file.write_all(header.as_bytes())?;
        }

        // Append the entry as a markdown code block with JSON
        let log_entry = format!(
            "### {}\n```json\n{}\n```\n\n",
            Utc::now().format("%H:%M:%S UTC"),
            serde_json::to_string_pretty(&entry)?
        );
        file.write_all(log_entry.as_bytes())?;

        Ok(())
    }

    /// Process all pending exports (auto /cx)
    ///
    /// This method:
    /// 1. Scans exports-pending/ for .jsonl and .txt files
    /// 2. Calls Python unified-message-extractor.py for each file
    /// 3. Moves processed files to exports-archive/
    /// 4. Generates a processing report in cx-processing-reports/
    /// 5. Updates the session log with results
    pub fn process_pending_exports(&mut self) -> Result<Option<CxProcessingReport>, Box<dyn std::error::Error + Send + Sync>> {
        let pending_files = self.find_pending_exports();

        if pending_files.is_empty() {
            return Ok(None);
        }

        let start_time = Instant::now();
        let run_id = format!("cx-{}", Utc::now().format("%Y%m%d-%H%M%S"));
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        tracing::info!(
            "[context-watcher] processing {} pending export(s)",
            pending_files.len()
        );

        let mut file_results = Vec::new();
        let mut total_new = 0u64;
        let mut total_duplicate = 0u64;
        let mut errors = 0u32;

        for file in &pending_files {
            tracing::debug!("[context-watcher] processing: {}", file.display());

            match self.call_python_extractor(file) {
                Ok(result) => {
                    if result.success {
                        total_new += result.messages_new;
                        total_duplicate += result.messages_duplicate;

                        // Move to archive
                        match self.move_to_archive(file) {
                            Ok(archive_path) => {
                                tracing::debug!(
                                    "[context-watcher] archived {} -> {}",
                                    file.display(),
                                    archive_path.display()
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "[context-watcher] failed to archive {}: {}",
                                    file.display(),
                                    e
                                );
                            }
                        }
                    } else {
                        errors += 1;
                        tracing::warn!(
                            "[context-watcher] extractor failed for {}: {:?}",
                            file.display(),
                            result.error
                        );
                    }
                    file_results.push(result);
                }
                Err(e) => {
                    errors += 1;
                    let filename = file.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    file_results.push(CxFileResult {
                        filename,
                        messages_new: 0,
                        messages_duplicate: 0,
                        success: false,
                        error: Some(e.to_string()),
                    });
                    tracing::error!("[context-watcher] processing error for {}: {}", file.display(), e);
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;

        let report = CxProcessingReport {
            timestamp,
            run_id,
            files_processed: pending_files.len() as u32,
            messages_new: total_new,
            messages_duplicate: total_duplicate,
            errors,
            duration_ms,
            file_results,
        };

        // Generate report file
        if let Err(e) = self.generate_report(&report) {
            tracing::warn!("[context-watcher] failed to generate report: {}", e);
        }

        // Update session log
        if let Err(e) = self.update_session_log(&report) {
            tracing::warn!("[context-watcher] failed to update session log: {}", e);
        }

        // Update state
        self.state.last_cx_processing = Some(Utc::now());
        self.state.cx_runs_total += 1;
        let _ = self.save_state();

        // Log summary
        tracing::info!(
            "[context-watcher] cx complete: {} files, {} new messages, {} duplicates, {} errors, {}ms",
            report.files_processed,
            report.messages_new,
            report.messages_duplicate,
            report.errors,
            report.duration_ms
        );

        Ok(Some(report))
    }

    // =========================================================================
    // Process Detection
    // =========================================================================

    /// Detect running Claude processes and update state
    fn update_active_processes(&mut self) {
        let processes = ProcessDetector::find_claude_processes(&self.config.claude_projects_dir);
        let count = processes.len();

        if count > 0 {
            tracing::info!(
                "[context-watcher] {} Claude process(es) detected: {:?}",
                count,
                processes.iter().map(|p| p.pid).collect::<Vec<_>>()
            );
        }

        self.state.active_processes = processes;
        self.state.active_process_count = count as u32;
        self.last_process_check = Instant::now();
    }

    // =========================================================================
    // Main Run Loop
    // =========================================================================

    /// Run the context watcher (event-driven)
    pub async fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("[context-watcher] starting");
        tracing::info!("[context-watcher] watching: {}", self.config.claude_projects_dir.display());
        tracing::info!("[context-watcher] threshold: {}%", self.config.min_context_percent);
        tracing::info!("[context-watcher] auto-cx interval: {}s", self.config.cx_processing_interval_secs);

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
                    // Check all project directories for context threshold
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

                    // Process any pending exports (auto /cx) at the configured interval
                    let elapsed = self.last_cx_check.elapsed();
                    if elapsed.as_secs() >= self.config.cx_processing_interval_secs {
                        self.last_cx_check = Instant::now();

                        if let Err(e) = self.process_pending_exports() {
                            tracing::error!("[context-watcher] cx processing error: {e}");
                        }
                    }

                    // Periodic process detection (every 30 seconds)
                    if self.last_process_check.elapsed() > self.process_check_interval {
                        self.update_active_processes();
                        // Save state to persist active processes
                        let _ = self.save_state();
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
            session_cooldowns: HashMap::new(),
            last_export: Some(Utc::now()),
            last_session_file: Some(PathBuf::from("/test/session.jsonl")),
            last_tokens: 150_000,
            last_context_percent: 75.0,
            exports_triggered: 5,
            last_cx_processing: None,
            cx_runs_total: 0,
            active_processes: Vec::new(),
            active_process_count: 0,
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: WatcherState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.last_tokens, 150_000);
        assert_eq!(restored.exports_triggered, 5);
        assert_eq!(restored.active_process_count, 0);
    }

    #[test]
    fn test_cx_processing_report_serialization() {
        let report = CxProcessingReport {
            timestamp: "2026-01-11T16:30:45Z".to_string(),
            run_id: "cx-20260111-163045".to_string(),
            files_processed: 4,
            messages_new: 4892,
            messages_duplicate: 224,
            errors: 0,
            duration_ms: 3450,
            file_results: vec![
                CxFileResult {
                    filename: "test-export.jsonl".to_string(),
                    messages_new: 4892,
                    messages_duplicate: 224,
                    success: true,
                    error: None,
                }
            ],
        };

        let json = serde_json::to_string(&report).unwrap();
        let restored: CxProcessingReport = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.files_processed, 4);
        assert_eq!(restored.messages_new, 4892);
        assert_eq!(restored.duration_ms, 3450);
        assert_eq!(restored.file_results.len(), 1);
        assert!(restored.file_results[0].success);
    }

    #[test]
    fn test_cx_file_result_error_case() {
        let result = CxFileResult {
            filename: "bad-file.txt".to_string(),
            messages_new: 0,
            messages_duplicate: 0,
            success: false,
            error: Some("File not found".to_string()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let restored: CxFileResult = serde_json::from_str(&json).unwrap();

        assert!(!restored.success);
        assert_eq!(restored.error, Some("File not found".to_string()));
    }
}
