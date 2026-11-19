//! [`WaylandOverlay`]: the `OverlaySurface` implementation handed to the
//! renderer/runtime. It is a cheap `Rc` handle into the shared Wayland state
//! owned by the platform/event source.

use std::rc::Rc;

use ocm_platform::{NativeHandles, OverlaySurface, Rect};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use std::cell::RefCell;
use std::ptr::NonNull;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::Proxy;

use crate::state::Shared;

/// A live Wayland layer-shell overlay surface.
///
/// Cheaply cloneable; all clones share the same underlying state (which the
/// `WaylandSource` in the event loop owns alongside this handle).
#[derive(Clone)]
pub struct WaylandOverlay {
    pub(crate) shared: Rc<RefCell<Shared>>,
}

impl WaylandOverlay {
    /// The name of the output this overlay lives on (the same id the
    /// desktop environment providers key on).
    pub fn target_output(&self) -> Option<String> {
        self.shared.borrow().target_output.clone()
    }
}

impl OverlaySurface for WaylandOverlay {
    fn native_handles(&self) -> NativeHandles {
        let s = self.shared.borrow();
        let conn = &s.conn;
        let surface = s.surface.as_ref().expect("surface destroyed prematurely");

        // SAFETY (documented lifetime contract, see docs/architecture.md §6):
        //
        // 1. The `wl_display` pointer comes from the live `Connection` stored
        //    in `Shared`; the `wl_surface` pointer comes from a live proxy in
        //    the same `Shared`. Both objects outlive any GPU surface created
        //    from these handles because the runtime drops the renderer (and
        //    thus the wgpu Surface) *before* dropping the platform/session
        //    that owns `Shared` (enforced by the explicit teardown order in
        //    `ocm-runtime::main`).
        // 2. The pointers are only *copied* here; they are dereferenced by
        //    the graphics stack (Vulkan WSI) while the owning objects are
        //    alive.
        // 3. The `wl_surface` is never destroyed while the renderer exists
        //    (destruction happens only when the last `Rc<Shared>` drops,
        //    which is after the renderer drop in the teardown sequence).
        let display_ptr = conn.backend().display_ptr();
        let surface_ptr = surface.id().as_ptr();
        let display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(display_ptr.cast::<std::ffi::c_void>())
                .expect("wl_display pointer is non-null"),
        ));
        let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr.cast::<std::ffi::c_void>())
                .expect("wl_surface pointer is non-null"),
        ));
        NativeHandles { display, window }
    }

    fn logical_size(&self) -> (u32, u32) {
        self.shared.borrow().logical_size.unwrap_or((1, 1))
    }

    fn buffer_size(&self) -> (u32, u32) {
        let s = self.shared.borrow();
        let logical = s.logical_size.unwrap_or((1, 1));
        s.buffer_size_for(logical)
    }

    fn scale_factor(&self) -> f64 {
        self.shared.borrow().effective_scale()
    }

    fn set_input_region(&self, rects: Option<&[Rect]>) {
        let s = self.shared.borrow();
        let Some(surface) = s.surface.clone() else {
            return;
        };
        match rects {
            // None = infinite input region: the whole surface is interactive.
            None => surface.set_input_region(None::<&WlRegion>),
            Some(rects) => {
                let Some(compositor) = s.compositor.clone() else {
                    return;
                };
                let region = compositor.create_region(&s.qh, ());
                for rect in rects {
                    region.add(rect.x, rect.y, rect.width, rect.height);
                }
                // The protocol gives set_*_region copy semantics, so the
                // region may be destroyed immediately after being set.
                surface.set_input_region(Some(&region));
                region.destroy();
            }
        }
        // Input region is double-buffered state: commit (and flush) right
        // away so a drag can never outrun its hitbox.
        surface.commit();
        if let Err(e) = s.conn.flush() {
            tracing::debug!("flush after set_input_region: {e}");
        }
    }

    fn request_frame(&self) -> bool {
        let mut s = self.shared.borrow_mut();
        if s.frame_armed {
            return false;
        }
        let Some(surface) = s.surface.clone() else {
            return false;
        };
        // The callback latches onto the next commit (the present that
        // follows the next render). User data is `()`: the shared
        // Dispatch<WlCallback, ()> implementation emits Frame events.
        surface.frame(&s.qh, ());
        s.frame_armed = true;
        if let Err(e) = s.conn.flush() {
            tracing::debug!("flush after frame request: {e}");
        }
        true
    }

    fn commit(&self) {
        let s = self.shared.borrow();
        if let Some(surface) = s.surface.clone() {
            surface.commit();
            if let Err(e) = s.conn.flush() {
                tracing::debug!("flush after commit: {e}");
            }
        }
    }
}
