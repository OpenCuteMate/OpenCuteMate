//! # ocm-renderer
//!
//! Rendering backends for OpenCuteMate, behind one small contract
//! ([`Renderer`]). A backend consumes platform-neutral
//! [`ocm_platform::NativeHandles`] (raw-window-handle enums) plus sizes; it
//! never touches a Wayland type.
//!
//! Two implementations live behind cargo features:
//!
//! * `wgpu-backend` (default) — the validated Phase 2 renderer: a
//!   procedural cube with a soft drop shadow over a fully transparent
//!   background, wgpu-direct.
//! * `bevy-backend` (default) — the Phase 2b Bevy 0.20 integration driving
//!   Bevy's render app against the same native surface (see
//!   `docs/bevy-integration.md`).
//!
//! Both share the placement math in [`math`] / [`scene`], so hitboxes and
//! on-screen placement are identical between backends.

pub mod math;
pub mod scene;

#[cfg(feature = "bevy-backend")]
pub mod bevy;
#[cfg(feature = "wgpu-backend")]
pub mod wgpu;

mod traits;

pub use traits::Renderer;

#[cfg(feature = "wgpu-backend")]
pub use wgpu::WgpuRenderer;

#[cfg(feature = "bevy-backend")]
pub use bevy::BevyRenderer;

/// Renderer construction options.
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    /// `true` = Fifo (vsync), `false` = best-effort no-vsync.
    pub vsync: bool,
}

/// A snapshot of the character state to render.
#[derive(Debug, Clone, Copy)]
pub struct SceneState {
    /// Normalized position [0,1] x [0,1], top-left origin.
    pub position: (f64, f64),
    /// Character height as a fraction of the logical output height.
    pub size_frac: f32,
    /// Pointer hover highlight.
    pub hover: bool,
    /// Continuous animation (bob + slow spin).
    pub animate: bool,
    /// Seconds since start (drives animation).
    pub time: f32,
}

/// Hitbox of the character in logical surface pixels.
#[derive(Debug, Clone, Copy)]
pub struct Hitbox {
    pub center_x: f32,
    pub center_y: f32,
    pub radius: f32,
}

impl Hitbox {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let dx = x as f32 - self.center_x;
        let dy = y as f32 - self.center_y;
        dx * dx + dy * dy <= self.radius * self.radius
    }
}

/// Result of one `render` call.
#[derive(Debug, Clone, Copy)]
pub struct FrameOutcome {
    /// Whether a frame was actually acquired, encoded and presented.
    pub presented: bool,
    /// Reason the frame was skipped (the scheduler should retry later).
    pub skipped: Option<&'static str>,
}

#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("failed to create the GPU surface: {0}")]
    SurfaceCreation(String),
    #[error("no GPU adapter compatible with the surface was found: {0}")]
    Adapter(String),
    #[error("failed to acquire a GPU device: {0}")]
    Device(String),
    #[error("no surface format with an alpha channel is supported (transparency impossible): available = {0}")]
    NoAlphaFormat(String),
    #[error("no alpha-compositing mode that supports transparency is available: {0}")]
    NoAlphaMode(String),
    #[error("the GPU surface was lost: {0}")]
    SurfaceLost(String),
    #[error("render failed: {0}")]
    Render(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hitbox_contains_is_inclusive_at_the_boundary() {
        let h = Hitbox {
            center_x: 100.0,
            center_y: 100.0,
            radius: 40.0,
        };
        assert!(h.contains(100.0, 100.0));
        assert!(h.contains(140.0, 100.0));
        assert!(h.contains(
            100.0 + 40.0 * std::f64::consts::FRAC_1_SQRT_2,
            100.0 + 40.0 * std::f64::consts::FRAC_1_SQRT_2
        ));
        assert!(!h.contains(140.01, 100.0));
        assert!(!h.contains(0.0, 0.0));
    }
}
