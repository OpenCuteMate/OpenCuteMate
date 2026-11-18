//! Logging initialization for all OpenCuteMate binaries.
//!
//! `RUST_LOG` takes precedence; otherwise the given default level is used.

use tracing_subscriber::EnvFilter;

/// Initialize the global `tracing` subscriber.
///
/// Honors `RUST_LOG` (e.g. `RUST_LOG=ocm_runtime=debug,info`); when unset,
/// uses `default_level` (e.g. `"info"`).
pub fn init(default_level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level.to_string()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .init();
}
