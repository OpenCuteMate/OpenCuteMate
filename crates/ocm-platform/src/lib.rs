//! # ocm-platform
//!
//! Platform-agnostic abstractions shared by platform backends
//! (e.g. `ocm-platform-wayland`) and consumers like `ocm-renderer`
//! and `ocm-runtime`.
//!
//! Design rule: nothing in this crate may expose a concrete windowing stack.
//! The only windowing vocabulary here is [`raw_window_handle`]'s
//! platform-neutral enums — the same abstraction wgpu consumes — so a
//! renderer can create a GPU surface without knowing what a `wl_surface` is,
//! and a future Windows/macOS backend can provide the same contract.

use ocm_core::{KeyboardInteractivity, Layer};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

/// Raw native handles for GPU surface creation.
///
/// On Wayland these carry `wl_display`/`wl_surface` pointers inside the
/// `Wayland` variants of the raw-window-handle enums; the renderer never
/// matches on the variants, so the same struct works on every platform.
///
/// # Safety contract (enforced by the runtime, not the type system)
/// The underlying native objects must outlive any GPU surface created from
/// these handles. See `docs/architecture.md` §6 for the teardown order that
/// guarantees this.
#[derive(Debug, Clone, Copy)]
pub struct NativeHandles {
    pub display: RawDisplayHandle,
    pub window: RawWindowHandle,
}

/// An integer rectangle in logical surface coordinates (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Pointer buttons, following Linux input-event-codes naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
    Other(u32),
}

impl PointerButton {
    /// Map a `wl_pointer` button code (Linux BTN_* constants) to a button.
    pub fn from_linux_code(code: u32) -> Self {
        match code {
            0x110 => Self::Left,
            0x111 => Self::Right,
            0x112 => Self::Middle,
            other => Self::Other(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEventKind {
    Enter,
    Leave,
    Motion,
    Press { button: PointerButton },
    Release { button: PointerButton },
}

/// A pointer event with the current pointer position in logical surface
/// coordinates (top-left origin), regardless of kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub kind: PointerEventKind,
    pub x: f64,
    pub y: f64,
}

/// Events a platform backend reports to the application.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlatformEvent {
    Pointer(PointerEvent),
    /// The overlay's logical size changed (e.g. layer-shell configure).
    Configure {
        width: u32,
        height: u32,
    },
    /// The compositor's preferred scale for the surface changed
    /// (1.0, 1.25, 2.0, ...).
    ScaleChanged {
        scale: f64,
    },
    /// A frame callback fired: the compositor is ready for the next frame.
    Frame,
    /// The compositor closed the surface; the app should exit.
    Closed,
}

/// Static information about one monitor/output.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub name: String,
    pub description: String,
    /// Logical position of this output in the global layout, if known.
    pub logical_position: Option<(i32, i32)>,
    /// Logical size, if known (via xdg-output).
    pub logical_size: Option<(u32, u32)>,
    /// Integer scale factor reported by `wl_output`.
    pub scale: i32,
}

/// Parameters describing the overlay surface to create.
#[derive(Debug, Clone)]
pub struct OverlayParams {
    pub layer: Layer,
    pub keyboard_interactivity: KeyboardInteractivity,
    /// Output name to target; empty = first advertised output.
    pub output: String,
    /// Layer-shell namespace identifier (for compositor rules/debugging).
    pub namespace: String,
}

/// A live overlay surface.
///
/// All coordinates are **logical** surface coordinates; use
/// [`OverlaySurface::buffer_size`] for device-pixel sizing of GPU back
/// buffers (logical size × effective scale).
pub trait OverlaySurface {
    /// Raw handles for GPU surface creation (platform-neutral).
    fn native_handles(&self) -> NativeHandles;
    /// Logical size of the surface.
    fn logical_size(&self) -> (u32, u32);
    /// Buffer size in device pixels (logical × effective scale, rounded up).
    fn buffer_size(&self) -> (u32, u32);
    /// Effective scale factor (1.0, 1.25, 2.0, ...).
    fn scale_factor(&self) -> f64;

    /// Set the pointer input region.
    ///
    /// * `None` — the whole surface is interactive (click-through disabled).
    /// * `Some(rects)` — only the given rects receive input; clicks
    ///   elsewhere pass through to surfaces below.
    /// * `Some(&[])` — no input at all (fully click-through).
    ///
    /// Takes effect at the next commit; implementations must commit (and
    /// flush) so the change is applied immediately.
    fn set_input_region(&self, rects: Option<&[Rect]>);

    /// Arm a frame callback for the next compositor repaint opportunity.
    /// Returns `false` if one is already armed.
    fn request_frame(&self) -> bool;

    /// Commit the surface (applies pending double-buffered state).
    fn commit(&self);
}

/// A platform backend that can enumerate monitors and create overlays.
pub trait Platform: Sized {
    type Overlay: OverlaySurface;
    type Error: std::error::Error + 'static;

    /// Connect to the native display/session and enumerate basics.
    fn connect() -> Result<Self, Self::Error>;

    /// Monitors discovered at connect time.
    fn monitors(&self) -> Vec<MonitorInfo>;

    /// Create an overlay surface with the given parameters.
    fn create_overlay(&mut self, params: &OverlayParams) -> Result<Self::Overlay, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_linux_button_codes() {
        assert_eq!(PointerButton::from_linux_code(0x110), PointerButton::Left);
        assert_eq!(PointerButton::from_linux_code(0x111), PointerButton::Right);
        assert_eq!(PointerButton::from_linux_code(0x112), PointerButton::Middle);
        assert_eq!(
            PointerButton::from_linux_code(0x113),
            PointerButton::Other(0x113)
        );
    }
}
