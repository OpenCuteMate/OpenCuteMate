//! Desktop environment abstraction.
//!
//! The only capability the current platform stack can support **reliably**
//! is the geometry of the output the overlay lives on: its logical size.
//! Everything a Wayland layer-shell client can know about the desktop is
//! derivable from that (floor, walls, per-output placement via the
//! existing output selection in `ocm-platform`).
//!
//! ## What is deliberately NOT here
//!
//! A plain layer-shell client cannot see other windows: there is no
//! standard Wayland protocol exposing foreign-window geometry, stacking
//! or focus to arbitrary clients. `zwlr_layer_shell` has no window-list
//! request; ext-workspace/foreign-toplevel protocols cover *toplevel*
//! windows but require the compositor to advertise them and do not give
//! reliable pixel geometry to overlay clients — and Niri does not expose
//! them to layer-shell clients today. OpenCuteMate therefore does **not**
//! pretend to know window positions: no fake window detection, no
//! per-window collision. Future desktop interactions that need window
//! knowledge (walking behind windows, reacting to focus changes) will
//! require compositor-specific protocols or helper integration and must
//! be added here as explicitly optional capabilities, never as
//! assumptions baked into the character crate.

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
