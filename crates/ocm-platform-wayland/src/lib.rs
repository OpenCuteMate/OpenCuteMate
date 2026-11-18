//! # ocm-platform-wayland
//!
//! Native Wayland backend for OpenCuteMate:
//! connection, wlr-layer-shell integration, pointer input, output discovery,
//! fractional scaling and a calloop event source that turns Wayland events
//! into [`ocm_platform::PlatformEvent`]s.
//!
//! This crate must never depend on rendering (`wgpu`, `ocm-renderer`) or
//! model/animation logic.

mod overlay;
mod platform;
mod source;
mod state;

pub use overlay::WaylandOverlay;
pub use platform::WaylandPlatform;
pub use source::{SourceError, WaylandSource};
