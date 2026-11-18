//! The renderer contract shared by all backends.
//!
//! One page, deliberately: a backend renders [`SceneState`] snapshots to
//! the native surface it was constructed from, reports what happened, and
//! answers the same hitbox question the input region needs. Construction
//! is backend-specific (feature-gated concrete types), so it is not part
//! of the trait.

use crate::{FrameOutcome, RendererError, SceneState};
use ocm_character::ElementHitbox;

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

    /// Per-element hitboxes in logical surface pixels for a given scene,
    /// in hit-priority order (children before parents — see
    /// `ocm_character::model`).
    ///
    /// Must use the same placement math as `render`, so the compositor
    /// input region always matches what is drawn. Hitboxes are
    /// **pose-independent** by design: the animation pose (bob, squash)
    /// must never move the input region, or every animated frame would
    /// commit a compositor state update.
    fn character_hitboxes(&self, scene: &SceneState) -> Vec<ElementHitbox>;
}
