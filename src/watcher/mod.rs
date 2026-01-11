//! Unified file watcher system for automatic re-indexing.
//!
//! This module provides a single file watcher that routes events to
//! pluggable handlers for code files, documents, and configuration.
//!
//! # Architecture
//!
//! ```text
//! UnifiedWatcher
//!   - Single notify::RecommendedWatcher
//!   - Shared PathRegistry (interned paths)
//!   - Shared Debouncer
//!   - Routes events to handlers
//!         |
//!    +---------+---------+---------+
//!    |         |         |         |
//! CodeHandler DocHandler ConfigHandler ContextHandler
//! ```
//!
//! # Context Watcher (CODI2-Inspired)
//!
//! The `context_watcher` module provides Claude Code session monitoring:
//! - Watches `~/.claude/projects/` for JSONL changes
//! - Parses token usage and calculates context percentage
//! - Triggers auto-export at configurable threshold (default: 75%)
//! - Sends desktop notifications and opens exports in editor
//!
//! See `codi_fork/` for reference implementations from CODI2.

mod debouncer;
mod error;
mod handler;
pub mod handlers;
mod hot_reload;
mod path_registry;
mod unified;

// Context watcher for Claude Code sessions
pub mod context_watcher;

// CODI2 reference implementations (forked)
pub mod codi_fork;

pub use debouncer::Debouncer;
pub use error::WatchError;
pub use handler::{WatchAction, WatchHandler};
pub use hot_reload::{HotReloadWatcher, IndexStats};
pub use path_registry::PathRegistry;
pub use unified::{UnifiedWatcher, UnifiedWatcherBuilder};

// Context watcher exports
pub use context_watcher::{ContextConfig, ContextWatcher, TokenUsage, WatcherState};
