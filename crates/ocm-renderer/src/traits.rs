//! The renderer contract shared by all backends.
//!
//! One page, deliberately: a backend renders [`SceneState`] snapshots to
//! the native surface it was constructed from, reports what happened, and
//! answers the same hitbox question the input region needs. Construction
//! is backend-specific (feature-gated concrete types), so it is not part
//! of the trait.

use crate::{FrameOutcome, Hitbox, RendererError, SceneState};

/// A rendering backend rendering into the platform-owned native surface.
///
/// # Lifetime contract
/// The native display/window objects behind the handles passed to the
/// backend constructor must outlive the renderer (the GPU surface holds
/// raw pointers into them). The runtime enforces this by dropping the
/// renderer before the platform session (see docs/architecture.md §6).
pub trait Renderer {
    /// Resize to new logical/buffer dimensions.
    fn resize(&mut self, logical: (u32, u32), buffer: (u32, u32));

    /// Render one frame from a scene snapshot.
    fn render(&mut self, scene: &SceneState) -> Result<FrameOutcome, RendererError>;

    /// Character hitbox in logical surface pixels for a given scene.
    /// Must use the same placement math as `render`, so input regions
    /// always match what is drawn.
    fn character_hitbox(&self, scene: &SceneState) -> Hitbox;
}
