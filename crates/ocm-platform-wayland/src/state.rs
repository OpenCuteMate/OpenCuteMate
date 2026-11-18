//! Internal shared state for the Wayland backend, plus every
//! [`wayland_client::Dispatch`] implementation.
//!
//! `Shared` holds the live protocol objects and is reference-counted so that
//! both the calloop event source (the dispatcher) and the
//! [`crate::WaylandOverlay`] handle (used by the renderer/runtime) can reach
//! it. Everything runs on one thread; there are no `Send`/`Sync` claims.

use std::cell::RefCell;
use std::rc::Rc;

use ocm_platform::{MonitorInfo, PlatformEvent, PointerButton, PointerEvent, PointerEventKind};
use wayland_client::protocol::{
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_output::WlOutput,
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1,
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

/// Per-output user data attached to `wl_output` / `zxdg_output_v1` proxies;
/// indexes into [`Shared::outputs`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputData {
    pub index: usize,
}

/// One discovered monitor.
#[derive(Clone)]
pub(crate) struct OutputRecord {
    pub proxy: WlOutput,
    pub monitor: MonitorInfo,
}

/// Live Wayland state, shared between the event source and the overlay handle.
pub(crate) struct Shared {
    pub conn: Connection,
    pub qh: QueueHandle<DispatchState>,
    pub compositor: Option<WlCompositor>,
    pub surface: Option<WlSurface>,
    pub layer_surface: Option<ZwlrLayerSurfaceV1>,
    pub seat: Option<WlSeat>,
    pub pointer: Option<WlPointer>,
    pub viewport: Option<WpViewport>,
    pub fractional: Option<WpFractionalScaleV1>,

    /// Logical size from the layer-shell configure.
    pub logical_size: Option<(u32, u32)>,
    /// The compositor closed the layer surface.
    pub closed: bool,
    /// Preferred scale in 1/120 units from `wp_fractional_scale_v1`
    /// (None when the compositor lacks the protocol).
    pub frac_scale: Option<u32>,
    /// Integer scale of the target output (`wl_output.scale`).
    pub output_scale: i32,
    /// All discovered outputs (index-matched with `OutputData`).
    pub outputs: Vec<OutputRecord>,
    /// Whether a frame callback is currently armed.
    pub frame_armed: bool,
    /// Events waiting to be emitted to the application.
    pub pending: Vec<PlatformEvent>,
    /// Last scale value reported via `PlatformEvent::ScaleChanged`.
    last_scale_event: f64,
    /// Last known pointer position (logical surface coords).
    pointer_pos: (f64, f64),
}

impl Shared {
    pub fn new(conn: Connection, qh: QueueHandle<DispatchState>) -> Self {
        Self {
            conn,
            qh,
            compositor: None,
            surface: None,
            layer_surface: None,
            seat: None,
            pointer: None,
            viewport: None,
            fractional: None,
            logical_size: None,
            closed: false,
            frac_scale: None,
            output_scale: 1,
            outputs: Vec::new(),
            frame_armed: false,
            pending: Vec::new(),
            last_scale_event: 1.0,
            pointer_pos: (0.0, 0.0),
        }
    }

    /// The effective scale factor for the surface.
    pub fn effective_scale(&self) -> f64 {
        match self.frac_scale {
            Some(frac) if frac > 0 => frac as f64 / 120.0,
            Some(_) => 1.0,
            None if self.output_scale > 0 => self.output_scale as f64,
            None => 1.0,
        }
    }

    /// Buffer (device pixel) size for a logical size at the effective scale.
    pub fn buffer_size_for(&self, logical: (u32, u32)) -> (u32, u32) {
        let scale = self.effective_scale();
        let is_integer = (scale - scale.round()).abs() < 1e-9;
        if is_integer {
            // Integer path: exact multiple via wl_surface buffer scale.
            let k = scale.round().max(1.0) as u32;
            (logical.0 * k, logical.1 * k)
        } else {
            // Fractional path: ceil; the viewport crops the remainder.
            (
                (logical.0 as f64 * scale).ceil() as u32,
                (logical.1 as f64 * scale).ceil() as u32,
            )
        }
    }

    /// Apply the current scale to the surface (buffer scale / viewport).
    /// Idempotent; safe to call whenever scale or logical size changes.
    pub fn apply_scale(&mut self) {
        let Some((w, h)) = self.logical_size else { return };
        let scale = self.effective_scale();
        if scale <= 0.0 {
            return;
        }
        let Some(surface) = self.surface.clone() else { return };
        let is_integer = (scale - scale.round()).abs() < 1e-9;
        if is_integer {
            let k = scale.round().max(1.0) as i32;
            surface.set_buffer_scale(k);
            if let Some(viewport) = self.viewport.take() {
                viewport.destroy();
            }
            tracing::debug!(k, "integer buffer scale applied");
        } else if let Some(viewport) = self.viewport.as_ref() {
            // Fractional: buffer is ceil(logical*scale) px, the viewport maps
            // the exact logical*scale source rectangle onto the logical size.
            surface.set_buffer_scale(1);
            viewport.set_source(0.0, 0.0, w as f64 * scale, h as f64 * scale);
            viewport.set_destination(w as i32, h as i32);
            tracing::debug!(scale, "fractional viewport applied ({w}x{h} logical)");
        } else {
            // Compositor lacks viewporter: cannot scale; render at logical
            // size and let the compositor upscale.
            tracing::warn!("fractional scale {scale} without viewporter support; buffers stay unscaled");
        }
    }

    /// Record a new effective scale; emit a ScaleChanged event when it moved.
    pub fn note_scale(&mut self) {
        let scale = self.effective_scale();
        if (scale - self.last_scale_event).abs() > 1e-9 {
            self.last_scale_event = scale;
            self.pending.push(PlatformEvent::ScaleChanged { scale });
        }
    }

