//! Desktop environment abstraction.
//!
//! The character crate's own view of the desktop: the logical bounds of
//! the output it lives on (top-left origin, logical pixels). Physics,
//! drag clamping and floor/wall logic are all stated in this
//! output-local frame; the runtime derives it from the layer-shell
//! configure and reconciles it with the neutral desktop environment's
//! work area when the compositor reports one (see the Phase 3b
//! `ocm-desktop` / `ocm-desktop-niri` crates and
//! `docs/desktop-awareness.md`).
//!
//! ## What is deliberately NOT here
//!
//! Everything environmental that the compositor must provide: window
//! geometry, focus, work areas, multi-output layout. That knowledge
//! lives in the neutral `ocm-desktop` model and arrives through its
//! providers — the character never talks to a compositor. On Niri,
//! window pixel geometry exists only for floating windows and
//! minimized/maximized/fullscreen are not exposed at all; such gaps are
//! represented as absent data, never guesses. Future desktop
//! interactions that consume windows (walking on them, reacting to
//! focus changes) will read the neutral environment through the runtime,
//! never compositor APIs.

/// The currently knowable desktop geometry: the logical bounds of the
/// output the character lives on (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopBounds {
    /// Logical output width in px.
    pub width: u32,
    /// Logical output height in px.
    pub height: u32,
}

impl DesktopBounds {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn center(&self) -> (f64, f64) {
        (self.width as f64 / 2.0, self.height as f64 / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_never_collapse_to_zero() {
        let b = DesktopBounds::new(0, 0);
        assert_eq!(b, DesktopBounds::new(1, 1));
    }

    #[test]
    fn center_is_half_the_extent() {
        let b = DesktopBounds::new(1536, 864);
        assert_eq!(b.center(), (768.0, 432.0));
    }
}
