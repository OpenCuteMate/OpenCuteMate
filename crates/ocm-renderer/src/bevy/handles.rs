//! Bridge from `ocm_platform::NativeHandles` to Bevy's window-handle
//! machinery.
//!
//! Bevy's render app creates GPU surfaces from any entity carrying
//! `bevy_window::RawHandleWrapper`; the wrapper is built from any type
//! implementing the `raw_window_handle` `Has*` traits
//! (`bevy_window-0.20.1/src/raw_handle.rs:64-74`). This module implements
//! those traits over our platform-neutral `NativeHandles` — the renderer
//! therefore still never names a Wayland type.

use bevy::window::WindowWrapper;
use ocm_platform::NativeHandles;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};

/// `NativeHandles` exposed through the `raw_window_handle` traits, plus the
/// `Send + Sync` bound Bevy's `WindowWrapper<W>` requires.
///
/// SAFETY (the `unsafe impl`s below): the raw pointers inside are only
/// *copied* into wgpu surface creation and dereferenced by the Vulkan WSI
/// while the owning objects (the platform's shared Wayland state) are alive.
/// The runtime drops this renderer — and with it Bevy's wgpu surface —
/// strictly before the platform session (docs/architecture.md §6), and
/// rendering is non-pipelined, so all handle use happens on the main
/// thread. This is the same contract Bevy itself relies on for
/// `RawHandleWrapper` (`bevy_window-0.20.1/src/raw_handle.rs:122-129`) and
/// winit for `SendSyncRawWindowHandle` (`winit-0.30.13/src/window.rs:174`).
#[derive(Clone, Copy)]
pub struct NativeSurface {
    handles: NativeHandles,
}

unsafe impl Send for NativeSurface {}
unsafe impl Sync for NativeSurface {}

impl NativeSurface {
    pub fn new(handles: &NativeHandles) -> Self {
        Self { handles: *handles }
    }

    /// Build Bevy's window-handle wrapper for this surface.
    ///
    /// The returned wrapper stores an `Arc` clone of the `NativeSurface`
    /// (Bevy keeps it alive for pipelined frames; with non-pipelined
    /// rendering our teardown order governs regardless — see the type's
    /// SAFETY comment).
    pub fn raw_handle_wrapper(&self) -> Result<bevy::window::RawHandleWrapper, HandleError> {
        bevy::window::RawHandleWrapper::new(&WindowWrapper::new(*self))
    }
}

impl HasWindowHandle for NativeSurface {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: the handle points at the live `wl_surface` owned by the
        // platform's shared state, which outlives this renderer (teardown
        // order, docs/architecture.md §6).
        unsafe { Ok(WindowHandle::borrow_raw(self.handles.window)) }
    }
}

impl HasDisplayHandle for NativeSurface {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: the handle points at the live `wl_display` owned by the
        // platform's shared state; same lifetime contract as above.
        unsafe { Ok(DisplayHandle::borrow_raw(self.handles.display)) }
    }
}