    pub fn push_pointer(&mut self, kind: PointerEventKind) {
        let (x, y) = self.pointer_pos;
        self.pending.push(PlatformEvent::Pointer(PointerEvent { kind, x, y }));
    }
}

/// The `wayland-client` dispatch state: routes every protocol event into
/// [`Shared`].
///
/// Constructed in [`crate::platform::WaylandPlatform::connect`], where the
/// event queue (and its `QueueHandle`) exist.
pub(crate) struct DispatchState {
    pub shared: Rc<RefCell<Shared>>,
}

// ---------------------------------------------------------------------------
// Dispatch implementations
// ---------------------------------------------------------------------------

impl Dispatch<WlRegistry, wayland_client::globals::GlobalListContents> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Global add/remove is handled by the globals helper.
    }
}

impl Dispatch<WlCompositor, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WlCompositor,
        _event: <WlCompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegion, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegion,
        _event: <WlRegion as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        _event: <WlSurface as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // enter/leave for wl_surface itself arrive per-surface; we track
        // pointer presence through wl_pointer enter/leave instead.
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrLayerShellV1,
        _event: <ZwlrLayerShellV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputManagerV1, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &ZxdgOutputManagerV1,
        _event: <ZxdgOutputManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputV1, OutputData> for DispatchState {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: <ZxdgOutputV1 as Proxy>::Event,
        data: &OutputData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut shared = state.shared.borrow_mut();
        let Some(record) = shared.outputs.get_mut(data.index) else { return };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                record.monitor.logical_position = Some((x, y));
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                record.monitor.logical_size =
                    Some((width.max(0) as u32, height.max(0) as u32));
            }
            zxdg_output_v1::Event::Name { name } => {
                record.monitor.name = name;
            }
            zxdg_output_v1::Event::Description { description } => {
                record.monitor.description = description;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, OutputData> for DispatchState {
    fn event(
        state: &mut Self,
        _proxy: &WlOutput,
        event: <WlOutput as Proxy>::Event,
        data: &OutputData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_output;
        let mut shared = state.shared.borrow_mut();
        let Some(record) = shared.outputs.get_mut(data.index) else { return };
        match event {
            wl_output::Event::Scale { factor } => {
                record.monitor.scale = factor;
            }
            wl_output::Event::Name { name } => {
                record.monitor.name = name;
            }
            wl_output::Event::Description { description } => {
                record.monitor.description = description;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for DispatchState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut shared = state.shared.borrow_mut();
        match event {
            zwlr_layer_surface_v1::Event::Configure { width, height, serial } => {
                // Ack immediately: legal "sometime before the commit" that
                // responds to this configure (which our next present is).
                proxy.ack_configure(serial);
                let width = width.max(1);
                let height = height.max(1);
                tracing::info!(width, height, "layer surface configured");
                shared.logical_size = Some((width, height));
                shared.apply_scale();
                shared.pending.push(PlatformEvent::Configure { width, height });
            }
            zwlr_layer_surface_v1::Event::Closed => {
                tracing::info!("layer surface closed by compositor");
                shared.closed = true;
                shared.pending.push(PlatformEvent::Closed);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for DispatchState {
    fn event(
        state: &mut Self,
        proxy: &WlSeat,
        event: <WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Capabilities { capabilities } => {
                if let WEnum::Value(caps) = capabilities {
                    if caps.contains(wl_seat::Capability::Pointer) {
                        let mut shared = state.shared.borrow_mut();
                        if shared.pointer.is_none() {
                            tracing::debug!("seat has pointer capability; binding wl_pointer");
                            shared.pointer = Some(proxy.get_pointer(qh, ()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for DispatchState {
    fn event(
        state: &mut Self,
        _proxy: &WlPointer,
        event: <WlPointer as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut shared = state.shared.borrow_mut();
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } => {
                shared.pointer_pos = (surface_x, surface_y);
                tracing::debug!(x = surface_x, y = surface_y, "pointer enter");
                shared.push_pointer(PointerEventKind::Enter);
            }
            wl_pointer::Event::Leave { .. } => {
                tracing::debug!("pointer leave");
                shared.push_pointer(PointerEventKind::Leave);
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                shared.pointer_pos = (surface_x, surface_y);
                shared.push_pointer(PointerEventKind::Motion);
            }
            wl_pointer::Event::Button { button, state: press_state, .. } => {
                let button = PointerButton::from_linux_code(button);
                let kind = match press_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        PointerEventKind::Press { button }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        PointerEventKind::Release { button }
                    }
                    // ButtonState is non-exhaustive upstream; any other
                    // value (now or in the future) is ignored.
                    WEnum::Value(_) | WEnum::Unknown(_) => {
                        return;
                    }
                };
                tracing::debug!(?button, ?kind, "pointer button");
                shared.push_pointer(kind);
            }
            wl_pointer::Event::Frame { .. } => {
                // Batching boundary; our reactions are idempotent.
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for DispatchState {
    fn event(
        state: &mut Self,
        _proxy: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // We only create wl_callbacks via wl_surface.frame.
        let mut shared = state.shared.borrow_mut();
        shared.frame_armed = false;
        shared.pending.push(PlatformEvent::Frame);
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WpFractionalScaleManagerV1,
        _event: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for DispatchState {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: <WpFractionalScaleV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let mut shared = state.shared.borrow_mut();
            tracing::info!(scale, "fractional preferred scale (units of 1/120)");
            shared.frac_scale = Some(scale);
            shared.apply_scale();
            shared.note_scale();
        }
    }
}

impl Dispatch<WpViewporter, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewporter,
        _event: <WpViewporter as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for DispatchState {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewport,
        _event: <WpViewport as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
