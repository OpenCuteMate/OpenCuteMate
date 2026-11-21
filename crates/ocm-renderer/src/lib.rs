//! # ocm-renderer
//!
//! Rendering backends for OpenCuteMate, behind one small contract
//! ([`Renderer`]). A backend consumes platform-neutral
//! [`ocm_platform::NativeHandles`] (raw-window-handle enums) plus sizes; it
//! never touches a Wayland type.
//!
//! Two implementations live behind cargo features:
//!
//! * `wgpu-backend` (default) — the validated Phase 2 renderer, now
//!   drawing arbitrary box-part character models with a soft drop shadow
//!   over a fully transparent background, wgpu-direct.
//! * `bevy-backend` (default) — the Phase 2b Bevy 0.20 integration driving
//!   Bevy's render app against the same native surface (see
//!   `docs/bevy-integration.md`).
//!
//! Both share the placement math in [`math`] / [`scene`], so hitboxes and
//! on-screen placement are identical between backends.
//!
//! Since Phase 3a the backends do not hardcode the cube: they render the
//! [`ocm_character::CharacterModel`] they were constructed with (one
//! painter's-order list of box parts) and apply the fully resolved
//! [`ocm_character::AnimatedPose`] per frame. Hitboxes are per semantic
//! element, computed from the same camera math — the single source of
//! truth for what is where on screen.

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

pub use ocm_character::{AnimatedPose, ElementId};

/// Renderer construction options.
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    /// `true` = Fifo (vsync), `false` = best-effort no-vsync.
    pub vsync: bool,
    /// Allow configuring an **Opaque** surface when the backend offers no
    /// compositing alpha mode. Win32 DX12 HWND surfaces report only
    /// `Opaque`; the runtime then applies a window-system color key and
    /// the renderer clears to `opaque_clear_color` (skipping the soft
    /// shadow, which cannot survive binary keying). Wayland leaves this
    /// `false`: an opaque layer-shell surface is an error there.
    pub allow_opaque_fallback: bool,
    /// Clear color used when the negotiated surface is Opaque (the color
    /// key the platform makes transparent). Linear RGBA.
    pub opaque_clear_color: [f32; 4],
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            vsync: true,
            allow_opaque_fallback: false,
            opaque_clear_color: [1.0, 0.0, 1.0, 1.0],
        }
    }
}

/// A snapshot of the character state to render.
#[derive(Debug, Clone)]
pub struct SceneState {
    /// Normalized position [0,1] x [0,1], top-left origin.
    pub position: (f64, f64),
    /// Character height as a fraction of the logical output height.
    pub size_frac: f32,
    /// The element under the pointer, for the hover tint (`None` = no
    /// hover). Pose-driven visuals come from `pose`.
    pub hover: Option<ElementId>,
    /// Fully resolved animation state for this frame.
    pub pose: AnimatedPose,
}

impl SceneState {
    /// A static scene: neutral pose, nothing hovered.
    pub fn idle(position: (f64, f64), size_frac: f32) -> Self {
        Self {
            position,
            size_frac,
            hover: None,
            pose: AnimatedPose::neutral(0),
        }
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
    fn idle_scene_is_pose_neutral() {
        let scene = SceneState::idle((0.5, 0.5), 0.4);
        assert_eq!(scene.hover, None);
        assert_eq!(scene.pose.root, ocm_character::RootPose::NEUTRAL);
    }
}
