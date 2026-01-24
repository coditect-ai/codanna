//! CODI2 Reference Code (Forked)
//!
//! This module contains reference implementations from the CODI2 project
//! that serve as architectural guidance for the context watcher.
//!
//! Original source: coditect-labs-v4-archive/codi2/src/monitor/
//!
//! The original CODI2 code is preserved unchanged in the archive.
//! This fork extracts key patterns for reuse in codanna.

mod file_monitor_ref;
mod export_handler_ref;

pub use file_monitor_ref::*;
pub use export_handler_ref::*;
